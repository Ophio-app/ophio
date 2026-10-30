// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.app.Activity
import android.app.Instrumentation
import android.app.UiAutomation
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.Rect
import android.os.Bundle
import android.os.SystemClock
import android.view.InputDevice
import android.view.MotionEvent
import android.view.WindowInsets
import android.view.WindowManager
import android.view.accessibility.AccessibilityNodeInfo
import app.tauri.plugin.Invoke
import com.fasterxml.jackson.databind.ObjectMapper
import java.io.File
import java.net.InetAddress
import java.net.ServerSocket
import java.util.concurrent.atomic.AtomicReference
import kotlin.concurrent.thread
import kotlin.math.abs
import org.json.JSONArray
import org.json.JSONObject

/** Holds the private preview for PreviewChecks, as the app's own activity does. */
class PreviewHost : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setShowWhenLocked(true)
        setTurnScreenOn(true)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
    }
}

/**
 * The private preview fills the screen in the orientation the phone is in: a
 * page's corners are whole on screen, below the bar and clear of the system
 * bars, and the X closes it with the picture taken. Its screenshots go to this
 * check app's own external files.
 */
internal object PreviewChecks {
    private const val PAGE =
        """<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1">
<style>html,body{margin:0;height:100%;background:#fff}#tl,#br{position:fixed;width:40px;height:40px}
#tl{left:0;top:0;background:#0000ee}#br{right:0;bottom:0;background:#00aa00}
p{margin:0;padding:60px 24px;font:18px sans-serif}</style>
<div id="tl"></div><div id="br"></div><p>Preview check</p>"""

    fun run(instrumentation: Instrumentation, name: String): String {
        val server = ServerSocket(0, 8, InetAddress.getByName("127.0.0.1"))
        val serving = thread { serve(server) }
        try {
            return if (name == "questions") asked(instrumentation, server.localPort)
            else once(instrumentation, server.localPort, name)
        } finally {
            server.close()
            serving.join(2_000)
        }
    }

    private fun once(i: Instrumentation, port: Int, name: String): String {
        val a = i.uiAutomation
        val out = i.targetContext.getExternalFilesDir(null)!!
        val host =
            i.startActivitySync(
                Intent(i.targetContext, PreviewHost::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            )
        val browser = PreviewBrowser { host }
        val answer = AtomicReference<String?>(null)
        val args =
            JSONObject()
                .put("url", "http://127.0.0.1:$port/")
                .put("id", "check-$name")
                .put("profileId", "check")
                .put("previewId", "check")
                .toString()
        browser.show(Invoke(1, "previewShow", 1, 2, { _, data -> answer.set(data) }, args, ObjectMapper()))
        try {
            until("page shown") { find(a, "Private preview") != null }
            Thread.sleep(1_000)
            val shot = a.takeScreenshot().copy(Bitmap.Config.ARGB_8888, false)
            save(shot, File(out, "preview-$name.png"))
            val density = host.resources.displayMetrics.density
            val side = 40 * density
            val blue = box(shot, 0x00, 0x00, 0xee) ?: error("$name: top left corner not on screen")
            val green = box(shot, 0x00, 0xaa, 0x00) ?: error("$name: bottom right corner not on screen")
            fun whole(b: Rect) = abs(b.width() - side) <= 3 && abs(b.height() - side) <= 3
            check(whole(blue)) { "$name: top left corner cut to $blue" }
            check(whole(green)) { "$name: bottom right corner cut to $green" }
            val close = find(a, "Close preview") ?: error("$name: no X")
            val x = Rect().also { close.getBoundsInScreen(it) }
            check(x.left < 72 * density && x.width() >= 47 * density) { "$name: X at $x" }
            check(blue.top >= x.bottom) { "$name: page under the bar ($blue, X $x)" }
            val bars =
                host.window.decorView.rootWindowInsets.getInsets(
                    WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout()
                )
            check(green.bottom <= shot.height - bars.bottom && green.right <= shot.width - bars.right) {
                "$name: page under the system bars ($green, bars $bars)"
            }
            click(find(a, "Take picture") ?: error("$name: no Take picture"))
            until("picture saved") { find(a, "Picture saved") != null && find(a, "Annotate") != null }
            Thread.sleep(500)
            save(a.takeScreenshot().copy(Bitmap.Config.ARGB_8888, false), File(out, "preview-$name-saved.png"))
            click(close)
            until("closed") { answer.get() != null }
            val result = JSONObject(answer.get()!!)
            val pictures = result.getJSONArray("pictures")
            check(pictures.length() == 1 && result.isNull("annotate")) { "$name: closed with $result" }
            browser.acknowledge(pictures.getJSONObject(0).getString("id"))
            return "$name ${shot.width}x${shot.height}: X $x, corners whole at $blue and $green, bars $bars"
        } finally {
            browser.close()
            host.finish()
        }
    }

    /**
     * The agent's questions sit under the page: an option chosen, the person's
     * own words for the next, and Submit closes the preview with both answers.
     */
    private fun asked(i: Instrumentation, port: Int): String {
        val a = i.uiAutomation
        val out = i.targetContext.getExternalFilesDir(null)!!
        val host =
            i.startActivitySync(
                Intent(i.targetContext, PreviewHost::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            )
        val browser = PreviewBrowser { host }
        val answer = AtomicReference<String?>(null)
        val questions =
            JSONArray()
                .put(
                    JSONObject()
                        .put("header", "Pay button")
                        .put("question", "Where should the Pay button sit?")
                        .put(
                            "options",
                            JSONArray()
                                .put(JSONObject().put("label", "Under the total"))
                                .put(
                                    JSONObject()
                                        .put("label", "Pinned to the bottom")
                                        .put("description", "Always in reach")
                                ),
                        )
                )
                .put(JSONObject().put("question", "Anything else feel off?").put("options", JSONArray()))
        val args =
            JSONObject()
                .put("url", "http://127.0.0.1:$port/")
                .put("id", "check-questions")
                .put("profileId", "check")
                .put("previewId", "check")
                .put("questions", questions)
                .put(
                    "answers",
                    JSONArray()
                        .put(JSONObject().put("labels", JSONArray()).put("own", ""))
                        .put(JSONObject().put("labels", JSONArray()).put("own", "")),
                )
                .toString()
        browser.show(Invoke(1, "previewShow", 1, 2, { _, data -> answer.set(data) }, args, ObjectMapper()))
        try {
            until("questions shown") { find(a, "Where should the Pay button sit?") != null }
            val submit = find(a, "Submit answers") ?: error("questions: no Submit")
            check(!submit.isEnabled) { "questions: Submit before any answer" }
            click(find(a, "Pinned to the bottom") ?: error("questions: no option"))
            // Fields are found through the tree, as a hint isn't searchable text.
            val fields = editable(a.rootInActiveWindow)
            check(fields.size == 2) { "questions: ${fields.size} fields" }
            val own = fields[1]
            check(
                own.performAction(
                    AccessibilityNodeInfo.ACTION_SET_TEXT,
                    Bundle().apply {
                        putCharSequence(
                            AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,
                            "The total jumps",
                        )
                    },
                )
            ) { "questions: text not set" }
            Thread.sleep(500)
            val shot = a.takeScreenshot().copy(Bitmap.Config.ARGB_8888, false)
            save(shot, File(out, "preview-questions.png"))
            val page = box(shot, 0x00, 0xaa, 0x00) ?: error("questions: the page is hidden by them")
            // Minimized, they wait in a small bubble and the page has the screen to the bottom.
            click(find(a, "Minimize questions") ?: error("questions: no Minimize"))
            until("minimized") { find(a, "Show the agent’s 2 questions") != null }
            check(find(a, "Where should the Pay button sit?") == null) { "questions: still shown minimized" }
            Thread.sleep(500)
            val folded = a.takeScreenshot().copy(Bitmap.Config.ARGB_8888, false)
            save(folded, File(out, "preview-questions-minimized.png"))
            val density = host.resources.displayMetrics.density
            val bars =
                host.window.decorView.rootWindowInsets.getInsets(
                    WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout()
                )
            val corner = box(folded, 0x00, 0xaa, 0x00) ?: error("questions: page corner hidden minimized")
            check(corner.bottom >= folded.height - bars.bottom - 2 && corner.bottom > page.bottom) {
                "questions: page stops at $corner minimized (was $page, bars $bars)"
            }
            val bubble = find(a, "Show the agent’s 2 questions")!!
            val at = Rect().also { bubble.getBoundsInScreen(it) }
            check(at.width() <= 64 * density && at.height() <= 64 * density && at.right > folded.width / 2) {
                "questions: bubble at $at"
            }
            // Dragged across, it settles against the other side and stays a tap to open.
            drag(a, at.centerX().toFloat(), at.centerY().toFloat(), 40 * density, at.centerY() - 120 * density)
            Thread.sleep(600)
            val moved = Rect().also { find(a, "Show the agent’s 2 questions")!!.getBoundsInScreen(it) }
            check(moved.left < 40 * density + bars.left && moved.top < at.top) { "questions: dragged bubble at $moved (from $at)" }
            save(a.takeScreenshot().copy(Bitmap.Config.ARGB_8888, false), File(out, "preview-questions-moved.png"))
            click(find(a, "Show the agent’s 2 questions")!!)
            until("shown again") { find(a, "Where should the Pay button sit?") != null }
            check(find(a, "Show the agent’s 2 questions") == null) { "questions: bubble left over" }
            click(find(a, "Submit answers") ?: error("questions: no Submit"))
            until("submitted") { answer.get() != null }
            val result = JSONObject(answer.get()!!)
            check(result.getBoolean("submitted")) { "questions: closed with $result" }
            val answers = result.getJSONArray("answers")
            check(
                answers.length() == 2 &&
                    answers.getJSONObject(0).getJSONArray("labels").toString() == "[\"Pinned to the bottom\"]" &&
                    answers.getJSONObject(1).getString("own") == "The total jumps"
            ) { "questions: answered $answers" }
            return "questions ${shot.width}x${shot.height}: page corner at $page, answered $answers"
        } finally {
            browser.close()
            host.finish()
        }
    }

    private fun serve(server: ServerSocket) {
        while (!server.isClosed) {
            val socket = runCatching { server.accept() }.getOrNull() ?: return
            socket.use {
                val path = it.getInputStream().bufferedReader().readLine()?.split(' ')?.getOrNull(1)
                val body = (if (path == "/") PAGE else "").toByteArray()
                val head =
                    "HTTP/1.1 ${if (path == "/") "200 OK" else "404 Not Found"}\r\n" +
                        "Content-Type: text/html; charset=utf-8\r\nContent-Length: ${body.size}\r\nConnection: close\r\n\r\n"
                it.getOutputStream().apply {
                    write(head.toByteArray())
                    write(body)
                    flush()
                }
            }
        }
    }

    private fun find(a: UiAutomation, text: String): AccessibilityNodeInfo? =
        a.rootInActiveWindow?.findAccessibilityNodeInfosByText(text)?.firstOrNull()

    private fun editable(node: AccessibilityNodeInfo?): List<AccessibilityNodeInfo> =
        when {
            node == null -> emptyList()
            node.isEditable -> listOf(node)
            else -> (0 until node.childCount).flatMap { editable(node.getChild(it)) }
        }

    /** One finger, down at one point, moved in steps to another and lifted. */
    private fun drag(a: UiAutomation, x0: Float, y0: Float, x1: Float, y1: Float) {
        val down = SystemClock.uptimeMillis()
        fun touch(action: Int, x: Float, y: Float, at: Long) {
            val e = MotionEvent.obtain(down, at, action, x, y, 0)
            e.source = InputDevice.SOURCE_TOUCHSCREEN
            check(a.injectInputEvent(e, true)) { "not injected: $action" }
            e.recycle()
        }
        touch(MotionEvent.ACTION_DOWN, x0, y0, down)
        for (step in 1..12) touch(MotionEvent.ACTION_MOVE, x0 + (x1 - x0) * step / 12, y0 + (y1 - y0) * step / 12, down + step * 16L)
        touch(MotionEvent.ACTION_UP, x1, y1, down + 13 * 16L)
    }

    private fun click(node: AccessibilityNodeInfo) {
        check(node.performAction(AccessibilityNodeInfo.ACTION_CLICK)) { "not clicked: ${node.contentDescription ?: node.text}" }
    }

    private fun until(what: String, done: () -> Boolean) {
        val end = System.currentTimeMillis() + 15_000
        while (!done()) {
            check(System.currentTimeMillis() < end) { "Timed out: $what" }
            Thread.sleep(100)
        }
    }

    /** The bounds of the pixels close to one colour, or null if there are none. */
    private fun box(shot: Bitmap, r: Int, g: Int, b: Int): Rect? {
        val w = shot.width
        val pixels = IntArray(w * shot.height).also { shot.getPixels(it, 0, w, 0, 0, w, shot.height) }
        var found: Rect? = null
        for (n in pixels.indices) {
            val p = pixels[n]
            if (abs((p shr 16 and 0xff) - r) > 16 || abs((p shr 8 and 0xff) - g) > 16 || abs((p and 0xff) - b) > 16) continue
            val x = n % w
            val y = n / w
            found = found?.apply { union(x, y) } ?: Rect(x, y, x, y)
        }
        // Rect.union(x, y) leaves right and bottom on the last pixel; a Rect's lie past it.
        return found?.apply { right += 1; bottom += 1 }
    }

    private fun save(bitmap: Bitmap, file: File) {
        file.outputStream().use { check(bitmap.compress(Bitmap.CompressFormat.PNG, 100, it)) }
    }
}
