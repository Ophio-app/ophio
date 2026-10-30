// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.app.Activity
import android.content.res.Configuration
import android.graphics.Color
import android.view.ViewGroup
import android.view.WindowManager
import android.webkit.WebSettings
import android.webkit.WebView
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import kotlin.math.roundToInt

internal class Shell(private val activity: Activity, private val web: WebView) {
    private var insets: WindowInsetsCompat? = null
    private var previousKeyboard = 0
    private var immersed = false

    init {
        WindowCompat.setDecorFitsSystemWindows(activity.window, false)
        activity.window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE)
        web.settings.apply {
            allowFileAccess = false
            allowContentAccess = false
            javaScriptCanOpenWindowsAutomatically = false
            mediaPlaybackRequiresUserGesture = true
            mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
            setGeolocationEnabled(false)
        }
        ViewCompat.setOnApplyWindowInsetsListener(web) { _, next ->
            insets = next
            apply()
            next
        }
        configure(activity.resources.configuration)
        ViewCompat.requestApplyInsets(web)
    }

    fun configure(config: Configuration) {
        web.settings.textZoom = (config.fontScale * 100).roundToInt().coerceIn(50, 300)
        theme(
            (config.uiMode and Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES
        )
        if (immersed) immersive(true)
        apply()
    }

    @Suppress("DEPRECATION")
    fun theme(dark: Boolean) {
        // Transparent bars let the existing app surface show through.
        activity.window.statusBarColor = Color.TRANSPARENT
        activity.window.navigationBarColor = Color.TRANSPARENT
        activity.window.isNavigationBarContrastEnforced = false
        val color =
            activity.resources.getIdentifier(
                if (dark) "app_background_dark" else "app_background_light",
                "color",
                activity.packageName,
            )
        if (color != 0) activity.window.setBackgroundDrawableResource(color)
        WindowCompat.getInsetsController(activity.window, web).apply {
            isAppearanceLightStatusBars = !dark
            isAppearanceLightNavigationBars = !dark
        }
    }

    fun apply() {
        val i = insets ?: ViewCompat.getRootWindowInsets(web) ?: return
        val bars =
            i.getInsets(
                WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()
            )
        val keyboard = i.getInsets(WindowInsetsCompat.Type.ime()).bottom
        val revealFocusedField = keyboard > 0 && keyboard != previousKeyboard
        previousKeyboard = keyboard
        // Resize the native WebView itself, so 100dvh, sticky composers and
        // sheets all agree on the available height while the IME is visible.
        (web.layoutParams as? ViewGroup.MarginLayoutParams)?.let { p ->
            val bottom = maxOf(keyboard, bars.bottom)
            if (
                p.leftMargin != bars.left ||
                    p.topMargin != bars.top ||
                    p.rightMargin != bars.right ||
                    p.bottomMargin != bottom
            ) {
                p.setMargins(bars.left, bars.top, bars.right, bottom)
                web.layoutParams = p
            }
        }
        // The viewport is physically inset, so none of its CSS pixels are
        // obscured. This also prevents a scrolled page from painting text
        // behind the transparent system bars.
        val js =
            """(()=>{const s=document.documentElement.style;for(const edge of ['top','bottom','left','right'])s.setProperty('--safe-'+edge,'0px');})()"""
        web.evaluateJavascript(js, null)
        if (revealFocusedField)
            web.post {
                // Reflow completes after Android has resized the native view.
                // Center a focused field so a sticky action footer cannot hide it.
                web.evaluateJavascript(
                    """requestAnimationFrame(()=>requestAnimationFrame(()=>{const e=document.activeElement;if(e&&(e.tagName==='INPUT'||e.tagName==='TEXTAREA'||e.isContentEditable))e.scrollIntoView({block:'center',inline:'nearest'});}));""",
                    null,
                )
            }
    }

    /** A display shown full screen: the status and navigation bars step aside,
     * and a swipe from an edge shows them for a moment. */
    fun immersive(on: Boolean) {
        immersed = on
        WindowCompat.getInsetsController(activity.window, web).apply {
            if (on) {
                systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
                hide(WindowInsetsCompat.Type.systemBars())
            } else show(WindowInsetsCompat.Type.systemBars())
        }
    }

    fun awake(on: Boolean) {
        web.keepScreenOn = on
    }
}
