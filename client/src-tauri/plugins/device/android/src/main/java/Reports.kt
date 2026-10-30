// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.app.Activity
import android.app.ActivityManager
import android.app.ApplicationExitInfo
import android.graphics.Bitmap
import android.graphics.Canvas
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.util.Base64
import android.view.PixelCopy
import androidx.annotation.RequiresApi
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import java.io.ByteArrayOutputStream
import java.util.concurrent.Executor

internal class Reports(
    private val activity: () -> Activity,
    private val foreground: () -> Boolean,
    private val io: Executor,
) {
    private var wanted = false
    private var events: Channel? = null
    private var watching: CaptureWatch? = null
    private val preferences = activity().getSharedPreferences("report-exits", Activity.MODE_PRIVATE)

    fun supported(invoke: Invoke) = invoke.resolveObject(mapOf("supported" to (Build.VERSION.SDK_INT >= 34)))

    fun start(invoke: Invoke) {
        val channel = invoke.parseArgs(EventArgs::class.java).events
        activity().runOnUiThread {
            events = channel
            wanted = true
            resume()
            supported(invoke)
        }
    }

    fun stop(invoke: Invoke) {
        activity().runOnUiThread {
            wanted = false
            events = null
            pause()
            invoke.resolve()
        }
    }

    fun resume() {
        if (Build.VERSION.SDK_INT < 34 || !wanted || watching != null || !foreground()) return
        watching = CaptureWatch(activity()) {
            if (wanted && foreground()) events?.send(JSObject().put("type", "screenshot") as JSObject)
        }
    }

    fun pause() {
        if (Build.VERSION.SDK_INT >= 34) watching?.close()
        watching = null
    }

    @RequiresApi(34)
    private class CaptureWatch(private val screen: Activity, captured: () -> Unit) {
        private val callback = Activity.ScreenCaptureCallback { captured() }
        init { screen.registerScreenCaptureCallback(screen.mainExecutor, callback) }
        fun close() = screen.unregisterScreenCaptureCallback(callback)
    }

    fun takeCrashes(invoke: Invoke) {
        io.execute {
            try {
                val crashes = if (Build.VERSION.SDK_INT >= 30) exits() else emptyList()
                // Installing an update stops the app, and Android may count that as an exit.
                val screen = activity()
                val updatedAt = screen.packageManager.getPackageInfo(screen.packageName, 0).lastUpdateTime
                invoke.resolveObject(mapOf("crashes" to crashes, "updatedAt" to updatedAt))
            } catch (_: Exception) {
                invoke.reject("The phone could not read its last exit reasons.")
            }
        }
    }

    @RequiresApi(30)
    private fun exits(): List<Map<String, Any>> = synchronized(preferences) {
        val screen = activity()
        val manager = screen.getSystemService(ActivityManager::class.java)
        val last = preferences.getLong("reportedAt", 0)
        val history = manager.getHistoricalProcessExitReasons(screen.packageName, 0, 0)
        val build = screen.packageManager.getPackageInfo(screen.packageName, 0).versionName ?: "unknown"
        val crashes = history.filter { it.timestamp > last }.mapNotNull { exit ->
            val reason = exitCode(exit.reason, exit.status, exit.importance) ?: return@mapNotNull null
            mutableMapOf<String, Any>(
                "at" to exit.timestamp, "side" to "phone", "kind" to "system",
                "code" to "exit:$reason", "build" to build, "frames" to emptyList<Any>(),
            ).apply {
                exit.description?.let { scrub(it) }?.takeIf { it.isNotBlank() }?.let { put("message", it) }
            }
        }.sortedByDescending { it["at"] as Long }
        // Commit before answering so a recreated WebView cannot deliver these twice.
        val newest = history.maxOfOrNull { it.timestamp } ?: last
        check(preferences.edit().putLong("reportedAt", maxOf(last, newest)).commit())
        crashes
    }

    fun capture(invoke: Invoke) {
        activity().runOnUiThread {
            val screen = activity()
            val view = screen.window.decorView
            if (!foreground() || !view.isAttachedToWindow || view.width <= 0 || view.height <= 0) {
                invoke.resolveObject(mapOf("screenshot" to null))
                return@runOnUiThread
            }
            // PixelCopy reads this app's window surface, never the display or other apps.
            val scale = minOf(1.0, 1600.0 / maxOf(view.width, view.height))
            val bitmap = Bitmap.createBitmap(
                maxOf(1, (view.width * scale).toInt()), maxOf(1, (view.height * scale).toInt()), Bitmap.Config.ARGB_8888,
            )
            try {
                PixelCopy.request(screen.window, bitmap, { result ->
                    if (!foreground() || screen !== activity()) {
                        bitmap.recycle()
                        invoke.resolveObject(mapOf("screenshot" to null))
                    } else {
                        // Secure app windows may refuse PixelCopy even while unlocked.
                        // Drawing our own view does not relax the window protection.
                        if (result != PixelCopy.SUCCESS) {
                            try {
                                val canvas = Canvas(bitmap)
                                canvas.scale(bitmap.width.toFloat() / view.width, bitmap.height.toFloat() / view.height)
                                view.draw(canvas)
                            } catch (_: Exception) {
                                bitmap.recycle()
                                invoke.resolveObject(mapOf("screenshot" to null))
                                return@request
                            }
                        }
                        io.execute {
                            val picture = try { encode(bitmap) } catch (_: Exception) { null } finally { bitmap.recycle() }
                            screen.runOnUiThread {
                                invoke.resolveObject(mapOf("screenshot" to if (foreground() && screen === activity()) picture else null))
                            }
                        }
                    }
                }, Handler(Looper.getMainLooper()))
            } catch (_: IllegalArgumentException) {
                bitmap.recycle()
                invoke.resolveObject(mapOf("screenshot" to null))
            }
        }
    }

    private fun encode(original: Bitmap): Map<String, Any>? {
        var bitmap = original
        try {
            repeat(8) {
                for (quality in listOf(85, 65, 45)) {
                    val bytes = ByteArrayOutputStream()
                    if (bitmap.compress(Bitmap.CompressFormat.JPEG, quality, bytes) && bytes.size() <= MAX_BYTES) {
                        return mapOf(
                            "media_type" to "image/jpeg", "data" to Base64.encodeToString(bytes.toByteArray(), Base64.NO_WRAP),
                            "width" to bitmap.width, "height" to bitmap.height,
                        )
                    }
                }
                val smaller = Bitmap.createScaledBitmap(bitmap, maxOf(1, bitmap.width * 3 / 4), maxOf(1, bitmap.height * 3 / 4), true)
                if (bitmap !== original && bitmap !== smaller) bitmap.recycle()
                bitmap = smaller
            }
            return null
        } finally { if (bitmap !== original) bitmap.recycle() }
    }

    companion object {
        private const val MAX_BYTES = 512 * 1024

        /** The report code for an exit worth asking about, or null for an ordinary one. */
        internal fun exitCode(reason: Int, status: Int, importance: Int): String? {
            // Android routinely closes a backgrounded app to reclaim memory or CPU. Those
            // exits only count when the person could see Ophio, so reopening it later
            // doesn't ask to report a crash that never happened.
            // Importance is a priority, not a visibility range: a foreground service
            // ranks between foreground and visible activities without showing either.
            val seen = importance == ActivityManager.RunningAppProcessInfo.IMPORTANCE_FOREGROUND ||
                importance == ActivityManager.RunningAppProcessInfo.IMPORTANCE_VISIBLE
            return when (reason) {
                ApplicationExitInfo.REASON_CRASH -> "crash"
                ApplicationExitInfo.REASON_CRASH_NATIVE -> "crash_native"
                ApplicationExitInfo.REASON_ANR -> "anr"
                ApplicationExitInfo.REASON_LOW_MEMORY -> if (seen) "low_memory" else null
                ApplicationExitInfo.REASON_EXCESSIVE_RESOURCE_USAGE -> if (seen) "excessive_resource_usage" else null
                ApplicationExitInfo.REASON_INITIALIZATION_FAILURE -> "initialization_failure"
                ApplicationExitInfo.REASON_DEPENDENCY_DIED -> if (seen) "dependency_died" else null
                ApplicationExitInfo.REASON_FREEZER -> if (seen) "freezer" else null
                ApplicationExitInfo.REASON_SIGNALED -> if (status in listOf(4, 6, 7, 8, 11)) "signaled" else null
                else -> null
            }
        }

        private fun scrub(text: String): String = text.take(4096)
            .replace(Regex("\"[^\"]*\"|'[^']*'|`[^`]*`|“[^”]*”|‘[^’]*’"), "\"…\"")
            .split(Regex("\\s+"))
            .joinToString(" ") { word ->
                val core = word.trimStart('(', '[').trimEnd('.', ',', ';', ':', ')', ']')
                when {
                    core.contains("://") -> "<url>"
                    core.contains('@') && core.contains('.') && !core.startsWith('@') -> "<email>"
                    core.startsWith('/') || core.startsWith("~/") || core.startsWith("./") ||
                        core.startsWith("../") || core.contains('\\') || core.count { it == '/' } >= 2 -> "<path>"
                    else -> word.replace(Regex("[0-9]{5,}"), "<n>")
                }
            }.trim().take(160)
    }
}
