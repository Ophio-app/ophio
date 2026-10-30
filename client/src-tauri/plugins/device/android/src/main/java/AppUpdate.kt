// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import java.io.File
import java.io.OutputStream
import java.security.MessageDigest
import org.json.JSONArray
import org.json.JSONObject

/** Why an update failed, worded for the person. */
internal class UpdateFailure(message: String) : Exception(message)

/**
 * Updates the app to one a paired computer offers. Android installs only an
 * app signed as this one is, and since Android 12 an app may update itself
 * without asking; before that, or when Android wants the person to allow it,
 * Android asks first. The download goes straight into Android's own install
 * session, so nothing is left behind on the phone either way.
 */
internal class AppUpdate(private val context: Context) {
    private class Open(
        val session: PackageInstaller.Session,
        val stream: OutputStream,
        val size: Long,
        val sha256: String,
        val digest: MessageDigest = MessageDigest.getInstance("SHA-256"),
        var written: Long = 0,
    )

    private val installer = context.packageManager.packageInstaller
    private var open: Open? = null

    init {
        // A session from before the app last closed was never finished.
        abandon()
    }

    @Synchronized
    fun begin(size: Long, sha256: String) {
        require(size in 1..MAX_SIZE && sha256.matches(Regex("[0-9a-f]{64}")))
        cancel()
        abandon()
        val params =
            PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
                setAppPackageName(context.packageName)
                setSize(size)
                setInstallReason(PackageManager.INSTALL_REASON_USER)
                if (Build.VERSION.SDK_INT >= 31)
                    setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
            }
        val id = installer.createSession(params)
        val session = installer.openSession(id)
        try {
            open = Open(session, session.openWrite("base.apk", 0, size), size, sha256)
        } catch (e: Exception) {
            session.abandon()
            throw e
        }
    }

    @Synchronized
    fun write(bytes: ByteArray) {
        val o = open ?: throw UpdateFailure("The update stopped. Try again.")
        require(bytes.size <= 256 * 1024 && o.written + bytes.size <= o.size)
        try {
            o.stream.write(bytes)
            o.digest.update(bytes)
            o.written += bytes.size
        } catch (e: Exception) {
            cancel()
            throw e
        }
    }

    /** Hands the app to Android, which answers [Result] once it is done or wants the person. */
    @Synchronized
    fun commit() {
        val o = open ?: throw UpdateFailure("The update stopped. Try again.")
        open = null
        try {
            if (o.written != o.size || hex(o.digest.digest()) != o.sha256)
                throw UpdateFailure("The app arrived damaged. Try again.")
            o.session.fsync(o.stream)
            o.stream.close()
            preferences(context).edit().putString(INSTALLING, o.sha256).commit()
            val result =
                PendingIntent.getBroadcast(
                    context,
                    0,
                    Intent(context, Result::class.java),
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
                )
            o.session.commit(result.intentSender)
            o.session.close()
        } catch (e: Exception) {
            runCatching { o.stream.close() }
            o.session.abandon()
            preferences(context).edit().remove(INSTALLING).commit()
            throw e
        }
    }

    @Synchronized
    fun cancel() {
        val o = open ?: return
        open = null
        runCatching { o.stream.close() }
        runCatching { o.session.abandon() }
    }

    private fun abandon() {
        for (session in installer.mySessions)
            runCatching { installer.abandonSession(session.sessionId) }
    }

    /** What Android made of an update. The app's process ends when it succeeds. */
    class Result : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            when (val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
                PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                    @Suppress("DEPRECATION")
                    val confirm =
                        if (Build.VERSION.SDK_INT >= 33)
                            intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
                        else intent.getParcelableExtra(Intent.EXTRA_INTENT)
                    try {
                        // The person just asked for it with the app in front.
                        context.startActivity(confirm!!.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                        report("asking", null)
                    } catch (_: Exception) {
                        finished(context)
                        report("failed", "Android didn’t ask to install the update. Try again from Settings › Versions.")
                    }
                }
                PackageInstaller.STATUS_SUCCESS -> report("installed", null)
                else -> {
                    finished(context)
                    report("failed", failure(status, intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)))
                }
            }
        }
    }

    companion object {
        /** Øphio's app is about 30 MB; this leaves room to grow. */
        private const val MAX_SIZE = 512L * 1024 * 1024
        private const val CHANNEL = "updates"
        private const val NOTE = 3
        private const val TAG = "app-update"
        private const val INSTALLING = "installing"
        private const val NOTIFIED = "notified"
        const val OPEN = "appUpdateProfile"

        /** The app's screen, told what became of an update while it is open. */
        @Volatile var listener: ((JSONObject) -> Unit)? = null

        private fun report(state: String, message: String?) {
            listener?.invoke(JSONObject().put("type", "appUpdate").put("state", state).put("message", message ?: JSONObject.NULL))
        }

        private fun preferences(context: Context) =
            context.getSharedPreferences("app-update", Context.MODE_PRIVATE)

        private fun finished(context: Context) {
            preferences(context).edit().remove(INSTALLING).commit()
        }

        private fun failure(status: Int, detail: String?): String =
            when (status) {
                PackageInstaller.STATUS_FAILURE_ABORTED -> "The update was cancelled."
                PackageInstaller.STATUS_FAILURE_BLOCKED -> "Android or this phone’s administrator blocked the update."
                PackageInstaller.STATUS_FAILURE_CONFLICT ->
                    "Android refused the update: it isn’t signed as this app is, or is older. Install it from the computer with `ophio phone install`."
                PackageInstaller.STATUS_FAILURE_INCOMPATIBLE -> "This phone can’t run that version of the app."
                PackageInstaller.STATUS_FAILURE_STORAGE -> "The phone hasn’t enough free storage for the update."
                PackageInstaller.STATUS_FAILURE_INVALID -> "The update file isn’t a valid app."
                else -> "Android couldn’t install the update" + (detail?.let { ": $it" } ?: ".")
            }

        fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

        /** This app as installed: its file's SHA-256, its signers' and its version. */
        fun identity(context: Context): JSONObject {
            val pm = context.packageManager
            val info = pm.getPackageInfo(context.packageName, PackageManager.GET_SIGNING_CERTIFICATES)
            val signing = info.signingInfo ?: error("Android didn’t say how this app is signed.")
            val sha = MessageDigest.getInstance("SHA-256")
            val signers =
                (if (signing.hasMultipleSigners()) signing.apkContentsSigners else signing.signingCertificateHistory)
                    .map { hex(sha.digest(it.toByteArray())) }
            return JSONObject()
                .put("sha256", fileSha256(context, info.lastUpdateTime))
                .put("signers", JSONArray(signers))
                .put("version", info.versionName ?: "")
        }

        /** The installed app's SHA-256, worked out once for each install. */
        private fun fileSha256(context: Context, installed: Long): String {
            val prefs = preferences(context)
            val key = "sha256-$installed"
            prefs.getString(key, null)?.let { return it }
            val digest = MessageDigest.getInstance("SHA-256")
            File(context.applicationInfo.sourceDir).inputStream().use { input ->
                val buffer = ByteArray(256 * 1024)
                while (true) {
                    val n = input.read(buffer)
                    if (n < 0) break
                    digest.update(buffer, 0, n)
                }
            }
            val sha256 = hex(digest.digest())
            val editor = prefs.edit()
            for (old in prefs.all.keys) if (old.startsWith("sha256-")) editor.remove(old)
            editor.putString(key, sha256).commit()
            return sha256
        }

        /**
         * Of a computer's offers, the app this one can update to: signed as it
         * is, a different build, and not older. None if there is none.
         */
        fun matching(context: Context, offers: JSONArray): JSONObject? {
            val me = identity(context)
            val mine = (0 until me.getJSONArray("signers").length()).map { me.getJSONArray("signers").getString(it) }
            val version = me.getString("version")
            for (i in 0 until offers.length()) {
                val offer = offers.optJSONObject(i) ?: continue
                val signers = offer.optJSONArray("signers") ?: continue
                val theirs = (0 until signers.length()).map { signers.getString(it) }.toSet()
                if (mine.isEmpty() || !theirs.containsAll(mine)) continue
                if (offer.optString("sha256") == me.getString("sha256")) return null
                if (older(offer.optString("version"), version)) return null
                return offer
            }
            return null
        }

        /** Whether version `a` comes before `b`, comparing their numbers in turn. */
        internal fun older(a: String, b: String): Boolean {
            val x = a.split('.', '-', '+').map { it.toIntOrNull() ?: 0 }
            val y = b.split('.', '-', '+').map { it.toIntOrNull() ?: 0 }
            for (i in 0 until maxOf(x.size, y.size)) {
                val d = x.getOrElse(i) { 0 } - y.getOrElse(i) { 0 }
                if (d != 0) return d < 0
            }
            return false
        }

        /**
         * Says once, for each new app a computer offers, that an update is
         * ready; a tap opens Settings › Versions for that computer. Returns
         * the offer, or null when there is nothing to update to.
         */
        @Synchronized
        fun offered(context: Context, profileId: String, computer: String, offers: JSONArray): JSONObject? {
            val offer = matching(context, offers)
            val manager = context.getSystemService(NotificationManager::class.java)
            val prefs = preferences(context)
            if (offer == null) {
                if (prefs.contains(NOTIFIED)) manager.cancel(TAG, NOTE)
                prefs.edit().remove(NOTIFIED).commit()
                return null
            }
            val sha256 = offer.getString("sha256")
            if (prefs.getString(NOTIFIED, null) != sha256 && prefs.getString(INSTALLING, null) != sha256) {
                note(context, "Øphio update ready", "$computer has Øphio ${offer.optString("version")} for this phone. Tap to update.", profileId)
                prefs.edit().putString(NOTIFIED, sha256).commit()
            }
            return offer
        }

        /** After an update this app asked for, says it is done. */
        fun replaced(context: Context) {
            val prefs = preferences(context)
            val profile = prefs.getString("profile", null)
            if (prefs.getString(INSTALLING, null) == null) return
            prefs.edit().remove(INSTALLING).remove(NOTIFIED).commit()
            val version = runCatching {
                context.packageManager.getPackageInfo(context.packageName, 0).versionName
            }.getOrNull()
            note(context, "Øphio updated", "The app is now ${version ?: "up to date"}. Tap to open it.", profile)
        }

        /** Remembers which computer an update came from, for the note after it. */
        fun from(context: Context, profileId: String) {
            preferences(context).edit().putString("profile", profileId).commit()
        }

        private fun note(context: Context, title: String, body: String, profileId: String?) {
            val manager = context.getSystemService(NotificationManager::class.java)
            if (!manager.areNotificationsEnabled()) return
            manager.createNotificationChannel(
                NotificationChannel(CHANNEL, "App updates", NotificationManager.IMPORTANCE_DEFAULT)
            )
            val launch = context.packageManager.getLaunchIntentForPackage(context.packageName) ?: return
            if (profileId != null) {
                launch.data = Uri.parse("notification://app-update/" + Uri.encode(profileId))
                launch.putExtra(OPEN, profileId)
            }
            launch.addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
            val open = PendingIntent.getActivity(context, NOTE, launch, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
            manager.notify(
                TAG,
                NOTE,
                Notification.Builder(context, CHANNEL)
                    .setSmallIcon(R.drawable.ic_stat_ophio)
                    .setContentTitle(title)
                    .setContentText(body)
                    .setStyle(Notification.BigTextStyle().bigText(body))
                    .setContentIntent(open)
                    .setAutoCancel(true)
                    .build(),
            )
        }
    }
}
