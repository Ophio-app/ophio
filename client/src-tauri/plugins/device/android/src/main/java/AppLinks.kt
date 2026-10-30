// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.content.Intent

/** Identifiers carry no authorization. */
internal const val OBJECT_LINK_PREFIX = "ophio-object://open/v1/"
/** Links made before the product was named, which may sit in notes and chats. */
internal const val EARLIER_OBJECT_LINK_PREFIX = "companion-object://open/v1/"
/**
 * A computer's pairing offer, as `ophio setup` opens the app with it over
 * Wireless debugging. Only an intent naming this app reaches it: no intent
 * filter lists the scheme, so no web page or browser can open one. Another
 * app on the phone could still name it, so the app says a link opened the
 * pairing, and pairs only once the person checks the fingerprint and taps Pair.
 */
internal const val PAIRING_LINK_PREFIX = "ophio://pair?"
internal const val CONTRIBUTION_LINK_PREFIX = "ophio-contribution://open/v1?"

internal class AppLinks {
    private val pending = java.util.ArrayDeque<String>()
    fun accept(intent: Intent) {
        if (intent.action != Intent.ACTION_VIEW) return
        val url = intent.dataString ?: return
        val contribution = url.startsWith(CONTRIBUTION_LINK_PREFIX)
        if (url.length > (if (contribution) 96 * 1024 else 4096) ||
            (!contribution && listOf(OBJECT_LINK_PREFIX, EARLIER_OBJECT_LINK_PREFIX, PAIRING_LINK_PREFIX).none { url.startsWith(it) })) return
        if (pending.size >= 8) pending.removeFirst()
        pending.addLast(url)
        // Avoid replaying the launch intent when the Activity is recreated.
        intent.data = null
    }
    fun drain(): List<String> = pending.toList().also { pending.clear() }
}
