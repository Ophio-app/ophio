// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.app.Activity
import android.view.ViewTreeObserver
import android.webkit.WebView
import androidx.core.content.ContextCompat
import androidx.core.util.Consumer
import androidx.window.java.layout.WindowInfoTrackerCallbackAdapter
import androidx.window.layout.FoldingFeature
import androidx.window.layout.WindowInfoTracker
import androidx.window.layout.WindowLayoutInfo
import app.tauri.plugin.JSObject
import org.json.JSONArray
import org.json.JSONObject

/** WindowManager owns fold geometry; WebView viewport segments are not enabled.
 * WindowManager uses physical window pixels. Convert from the WebView's actual
 * location and its measured CSS viewport, never display density or screen size. */
internal class WindowLayout(private val emit: (JSObject) -> Unit) {
    private var web: WebView? = null
    private var tracker: WindowInfoTrackerCallbackAdapter? = null
    private var info: WindowLayoutInfo? = null
    private var revision = 0L
    private var busy = false
    private var queued = false
    private var latest = unavailable()
    private var lastSent: String? = null
    private val waiting = mutableListOf<(JSObject) -> Unit>()
    private val changes = ViewTreeObserver.OnGlobalLayoutListener { refresh() }
    private var consumer: Consumer<WindowLayoutInfo>? = null
    private var attachment = 0L

    fun attach(activity: Activity, view: WebView) {
        detach()
        web = view
        view.viewTreeObserver.addOnGlobalLayoutListener(changes)
        val attached = attachment
        val listener = Consumer<WindowLayoutInfo> { value ->
            if (attachment == attached && web === view) {
                info = value
                refresh()
            }
        }
        consumer = listener
        try {
            tracker = WindowInfoTrackerCallbackAdapter(WindowInfoTracker.getOrCreate(activity)).also {
                it.addWindowLayoutInfoListener(activity, ContextCompat.getMainExecutor(activity), listener)
            }
        } catch (_: Exception) {
            tracker = null
        }
        refresh()
    }

    fun detach() {
        attachment++
        consumer?.let { tracker?.removeWindowLayoutInfoListener(it) }
        consumer = null
        tracker = null
        web?.viewTreeObserver?.takeIf { it.isAlive }?.removeOnGlobalLayoutListener(changes)
        web = null
        info = null
        latest = unavailable()
        revision++
        // An old WebView's asynchronous callback cannot block its replacement.
        busy = false
        queued = false
        finish(latest)
    }

    fun snapshot(reply: (JSObject) -> Unit) {
        waiting.add(reply)
        refresh()
    }

    fun replay() {
        lastSent = null
        refresh()
    }

    fun refresh() {
        revision++
        if (busy) {
            queued = true
            return
        }
        val view = web
        if (view == null || view.width <= 0 || view.height <= 0) {
            publish(unavailable())
            return
        }
        busy = true
        queued = false
        val expected = revision
        val attached = attachment
        val location = IntArray(2)
        view.getLocationInWindow(location)
        val physicalWidth = view.width.toDouble()
        val physicalHeight = view.height.toDouble()
        val features = info?.displayFeatures?.filterIsInstance<FoldingFeature>()
        view.evaluateJavascript(VIEWPORT) { raw ->
            if (view !== web || attached != attachment) return@evaluateJavascript
            busy = false
            // A rotation/layout/hinge update during evaluation invalidates both
            // the native geometry and the sampled JavaScript viewport.
            if (expected != revision || queued) {
                refresh()
                return@evaluateJavascript
            }
            val value = runCatching {
                val css = JSONObject(raw)
                val width = css.getDouble("width")
                val height = css.getDouble("height")
                val visibleWidth = css.getDouble("visibleWidth")
                val visibleHeight = css.getDouble("visibleHeight")
                check(width > 0 && height > 0 && visibleWidth > 0 && visibleHeight > 0)
                val coordinates = WindowCoordinates(location[0], location[1], physicalWidth, physicalHeight,
                    visibleWidth, visibleHeight, css.getDouble("offsetX"), css.getDouble("offsetY"))
                val folds = JSONArray()
                features?.forEach { fold ->
                    val b = fold.bounds
                    folds.put(JSONObject()
                        .put("bounds", JSONObject()
                            .put("x", coordinates.x(b.left))
                            .put("y", coordinates.y(b.top))
                            .put("width", coordinates.width(b.width()))
                            .put("height", coordinates.height(b.height())))
                        .put("orientation", if (fold.orientation == FoldingFeature.Orientation.HORIZONTAL) "horizontal" else "vertical")
                        .put("state", if (fold.state == FoldingFeature.State.FLAT) "flat" else "half-opened")
                        .put("separating", fold.isSeparating)
                        .put("occlusion", if (fold.occlusionType == FoldingFeature.OcclusionType.FULL) "full" else "none"))
                }
                JSObject().put("availability", if (features == null) "unavailable" else "available")
                    .put("width", width).put("height", height).put("folds", folds) as JSObject
            }.getOrElse { unavailable() }
            publish(value)
        }
    }

    private fun publish(value: JSObject) {
        latest = value
        val encoded = value.toString()
        if (encoded != lastSent) {
            lastSent = encoded
            emit(JSObject().put("type", "windowLayoutChanged").put("layout", value) as JSObject)
        }
        finish(value)
    }

    private fun finish(value: JSObject) {
        val replies = waiting.toList()
        waiting.clear()
        replies.forEach { it(value) }
    }

    companion object {
        private fun unavailable() = JSObject().put("availability", "unavailable")
            .put("width", 0).put("height", 0).put("folds", JSONArray()) as JSObject
        private const val VIEWPORT = """(() => {
            const v = window.visualViewport;
            return {width: innerWidth, height: innerHeight,
                visibleWidth: v ? v.width : innerWidth,
                visibleHeight: v ? v.height : innerHeight,
                offsetX: v ? v.offsetLeft : 0, offsetY: v ? v.offsetTop : 0};
        })()"""
    }
}
