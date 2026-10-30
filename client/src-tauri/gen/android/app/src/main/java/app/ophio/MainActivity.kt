// SPDX-License-Identifier: Apache-2.0
package app.ophio

import android.os.Bundle
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge
import app.ophio.device.DevicePlugin

class MainActivity : TauriActivity() {
    override val handleBackNavigation: Boolean = false

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
    }

    override fun onWebViewCreate(webView: WebView) {
        super.onWebViewCreate(webView)
        DevicePlugin.webViewCreated(this, webView)
        onBackPressedDispatcher.addCallback(
            this,
            object : OnBackPressedCallback(true) {
                override fun handleOnBackPressed() {
                    // The app closes sheets and panels, pops screen stacks and
                    // returns to Talk, and says whether it did. WebView history
                    // is not asked: it skips entries added without a tap.
                    // Otherwise leave the task using normal Android behavior;
                    // retain the runtime for an explicitly enabled watching service.
                    webView.evaluateJavascript("window.systemBack?.() === true") { handled ->
                        if (handled != "true") moveTaskToBack(true)
                    }
                }
            },
        )
    }
}
