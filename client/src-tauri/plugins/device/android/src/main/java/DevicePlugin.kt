// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.res.Configuration
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import android.webkit.WebView
import androidx.activity.result.ActivityResult
import androidx.appcompat.app.AppCompatActivity
import androidx.core.app.ActivityCompat
import androidx.lifecycle.Lifecycle
import app.tauri.PermissionState
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.util.concurrent.Executors

@InvokeArg
internal class EventArgs {
    lateinit var events: Channel
}

@TauriPlugin(
    permissions =
        [
            Permission(strings = [Manifest.permission.RECORD_AUDIO], alias = "microphone"),
            Permission(strings = [Manifest.permission.POST_NOTIFICATIONS], alias = "notifications"),
            Permission(strings = [Manifest.permission.CAMERA], alias = "camera"),
        ]
)
class DevicePlugin(first: Activity) : Plugin(first) {
    // Tauri keeps this plugin for the whole process, while Android can replace
    // the screen under it: after a configuration change it cannot absorb, such
    // as the first moments after the phone starts, or when the app reopens
    // while the alert service kept the process. Window work follows the newest
    // screen, and nothing the process needs closes with an old one.
    private var activity: Activity = first
    private var handled: Intent? = null
    private val io = Executors.newFixedThreadPool(2)
    private val microphone = Microphone()
    private val shares = SharedFiles(activity)
    private val downloads = Downloads(activity)
    private val clips = ClipTrimmer(activity)
    private val previews = PreviewBrowser { activity }
    private val reports: Reports = Reports({ activity }, { foreground && !appLock.state().optBoolean("locked") }, io)
    private val notifications = Notifications(activity)
    private val links = AppLinks()
    private val update = AppUpdate(activity.applicationContext)
    private var updateOpened: String? = null
    private val appLock: AppLock =
        AppLock(
            activity,
            { event ->
                val locked = event.optJSONObject("state")?.optBoolean("locked") == true
                if (locked) {
                    reports.pause()
                    previews.close()
                } else reports.resume()
                // Locked, the screen shows no alerts: the watcher raises them.
                AlertWatcher.screen = foreground && !locked
                events?.send(event)
            },
            { invoke, intent -> startActivityForResult(invoke, intent, "lockCredential") },
            // Paced, it can take some seconds: on its own thread, and again
            // later should the process end first.
            {
                AlertRedaction.schedule(activity)
                Thread { runCatching { notifications.redact() } }.start()
            },
        )
    private var shell: Shell? = null
    private var web: WebView? = null
    private var events: Channel? = null
    @Volatile private var foreground = true
    private var notificationPrompt = false
    private var notificationHadRationale = false
    private val preferences =
        activity.getSharedPreferences("device-preferences", Activity.MODE_PRIVATE)
    private var scannerOpen = false
    private val microphoneWaiting = mutableMapOf<Long, Invoke>()
    private val microphoneRequests = java.util.concurrent.ConcurrentHashMap<Long, Long>()

    override fun load(webView: WebView) {
        loaded = this
        AppUpdate.listener = { event -> activity.runOnUiThread { events?.send(JSObject(event.toString())) } }
        bind(activity, webView)
        AlertWatcher.attach(activity)
        // The plugin may load after the screen's first onResume.
        AlertWatcher.screen =
            (activity as? AppCompatActivity)?.lifecycle?.currentState?.isAtLeast(Lifecycle.State.RESUMED) == true &&
                !appLock.state().optBoolean("locked")
        if (AlertService.wanted(activity)) AlertService.start(activity)
        AlertCheck.schedule(activity)
    }

    private fun bind(screen: Activity, webView: WebView) {
        reports.pause()
        activity = screen
        web = webView
        shell = Shell(screen, webView)
        foreground = (screen as? AppCompatActivity)?.lifecycle?.currentState?.isAtLeast(Lifecycle.State.RESUMED) == true
        appLock.attach(screen)
        reports.resume()
        // A relaunched screen keeps the intent its predecessor already handled.
        if (screen.intent !== handled) receive(screen.intent)
    }

    override fun onNewIntent(intent: Intent) {
        receive(intent)
    }

    private fun receive(intent: Intent) {
        handled = intent
        links.accept(intent)
        notifications.accept(intent)
        intent.getStringExtra(AppUpdate.OPEN)?.let {
            intent.removeExtra(AppUpdate.OPEN)
            updateOpened = it
        }
        io.execute {
            shares.accept(intent)
            activity.runOnUiThread { flushEvents() }
        }
    }

    private fun flushEvents() {
        val target = events ?: return
        for (url in links.drain()) target.send(JSObject().put("type", "appLink").put("url", url) as JSObject)
        val pending = shares.pending()
        if (pending.length() > 0)
            target.send(JSObject().put("type", "shared").put("items", pending) as JSObject)
        notifications.takeOpen()?.let {
            target.send(
                JSObject().put("type", "notificationOpen").put("notification", it) as JSObject
            )
        }
        updateOpened?.let {
            updateOpened = null
            val route = JSObject().put("kind", "app_update").put("profileId", it)
            val note = JSObject().put("id", "app-update").put("route", route)
            target.send(JSObject().put("type", "notificationOpen").put("notification", note) as JSObject)
        }
    }

    override fun onPause(activity: AppCompatActivity) {
        if (activity !== this.activity) return
        foreground = false
        reports.pause()
        appLock.pause()
        AlertWatcher.screen = false
        microphone.interrupt("Recording stopped when the app left the foreground.")
        shell?.awake(false)
        events?.send(JSObject().put("type", "background") as JSObject)
    }

    override fun onResume(activity: AppCompatActivity) {
        if (activity !== this.activity) return
        appLock.resume()
        foreground = true
        reports.resume()
        AlertWatcher.screen = !appLock.state().optBoolean("locked")
        // Without Unrestricted battery use, Android stops the service soon
        // after the app leaves the screen; it runs again while the app is open.
        if (AlertService.wanted(activity) && !AlertWatcher.running()) AlertService.start(activity)
        AlertWatcher.retry()
        shell?.apply()
        events?.send(JSObject().put("type", "foreground") as JSObject)
        val waiting = microphoneWaiting.values.toList()
        microphoneWaiting.clear()
        waiting.forEach { beginMic(it) }
    }

    override fun onStop(activity: AppCompatActivity) {
        microphone.cancelPending()
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        shell?.configure(newConfig)
    }

    override fun onDestroy(activity: AppCompatActivity) {
        // A screen that was already replaced takes nothing of its successor's with it.
        if (activity !== this.activity) return
        foreground = false
        reports.pause()
        previews.close()
        io.execute { clips.close() }
        appLock.destroy()
        microphone.shutdown()
        // The next screen's app registers its own listener.
        events = null
    }

    private fun work(invoke: Invoke, message: String, action: () -> Any?) {
        io.execute {
            try {
                val result = action()
                if (result == null) invoke.resolve() else invoke.resolveObject(result)
            } catch (_: Exception) {
                invoke.reject(message)
            }
        }
    }

    @Command
    fun keyCreate(invoke: Invoke) =
        work(invoke, "The phone could not create a protected device key.") {
            mapOf("publicKey" to Keys.create(invoke.getArgs().getString("alias")))
        }

    @Command
    fun keySign(invoke: Invoke) =
        work(invoke, "The phone could not sign with its protected device key.") {
            val a = invoke.getArgs()
            mapOf(
                "signature" to
                    Keys.sign(a.getString("alias"), Keys.decode(a.getString("transcript")))
            )
        }

    @Command
    fun keyWrap(invoke: Invoke) =
        work(invoke, "Android Keystore could not protect the device key.") {
            val a = invoke.getArgs()
            mapOf("sealed" to Keys.wrap(a.getString("alias"), Keys.decode(a.getString("seed"))))
        }

    @Command
    fun keyUnwrap(invoke: Invoke) =
        work(invoke, "The protected device key is unavailable. Pair again on the computer.") {
            val a = invoke.getArgs()
            val seed = Keys.unwrap(a.getString("alias"), Keys.decode(a.getString("sealed")))
            try {
                mapOf("seed" to Keys.encode(seed))
            } finally {
                seed.fill(0)
            }
        }

    @Command
    fun keyDelete(invoke: Invoke) =
        work(invoke, "The phone could not remove its device key.") {
            Keys.delete(invoke.getArgs().getString("alias"))
            null
        }

    @Command
    fun micStart(invoke: Invoke) {
        if (!foreground) {
            invoke.reject("Open the app before recording.")
            return
        }
        microphoneRequests[invoke.id] = microphone.prepare()
        if (getPermissionState("microphone") != PermissionState.GRANTED)
            requestPermissionForAlias("microphone", invoke, "micPermission")
        else beginMic(invoke)
    }

    @PermissionCallback
    fun micPermission(invoke: Invoke) {
        if (getPermissionState("microphone") == PermissionState.GRANTED) {
            if (foreground) beginMic(invoke) else microphoneWaiting[invoke.id] = invoke
        } else {
            microphoneRequests.remove(invoke.id)
            invoke.reject("Microphone permission was denied. Typing is still available.")
        }
    }

    private fun beginMic(invoke: Invoke) =
        work(
            invoke,
            "The microphone could not start. Check its permission and whether another app is recording.",
        ) {
            val expected = microphoneRequests.remove(invoke.id) ?: error("Recording cancelled.")
            check(foreground)
            microphone.start(invoke.getArgs().getLong("linkId"), expected)
            null
        }

    @Command
    fun micRead(invoke: Invoke) =
        work(invoke, "The microphone was interrupted.") {
            microphone.read(invoke.getArgs().getLong("linkId"))
        }

    @Command
    fun micStop(invoke: Invoke) =
        work(invoke, "The microphone could not finish recording.") {
            val a = invoke.getArgs()
            mapOf(
                "pcm" to
                    Keys.encode(microphone.stop(a.getLong("linkId"), a.optBoolean("drain", false)))
            )
        }

    @Command
    fun shareText(invoke: Invoke) {
        val text = invoke.getArgs().getString("text")
        activity.runOnUiThread {
            try {
                val intent = Intent(Intent.ACTION_SEND).apply {
                    type = "text/plain"
                    putExtra(Intent.EXTRA_TEXT, text)
                }
                activity.startActivity(Intent.createChooser(intent, "Send steps to your computer"))
                invoke.resolve()
            } catch (_: Exception) {
                invoke.reject("The phone could not open the share sheet.")
            }
        }
    }

    @Command
    fun events(invoke: Invoke) {
        events = invoke.parseArgs(EventArgs::class.java).events
        shares.ready()
        shell?.apply()
        flushEvents()
        invoke.resolve()
    }

    @Command
    fun sharedRead(invoke: Invoke) =
        work(invoke, "The shared file is no longer available.") {
            val a = invoke.getArgs()
            mapOf("bytes" to Keys.encode(shares.read(a.getString("id"), a.getLong("offset"))))
        }

    @Command
    fun sharedAck(invoke: Invoke) =
        work(invoke, "The phone could not finish importing this shared item.") {
            shares.acknowledge(invoke.getArgs().getString("id"))
            null
        }

    @Command
    fun saveBegin(invoke: Invoke) =
        work(
            invoke,
            "The phone could not create this download. Check its name and available storage.",
        ) {
            val a = invoke.getArgs()
            mapOf(
                "id" to downloads.begin(a.getString("name"), a.getString("mime"), a.getLong("size"))
            )
        }

    @Command
    fun saveChunk(invoke: Invoke) =
        work(invoke, "The phone could not write this download.") {
            val a = invoke.getArgs()
            downloads.write(a.getString("id"), Keys.decode(a.getString("bytes")))
            null
        }

    @Command
    fun saveFinish(invoke: Invoke) =
        work(invoke, "The download is incomplete or could not be saved.") {
            mapOf("savedAs" to downloads.finish(invoke.getArgs().getString("id")))
        }

    @Command
    fun saveCancel(invoke: Invoke) =
        work(invoke, "The phone could not remove the incomplete download.") {
            downloads.cancel(invoke.getArgs().getString("id"))
            null
        }

    /** Which of a computer's offered apps this one can update to, if any. */
    @Command
    fun appUpdateCheck(invoke: Invoke) {
        io.execute {
            try {
                val offers = invoke.getArgs().optJSONArray("offers") ?: org.json.JSONArray()
                invoke.resolve(
                    JSObject()
                        .put("offer", AppUpdate.matching(activity, offers) ?: org.json.JSONObject.NULL)
                        .put("version", AppUpdate.identity(activity).getString("version"))
                        // Before Android 12 the person confirms each update.
                        .put("asks", Build.VERSION.SDK_INT < 31) as JSObject
                )
            } catch (_: Exception) {
                invoke.reject("The phone couldn’t check this app’s version.")
            }
        }
    }

    private fun updateWork(invoke: Invoke, action: () -> Any?) {
        io.execute {
            try { val result = action(); if (result == null) invoke.resolve() else invoke.resolveObject(result) }
            catch (error: UpdateFailure) { invoke.reject(error.message) }
            catch (_: Exception) { invoke.reject("The phone couldn’t take the update. Check its free storage and try again.") }
        }
    }
    @Command fun appUpdateBegin(invoke: Invoke) = updateWork(invoke) {
        val args = invoke.getArgs()
        update.begin(args.getLong("size"), args.getString("sha256"))
        AppUpdate.from(activity, args.getString("profileId"))
        null
    }
    @Command fun appUpdateChunk(invoke: Invoke) = updateWork(invoke) {
        update.write(Keys.decode(invoke.getArgs().getString("bytes")))
        null
    }
    @Command fun appUpdateCommit(invoke: Invoke) = updateWork(invoke) {
        update.commit()
        null
    }
    @Command fun appUpdateCancel(invoke: Invoke) = updateWork(invoke) {
        update.cancel()
        null
    }

    @Command
    fun notify(invoke: Invoke) {
        if (notificationState() != "granted") {
            invoke.reject("Task alerts are off.")
            return
        }
        showNotification(invoke)
    }

    /** Clears alerts answered elsewhere, whichever of the app and the watcher raised them. */
    @Command
    fun cancelNotifications(invoke: Invoke) {
        val ids = invoke.getArgs().optJSONArray("ids")
        for (i in 0 until (ids?.length() ?: 0)) ids?.optString(i)?.takeIf { it.isNotEmpty() }?.let(notifications::cancel)
        invoke.resolve()
    }

    private fun notificationState(): String {
        if (Build.VERSION.SDK_INT >= 33) {
            val permission = getPermissionState("notifications")
            if (permission != PermissionState.GRANTED) {
                val rationale = ActivityCompat.shouldShowRequestPermissionRationale(
                    activity, Manifest.permission.POST_NOTIFICATIONS,
                )
                // Tauri caches DENIED even when the first prompt is swiped away.
                // Preserve that distinction; a second denial removes the rationale.
                val blocked = preferences.getBoolean(
                    "notificationPromptBlocked", permission == PermissionState.DENIED,
                )
                return if (rationale || permission == PermissionState.PROMPT || !blocked) "promptable" else "blocked"
            }
        }
        return if (notifications.enabled()) "granted" else "blocked"
    }

    @Command
    fun notificationState(invoke: Invoke) {
        invoke.resolveObject(notificationState())
    }

    @Command
    fun notificationSettings(invoke: Invoke) {
        if (!foreground) {
            invoke.reject("Open the app before changing task alerts.")
            return
        }
        try {
            activity.startActivity(
                Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                    .putExtra(Settings.EXTRA_APP_PACKAGE, activity.packageName)
            )
            invoke.resolve()
        } catch (_: Exception) {
            invoke.reject("Android’s notification settings could not be opened.")
        }
    }

    @Command
    fun requestNotifications(invoke: Invoke) {
        if (!foreground || notificationPrompt) {
            invoke.reject("Open the app and finish the current permission request first.")
            return
        }
        when (notificationState()) {
            "granted" -> { invoke.resolve(); return }
            "blocked" -> {
                invoke.reject("Task alerts are blocked in Android settings.")
                return
            }
        }
        // Every way in has already said why, so Android's own prompt comes next.
        notificationPrompt = true
        notificationHadRationale = ActivityCompat.shouldShowRequestPermissionRationale(
            activity, Manifest.permission.POST_NOTIFICATIONS,
        )
        requestPermissionForAlias("notifications", invoke, "notificationPermission")
    }

    @PermissionCallback
    fun notificationPermission(invoke: Invoke) {
        notificationPrompt = false
        val blocked = notificationHadRationale &&
            getPermissionState("notifications") != PermissionState.GRANTED &&
            !ActivityCompat.shouldShowRequestPermissionRationale(
                activity, Manifest.permission.POST_NOTIFICATIONS,
            )
        preferences.edit().putBoolean("notificationPromptBlocked", blocked).apply()
        // A declined or dismissed prompt is a permission state, not a delivery error.
        invoke.resolve()
    }

    /**
     * The app's own alerts stop where the shade has no room, as the
     * watcher's wait for it: both take room in turn. One answered while it
     * waits to post is not posted, and the app is told it was cleared.
     */
    private fun showNotification(invoke: Invoke) {
        val since = notifications.clears()
        io.execute {
            val posted =
                try {
                    val a = invoke.getArgs()
                    check(notifications.enabled())
                    notifications.show(
                        a.getString("id"),
                        a.getString("title"),
                        a.getString("body"),
                        a.opt("route"),
                        a.opt("category") as? String,
                        a.opt("group") as? String,
                        a.opt("groupTitle") as? String,
                        since,
                    )
                } catch (_: Exception) {
                    invoke.reject("The phone could not show this notification.")
                    return@execute
                }
            when (posted) {
                Notifications.Posted.SHOWN -> invoke.resolve()
                Notifications.Posted.CLEARED -> invoke.resolveObject(mapOf("cleared" to true))
                Notifications.Posted.FULL -> invoke.reject("Android has no room for more alerts.")
            }
        }
    }

    /** Turns the background alert service on or off. */
    @Command
    fun alertsService(invoke: Invoke) {
        val run = invoke.getArgs().optBoolean("run", false)
        if (!AlertService.want(activity, run)) {
            invoke.reject("The phone could not save your alert choice.")
            return
        }
        AlertCheck.schedule(activity)
        if (!run) {
            activity.stopService(Intent(activity, AlertService::class.java))
        } else if (!AlertService.start(activity)) {
            invoke.reject("Android did not let alerts start. Open the app and try again.")
            return
        }
        invoke.resolve()
    }

    @Command
    fun alertsInfo(invoke: Invoke) {
        val power = activity.getSystemService(PowerManager::class.java)
        invoke.resolve(
            JSObject()
                .put("running", AlertWatcher.running())
                .put("unrestricted", power.isIgnoringBatteryOptimizations(activity.packageName))
                .put("needsYou", notifications.channelOn(Notifications.NEEDS_YOU))
                .put("finished", notifications.channelOn(Notifications.FINISHED)) as JSObject
        )
    }

    /** Asks Android to let alerts run without battery limits. */
    @Command
    fun alertsBattery(invoke: Invoke) {
        if (!foreground) {
            invoke.reject("Open the app before changing battery settings.")
            return
        }
        val ask =
            Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS)
                .setData(Uri.parse("package:${activity.packageName}"))
        try {
            activity.startActivity(ask)
            invoke.resolve()
        } catch (_: Exception) {
            try {
                activity.startActivity(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS))
                invoke.resolve()
            } catch (_: Exception) {
                invoke.reject("Android’s battery settings could not be opened.")
            }
        }
    }

    /** Opens Android's settings for one of the app's notification categories. */
    @Command
    fun alertsChannel(invoke: Invoke) {
        val channel = invoke.getArgs().optString("channel")
        if (channel !in listOf(Notifications.NEEDS_YOU, Notifications.FINISHED)) {
            invoke.reject("Unknown notification category.")
            return
        }
        if (!foreground) {
            invoke.reject("Open the app before changing notification settings.")
            return
        }
        try {
            activity.startActivity(
                Intent(Settings.ACTION_CHANNEL_NOTIFICATION_SETTINGS)
                    .putExtra(Settings.EXTRA_APP_PACKAGE, activity.packageName)
                    .putExtra(Settings.EXTRA_CHANNEL_ID, channel)
            )
            invoke.resolve()
        } catch (_: Exception) {
            invoke.reject("Android’s notification settings could not be opened.")
        }
    }

    @Command
    fun keepAwake(invoke: Invoke) {
        shell?.awake(invoke.getArgs().optBoolean("on", false) && foreground)
        invoke.resolve()
    }

    @Command
    fun immersive(invoke: Invoke) {
        shell?.immersive(invoke.getArgs().optBoolean("on", false))
        invoke.resolve()
    }

    @Command
    fun theme(invoke: Invoke) {
        shell?.theme(invoke.getArgs().optBoolean("dark", false))
        invoke.resolve()
    }

    @Command
    fun haptic(invoke: Invoke) {
        web?.performHapticFeedback(android.view.HapticFeedbackConstants.CONTEXT_CLICK)
        invoke.resolve()
    }

    @Command
    fun info(invoke: Invoke) {
        val info = activity.packageManager.getPackageInfo(activity.packageName, 0)
        invoke.resolveObject(
            mapOf("version" to info.versionName, "build" to info.longVersionCode.toString(),
                "installerPackage" to installerPackage(), "deviceName" to deviceName())
        )
    }

    /** The device name the person set in Settings, or the model when there is none. */
    private fun deviceName(): String {
        val named = try {
            Settings.Global.getString(activity.contentResolver, Settings.Global.DEVICE_NAME)
        } catch (_: Exception) { null }
        return named?.takeIf { it.isNotBlank() } ?: Build.MODEL
    }

    @Command
    fun scan(invoke: Invoke) {
        if (scannerOpen) {
            invoke.reject("The camera scanner is already open.")
            return
        }
        if (getPermissionState("camera") != PermissionState.GRANTED)
            requestPermissionForAlias("camera", invoke, "cameraPermission")
        else startScanner(invoke)
    }

    @PermissionCallback
    fun cameraPermission(invoke: Invoke) {
        if (getPermissionState("camera") == PermissionState.GRANTED) startScanner(invoke)
        else invoke.reject("Camera permission was denied. Allow camera access to scan a code.")
    }

    private fun startScanner(invoke: Invoke) {
        scannerOpen = true
        startActivityForResult(invoke, Intent(activity, ScannerActivity::class.java), "scanned")
    }

    @ActivityCallback
    fun scanned(invoke: Invoke, result: ActivityResult) {
        scannerOpen = false
        val error = result.data?.getStringExtra("error")
        if (error != null) invoke.reject(error)
        else {
            val content = result.data?.getStringExtra("content")
            if (content == null) invoke.resolve() else invoke.resolveObject(content)
        }
    }

    @Suppress("DEPRECATION")
    private fun installerPackage(): String? = try {
        if (Build.VERSION.SDK_INT >= 30) activity.packageManager.getInstallSourceInfo(activity.packageName).installingPackageName
        else activity.packageManager.getInstallerPackageName(activity.packageName)
    } catch (_: Exception) { null }

    @Command fun lockState(invoke: Invoke) { invoke.resolve(appLock.state()) }
    @Command fun lockConfigure(invoke: Invoke) { appLock.configure(invoke) }
    @Command fun unlock(invoke: Invoke) { appLock.unlock(invoke) }
    @Command fun confirm(invoke: Invoke) { appLock.confirm(invoke) }
    @Command fun lock(invoke: Invoke) { appLock.lock(invoke) }
    @Command fun lockScreenReady(invoke: Invoke) { appLock.screenReady(invoke) }
    @ActivityCallback fun lockCredential(invoke: Invoke, result: ActivityResult) {
        appLock.credentialResult(invoke, result.resultCode == Activity.RESULT_OK)
    }

    @Command
    fun previewShow(invoke: Invoke) {
        if (!foreground || appLock.state().optBoolean("locked")) {
            invoke.reject("Unlock the app before opening a preview.")
            return
        }
        previews.show(invoke)
    }

    @Command
    fun previewClose(invoke: Invoke) {
        previews.close(invoke.getArgs().getString("id"))
        invoke.resolve()
    }

    @Command
    fun previewPending(invoke: Invoke) =
        work(invoke, "Saved preview pictures could not be read.") {
            val args = invoke.getArgs()
            mapOf(
                "pictures" to
                    previews.pending(args.getString("profileId"), args.getString("previewId"))
            )
        }

    @Command
    fun previewPictureRead(invoke: Invoke) =
        work(invoke, "The preview picture is unavailable.") {
            val args = invoke.getArgs()
            mapOf(
                "bytes" to Keys.encode(previews.read(args.getString("id"), args.getLong("offset")))
            )
        }

    @Command
    fun previewPictureAck(invoke: Invoke) =
        work(invoke, "The preview picture could not be removed.") {
            previews.acknowledge(invoke.getArgs().getString("id"))
            null
        }
    private fun clipWork(invoke: Invoke, action: () -> Any?) {
        if (!foreground || appLock.state().optBoolean("locked")) {
            invoke.reject("Unlock the app before preparing a clip.")
            return
        }
        io.execute {
            try { val result = action(); if (result == null) invoke.resolve() else invoke.resolveObject(result) }
            catch (error: ClipFailure) { invoke.reject(error.message ?: "The clip could not be prepared.") }
            catch (_: Exception) { invoke.reject("The clip could not be prepared. Check available device storage.") }
        }
    }
    @Command fun clipBegin(invoke: Invoke) = clipWork(invoke) {
        val args = invoke.getArgs()
        clips.begin(args.getLong("size"), args.getString("sha256"))
    }
    @Command fun clipWrite(invoke: Invoke) = clipWork(invoke) {
        val args = invoke.getArgs()
        clips.write(args.getString("id"), Keys.decode(args.getString("bytes")))
        null
    }
    @Command fun clipTrim(invoke: Invoke) = clipWork(invoke) {
        val args = invoke.getArgs()
        clips.trim(args.getString("id"), args.getLong("startMs"), args.getLong("endMs"))
    }
    @Command fun clipRead(invoke: Invoke) = clipWork(invoke) {
        val args = invoke.getArgs()
        mapOf("bytes" to Keys.encode(clips.read(args.getString("id"), args.getLong("offset"))))
    }
    @Command fun clipDiscard(invoke: Invoke) = work(invoke, "The temporary clip could not be removed.") {
        clips.discard(invoke.getArgs().getString("id"))
        null
    }


    @Command fun screenshotSupported(invoke: Invoke) = reports.supported(invoke)
    @Command fun startScreenshotWatch(invoke: Invoke) = reports.start(invoke)
    @Command fun stopScreenshotWatch(invoke: Invoke) = reports.stop(invoke)
    @Command fun takeNativeCrashes(invoke: Invoke) = reports.takeCrashes(invoke)
    @Command fun captureOwnView(invoke: Invoke) = reports.capture(invoke)

    companion object {
        @Volatile private var loaded: DevicePlugin? = null

        /** A new screen made its WebView. The first one arrives through [load]. */
        @JvmStatic
        fun webViewCreated(screen: Activity, webView: WebView) {
            val plugin = loaded ?: return
            if (plugin.web !== webView) plugin.bind(screen, webView)
        }
    }
}
