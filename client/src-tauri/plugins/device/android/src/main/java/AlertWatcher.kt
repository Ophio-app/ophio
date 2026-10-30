// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.content.Context
import org.json.JSONObject

/**
 * The bridge to the watcher in Rust, which keeps its own connection to each
 * paired computer while the app is closed. Rust calls back into the static
 * methods here; each catches its own failures, so none reaches Rust as an
 * exception.
 */
object AlertWatcher {
    init {
        System.loadLibrary("phone_shell")
    }

    /** Whether the app's screen is in front and unlocked, so shows alerts itself. */
    @Volatile var screen = false
    @Volatile private var notifications: Notifications? = null
    @Volatile private var context: Context? = null

    fun attach(context: Context) {
        if (notifications != null) return
        this.context = context.applicationContext
        val shade = Notifications(context.applicationContext)
        notifications = shade
        // App lock turned on just before the app's last process ended may
        // have left alerts showing their details: they are hidden now.
        Thread { runCatching { shade.redact() } }.start()
    }

    /**
     * Starts watching, with the app's files under `dataDir`. `cleared` changes
     * whenever Android clears the app's alerts, which are then raised again.
     */
    @JvmStatic external fun start(dataDir: String, cleared: String): Boolean

    @JvmStatic external fun stop()

    /** The network changed: reconnect on the new one. */
    @JvmStatic external fun nudge()

    /** The app opened: try a computer waiting to be retried now. */
    @JvmStatic external fun retry()

    @JvmStatic external fun running(): Boolean

    /**
     * Sends `text` as the answer to the question named by `reply`, a JSON
     * object, and waits for the computer to take it: null once taken,
     * otherwise why not. Blocks for up to about 20 seconds.
     */
    @JvmStatic external fun answer(dataDir: String, reply: String, text: String): String?

    /**
     * Answers the question alert `tag` with `text`, to where the alert says,
     * as [Answers] asks: taken, the alert clears; otherwise it asks again,
     * saying why. Blocks meanwhile.
     */
    fun reply(context: Context, tag: String, text: String) {
        attach(context)
        val shade = notifications ?: return
        val asked = shade.sending(tag) ?: return
        val reply = asked.extras.getString(Notifications.REPLY)
        val why =
            if (reply == null) {
                "Open the app to answer."
            } else {
                try {
                    answer(context.dataDir.absolutePath, reply, text)
                } catch (_: Throwable) {
                    "The phone could not send it."
                }
            }
        if (why == null) shade.cancel(tag) else shade.unsent(tag, asked, why)
    }

    @JvmStatic
    fun sign(id: String, transcript: ByteArray): ByteArray? =
        try {
            Keys.decode(Keys.sign(id, transcript))
        } catch (_: Exception) {
            null
        }

    @JvmStatic
    fun unwrap(id: String, sealed: String): ByteArray? =
        try {
            Keys.unwrap(id, Keys.decode(sealed))
        } catch (_: Exception) {
            null
        }

    /**
     * 0 when shown, 1 when the shade has no room, 2 when it failed, 3 when
     * the alert was cleared, as answered, after the count of clears was
     * `since`, when the watcher learned of it, or may have been once that
     * is no longer kept. `since` is negative for the watcher's own note.
     */
    @JvmStatic
    fun post(json: String, since: Long): Int {
        try {
            val note = JSONObject(json)
            val shade = notifications ?: return 2
            if (!shade.enabled()) return 2
            val posted = shade.show(
                note.getString("tag"),
                note.getString("title"),
                note.getString("body"),
                note.opt("route"),
                note.getString("category"),
                note.optString("group").takeIf { !note.isNull("group") },
                note.optString("groupTitle").takeIf { !note.isNull("groupTitle") },
                since.takeIf { it >= 0 },
                note.optJSONObject("question")?.let(Asking::of),
            )
            return when (posted) {
                Notifications.Posted.SHOWN -> 0
                Notifications.Posted.FULL -> 1
                Notifications.Posted.CLEARED -> 3
            }
        } catch (_: Exception) {
            return 2
        }
    }

    @JvmStatic
    fun cancel(tag: String) {
        try {
            notifications?.cancel(tag)
        } catch (_: Exception) {}
    }

    /** How many alerts the app and the watcher have cleared; -1 when unknown. */
    @JvmStatic
    fun clears(): Long =
        try {
            notifications?.clears() ?: -1
        } catch (_: Exception) {
            -1
        }

    @JvmStatic
    fun shown(): Array<String> =
        try {
            notifications?.shown()?.toTypedArray() ?: emptyArray()
        } catch (_: Exception) {
            emptyArray()
        }

    @JvmStatic fun onScreen(): Boolean = screen

    /**
     * A computer named `computer` offers the apps in `offers`, a JSON list:
     * says once that an update is ready if one of them is for this app.
     */
    @JvmStatic
    fun appUpdates(profileId: String, computer: String, offers: String) {
        val context = this.context ?: return
        // Hashing the installed app can take a moment: off the watcher's thread.
        Thread {
            try {
                AppUpdate.offered(context, profileId, computer, org.json.JSONArray(offers))
            } catch (_: Exception) {}
        }.start()
    }
}
