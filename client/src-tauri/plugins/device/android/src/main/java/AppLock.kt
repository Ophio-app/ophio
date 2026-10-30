// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.app.Activity
import android.app.KeyguardManager
import android.content.Intent
import android.hardware.biometrics.BiometricManager.Authenticators
import android.hardware.biometrics.BiometricPrompt
import android.os.Build
import android.os.CancellationSignal
import android.os.SystemClock
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.LinearLayout
import android.widget.TextView
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject

/** Phone-local authentication. No companion command or authority enters this class. */
internal class AppLock(
    private var activity: Activity,
    private val emit: (JSObject) -> Unit,
    private val credential: (Invoke, Intent) -> Unit,
    /** Called once the lock is turned on, to hide what alerts already show. */
    private val turnedOn: () -> Unit,
) {
    private val prefs = activity.getSharedPreferences("app-lock", Activity.MODE_PRIVATE)
    private val keyguard = activity.getSystemService(KeyguardManager::class.java)
    private var enabled = prefs.getBoolean("enabled", false)
    private var minutes = prefs.getInt("minutes", 0).takeIf { it in listOf(0, 1, 5, 15) } ?: 0
    private var locked = enabled
    private var leftAt: Long? = null
    private var curtain: LinearLayout? = null
    private var prompt: CancellationSignal? = null
    private var afterCredential: (() -> Unit)? = null

    fun state(): JSObject = JSObject().apply {
        put("available", keyguard.isDeviceSecure)
        put("reason", if (keyguard.isDeviceSecure) null else "Set a PIN, password or pattern in Android’s screen lock settings first.")
        put("enabled", enabled)
        put("locked", locked)
        put("relockMinutes", minutes)
    }

    /** Paints onto [screen], which may replace the screen the lock was painting on. */
    fun attach(screen: Activity) {
        if (screen !== activity) {
            activity = screen
            curtain = null
        }
        paint()
    }

    fun pause() {
        if (!enabled) return
        // Protect recents before Android takes its task snapshot, including during a grace period.
        activity.window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        // A canceled prompt may leave the activity in the background. Keep the
        // departure time even while authenticating so returning cannot bypass relock.
        if (leftAt == null) leftAt = SystemClock.elapsedRealtime()
        if (prompt == null && afterCredential == null) {
            if (minutes == 0) locked = true
        }
        publish()
    }

    fun resume() {
        val left = leftAt
        if (enabled && left != null && SystemClock.elapsedRealtime() - left >= minutes * 60_000L) locked = true
        leftAt = null
        publish()
    }

    fun lock(invoke: Invoke) {
        if (enabled) locked = true
        publish()
        invoke.resolve(state())
    }

    fun configure(invoke: Invoke) {
        val args = invoke.getArgs()
        val nextEnabled = args.optBoolean("enabled", false)
        val nextMinutes = args.optInt("relockMinutes", -1)
        if (nextMinutes !in listOf(0, 1, 5, 15)) { invoke.reject("Choose when the app should lock again."); return }
        authenticate(invoke) {
            if (!prefs.edit().putBoolean("enabled", nextEnabled).putInt("minutes", nextMinutes).commit()) {
                invoke.reject("The lock setting could not be saved.")
            } else {
                val was = enabled
                enabled = nextEnabled
                minutes = nextMinutes
                locked = false
                leftAt = null
                publish()
                if (enabled && !was) turnedOn()
                invoke.resolve(state())
            }
        }
    }

    fun unlock(invoke: Invoke) {
        if (!enabled || !locked) { invoke.resolve(state()); return }
        authenticate(invoke) {
            locked = false
            leftAt = null
            publish()
            invoke.resolve(state())
        }
    }

    /** Asks for the phone's own unlock before one account action, every time. */
    fun confirm(invoke: Invoke) {
        val title = invoke.getArgs().optString("title", "").trim().take(80).ifBlank { "Confirm" }
        authenticate(invoke, title, "Confirms this one action on your computer.") { invoke.resolve(state()) }
    }

    private fun authenticate(
        invoke: Invoke,
        title: String = "Unlock app",
        description: String = "Unlocking the app doesn’t approve or start work.",
        success: () -> Unit,
    ) {
        if (!keyguard.isDeviceSecure) { invoke.reject("Set a secure screen lock in Android settings first."); return }
        if (prompt != null || afterCredential != null) { invoke.reject("An unlock request is already open."); return }
        if (Build.VERSION.SDK_INT < 30) {
            // Android 10 cannot combine strong biometrics and credential in BiometricPrompt.
            // Use only the system device-credential UI there; never accept a weak biometric.
            @Suppress("DEPRECATION")
            val intent = keyguard.createConfirmDeviceCredentialIntent(title, description)
            if (intent == null) { invoke.reject("Android’s screen lock is unavailable."); return }
            afterCredential = success
            credential(invoke, intent)
            return
        }
        val cancellation = CancellationSignal()
        prompt = cancellation
        try {
            BiometricPrompt.Builder(activity)
            .setTitle(title)
            .setSubtitle("Use your biometrics or screen lock")
            .setDescription(description)
            .setAllowedAuthenticators(Authenticators.BIOMETRIC_STRONG or Authenticators.DEVICE_CREDENTIAL)
            .setConfirmationRequired(true)
            .build()
            .authenticate(cancellation, activity.mainExecutor, object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                    prompt = null
                    success()
                }
                override fun onAuthenticationError(code: Int, message: CharSequence) {
                    prompt = null
                    invoke.reject(if (title == "Unlock app") "Not unlocked. Use your phone’s biometrics or screen lock to try again." else "Not confirmed. Nothing was sent to the computer.")
                    publish()
                }
            })
        } catch (_: RuntimeException) {
            prompt = null
            invoke.reject("Android’s unlock prompt is unavailable. Try again with the app in the foreground.")
            publish()
        }
    }

    fun credentialResult(invoke: Invoke, accepted: Boolean) {
        val success = afterCredential
        afterCredential = null
        if (accepted && success != null) success() else invoke.reject("Not unlocked. Try again with your screen lock.")
        publish()
    }

    private fun publish() {
        paint()
        emit(JSObject().put("type", "lockChanged").put("state", state()) as JSObject)
    }

    private fun paint() {
        // Keep snapshots protected throughout the configured policy, even if JS is suspended.
        if (enabled) activity.window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        else activity.window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
        val root = activity.findViewById<ViewGroup>(android.R.id.content) ?: return
        if (curtain == null) {
            curtain = LinearLayout(activity).apply {
                orientation = LinearLayout.VERTICAL
                gravity = Gravity.CENTER
                val pad = (24 * resources.displayMetrics.density).toInt()
                setPadding(pad, pad, pad, pad)
                val background = android.util.TypedValue()
                activity.theme.resolveAttribute(android.R.attr.colorBackground, background, true)
                setBackgroundColor(background.data)
                isClickable = true
                isFocusable = true
                importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_YES
                addView(TextView(activity).apply { text = "App locked"; textSize = 24f; gravity = Gravity.CENTER })
                addView(TextView(activity).apply {
                    text = "Unlock with your phone’s biometrics or screen lock."
                    textSize = 16f
                    gravity = Gravity.CENTER
                    setPadding(0, pad, 0, pad)
                })
                // The WebView lock screen drives the native prompt. Until it is ready,
                // this opaque cover prevents a frame of remembered private content.
            }
            root.addView(curtain, ViewGroup.LayoutParams(-1, -1))
        }
        curtain?.visibility = if (locked) View.VISIBLE else View.GONE
        if (locked) curtain?.bringToFront()
    }

    /** Called only after React has removed all private screens and painted its unlock view. */
    fun screenReady(invoke: Invoke) {
        curtain?.visibility = View.GONE
        invoke.resolve(state())
    }

    fun destroy() { prompt?.cancel(); prompt = null; afterCredential = null }
}
