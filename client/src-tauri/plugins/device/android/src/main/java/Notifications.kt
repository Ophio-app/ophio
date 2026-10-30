// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.RemoteInput
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.graphics.drawable.Icon
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.SystemClock
import java.util.UUID
import org.json.JSONArray
import org.json.JSONObject

internal class Notifications(
    private val context: Context,
    /** What a tap opens: the app. Checks open something else. */
    private val launch: () -> Intent? = {
        context.packageManager.getLaunchIntentForPackage(context.packageName)
    },
) {
    private val manager = context.getSystemService(NotificationManager::class.java)
    private val routes = Routes(context.getSharedPreferences("notification-routes", Context.MODE_PRIVATE))

    init {
        // One channel each, so a person can silence finished tasks and keep
        // questions. The ones before them are gone, as is the background
        // service's notice: the service no longer shows one.
        manager.deleteNotificationChannel("attention")
        manager.deleteNotificationChannel("connection")
        manager.deleteNotificationChannel("background")
        manager.createNotificationChannel(
            NotificationChannel(NEEDS_YOU, "Needs you", NotificationManager.IMPORTANCE_HIGH)
        )
        manager.createNotificationChannel(
            NotificationChannel(FINISHED, "Finished tasks", NotificationManager.IMPORTANCE_DEFAULT)
        )
    }

    companion object {
        const val NEEDS_YOU = "needs-you"
        const val FINISHED = "finished"
        /** As the watcher's MAX_SHOWN: the most alerts kept in the shade. */
        const val MAX_SHOWN = 24
        /** The watcher's note of how many alerts wait for room, which takes none. */
        const val MORE = "more"
        /** An alert's text while app lock is on. */
        const val LOCKED = "Open the app to review."
        /** A question's alert while its answer is on its way. */
        const val SENDING = "Sending your answer…"
        /** After a computer's prefix, the note that counts a project's alerts, as the watcher's. */
        const val FOLDED = "/project:"
        /** Marks a note whose title names its project, which app lock hides. */
        private const val NAMED = "app.ophio.named"
        /** Where a question's answer goes, kept in its alert's extras. */
        internal const val REPLY = "app.ophio.reply"
        /** The alert an answer is to, and the answer, in an answer's intent. */
        internal const val TAG = "tag"
        internal const val ANSWER = "answer"
        /** The most choices offered as buttons: with Answer, all Android shows. */
        private const val BUTTONS = 2
        private const val ALERT = 1
        private const val SUMMARY = 2
        /** How long Android may take to show an update, in ms. */
        private const val SETTLE = 1_000L
        /** Between updates, in ms: Android sheds an app's beyond about five a second. */
        private const val PACE = 250L
        /**
         * Alerts just posted or cleared, by the app and the watcher alike,
         * each from its own instance: they take room, and count in their
         * groups, in turn.
         */
        private val posting = Shown()
        private val clearing = Clears()
        /** The alerts showing, as last posted here, which Android may not list yet. */
        private val notes = HashMap<String, Notification>()
        /** Redactions asked for since the one running began: one runs at a time. */
        private val redactions = java.util.concurrent.atomic.AtomicInteger()

        /** `note` with its text hidden, and a question's answers, replacing it quietly. */
        internal fun hidden(context: Context, note: Notification): Notification =
            rebuild(context, note, bare = true)
                .setContentText(LOCKED)
                .setStyle(Notification.BigTextStyle().bigText(LOCKED))
                .setOnlyAlertOnce(true)
                .apply { if (note.extras.getBoolean(NAMED)) setContentTitle("Øphio") }
                .build()

        /**
         * A builder for a copy of `note`, without its actions when `bare`.
         * Android's builder takes over the notification it is given, and
         * keeps its actions when given none.
         */
        private fun rebuild(context: Context, note: Notification, bare: Boolean = false): Notification.Builder =
            Notification.Builder.recoverBuilder(context, note.clone().apply { if (bare) actions = null })
    }

    enum class Posted { SHOWN, FULL, CLEARED }

    fun enabled(): Boolean = manager.areNotificationsEnabled()

    /** Whether alerts in a category can show: the app's and the category's switch. */
    fun channelOn(id: String): Boolean =
        manager.areNotificationsEnabled() &&
            (manager.getNotificationChannel(id)?.importance ?: NotificationManager.IMPORTANCE_NONE) !=
                NotificationManager.IMPORTANCE_NONE

    private fun locked(): Boolean =
        context.getSharedPreferences("app-lock", Context.MODE_PRIVATE).getBoolean("enabled", false)

    /** Where clearing stands now: a post asked for here is refused once its alert is cleared. */
    fun clears(): Long = synchronized(posting) { clearing.count }

    /**
     * Shows an alert, or replaces the one with its tag. FULL, showing
     * nothing, when the shade has no room: Android silently drops an app's
     * alerts beyond about 50. CLEARED, showing nothing, when the alert was
     * cleared, as answered, after `since`: the count of clears when its
     * poster learned of it. An agent's question, `asking`, shows its words
     * and takes an answer, unless app lock is on.
     */
    fun show(
        id: String,
        title: String,
        body: String,
        route: Any?,
        category: String? = null,
        group: String? = null,
        groupTitle: String? = null,
        since: Long? = null,
        asking: Asking? = null,
    ): Posted {
        require(id.length <= 256 && title.length <= 256 && body.length <= 2048)
        require((group?.length ?: 0) <= 256 && (groupTitle?.length ?: 0) <= 256)
        require(asking == null || asking.text.length <= 2048 && asking.reply.length <= 4096)
        require(asking == null || asking.choices.size <= 8 && asking.choices.all { it.length <= 64 })
        val channel = if (category == "finished") FINISHED else NEEDS_YOU
        val note =
            Notification.Builder(context, channel)
                .setSmallIcon(R.drawable.ic_stat_ophio)
                .setContentTitle(title)
                .setVisibility(Notification.VISIBILITY_PRIVATE)
                .setAutoCancel(true)
                // The app and the watcher can each raise the same alert.
                .setOnlyAlertOnce(true)
                .apply { if (group != null) setGroup(group) }
        // Posting and clearing take turns, from the app and the watcher
        // alike, so a clear is never overtaken by the post it answers.
        synchronized(posting) {
            if (since != null && clearing.refuses(id, since)) return Posted.CLEARED
            val shown = showing().keys
            if (id != MORE && id !in shown && shown.count { it != MORE } >= MAX_SHOWN) return Posted.FULL
            // Read in turn with redact(): app lock turned on before this
            // hides the text here, and after it, there.
            val locked = locked()
            // A project's note names it in its title.
            if (FOLDED in id) {
                note.addExtras(Bundle().apply { putBoolean(NAMED, true) })
                if (locked) note.setContentTitle("Øphio")
            }
            val text = if (locked) LOCKED else asking?.text ?: body
            val more = if (locked || asking == null) text else "$text\n\n$body"
            note.setContentText(text).setStyle(Notification.BigTextStyle().bigText(more))
            // Answered from here only while its words show.
            if (asking != null && !locked) answers(note, id, asking)
            // Only an alert shown gets a route, kept while it shows.
            note.setContentIntent(opener(id, route, shown))
            posting.post(id, group, SystemClock.elapsedRealtime())
            val built = note.build()
            notes.keys.retainAll(shown)
            notes[id] = built
            manager.notify(id, ALERT, built)
            if (group != null) summarize(group, groupTitle, grouped(group))
        }
        return Posted.SHOWN
    }

    /**
     * A question's actions: a button for each of a few short choices, and a
     * reply box offering any others. Each asks for the phone to be unlocked
     * first, from Android 12; before that, choices are offered only in the
     * reply box, which Android opens only on an unlocked phone. The answer
     * goes to [Answers], which finds where to in the alert itself.
     */
    private fun answers(note: Notification.Builder, id: String, asking: Asking) {
        val guarded = Build.VERSION.SDK_INT >= Build.VERSION_CODES.S
        val buttons = if (guarded && asking.choices.size <= BUTTONS) asking.choices else emptyList()
        buttons.forEachIndexed { i, choice ->
            val chosen = answering(id, i).putExtra(ANSWER, choice)
            val sent = PendingIntent.getBroadcast(
                context, 0, chosen, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
            note.addAction(action(choice, sent).build())
        }
        val box = RemoteInput.Builder(ANSWER)
            .setLabel("Your answer")
            .apply { if (buttons.isEmpty() && asking.choices.isNotEmpty()) setChoices(asking.choices.toTypedArray()) }
            .build()
        // Android adds what was written, so this one alone stays mutable.
        val mutable = if (guarded) PendingIntent.FLAG_MUTABLE else 0
        val sent = PendingIntent.getBroadcast(
            context, 0, answering(id, buttons.size), PendingIntent.FLAG_UPDATE_CURRENT or mutable,
        )
        note.addAction(action("Answer", sent).addRemoteInput(box).build())
            // Android's own suggested replies would answer for the person.
            .setAllowSystemGeneratedContextualActions(false)
            .addExtras(Bundle().apply { putString(REPLY, asking.reply) })
    }

    private fun action(label: String, sent: PendingIntent): Notification.Action.Builder =
        Notification.Action.Builder(null as Icon?, label, sent).apply {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) setAuthenticationRequired(true)
        }

    /** The `n`th of alert `id`'s answers, each its own to Android. */
    private fun answering(id: String, n: Int): Intent =
        Intent(context, Answers::class.java).setIdentifier("$id#$n").putExtra(TAG, id)

    /**
     * Says the answer to question `tag` is on its way, its actions gone so
     * it goes once. Returns the alert as it was, to ask again should the
     * answer not go; null when it no longer shows or takes no answer, as
     * while one is on its way.
     */
    fun sending(tag: String): Notification? =
        synchronized(posting) {
            val asked = current(tag) ?: return null
            if (asked.actions.isNullOrEmpty()) return null
            val note =
                rebuild(context, asked, bare = true)
                    .setContentText(SENDING)
                    .setStyle(Notification.BigTextStyle().bigText(SENDING))
                    .setOnlyAlertOnce(true)
                    .build()
            notes[tag] = note
            manager.notify(tag, ALERT, note)
            asked
        }

    /**
     * The answer to question `tag` did not go: it asks again as `asked`,
     * saying why. The agent still waits, so it alerts as a new one would.
     */
    fun unsent(tag: String, asked: Notification, why: String) {
        synchronized(posting) {
            // Answered or cleared meanwhile, it stays gone.
            if (current(tag) == null) return
            val note =
                if (locked()) {
                    hidden(context, asked)
                } else {
                    val said = "Not sent: $why"
                    val words = asked.extras.getCharSequence(Notification.EXTRA_BIG_TEXT) ?: ""
                    rebuild(context, asked)
                        .setContentText(said)
                        .setStyle(Notification.BigTextStyle().bigText("$said\n\n$words"))
                        .setOnlyAlertOnce(false)
                        .build()
                }
            notes[tag] = note
            manager.notify(tag, ALERT, note)
        }
    }

    /** Alert `tag` as it shows, or as just posted to; null when it doesn't. */
    private fun current(tag: String): Notification? =
        if (tag in showing()) {
            manager.activeNotifications.firstOrNull { it.id == ALERT && it.tag == tag }?.notification ?: notes[tag]
        } else {
            null
        }

    /** Several finished tasks from one computer stack under its name, with their count. */
    private fun summarize(group: String, groupTitle: String?, children: Int) {
        if (children < 2) {
            manager.cancel("summary:$group", SUMMARY)
            return
        }
        val title = if (locked()) "Øphio" else groupTitle ?: "Øphio"
        val summary =
            Notification.Builder(context, FINISHED)
                .setSmallIcon(R.drawable.ic_stat_ophio)
                .setContentTitle(title)
                .setContentText("$children tasks finished")
                .setVisibility(Notification.VISIBILITY_PRIVATE)
                .setGroup(group)
                .setGroupSummary(true)
                .setGroupAlertBehavior(Notification.GROUP_ALERT_CHILDREN)
                .setAutoCancel(true)
                .build()
        manager.notify("summary:$group", SUMMARY, summary)
    }

    private fun grouped(group: String): Int = showing().values.count { it == group }

    /**
     * Hides the text of the alerts showing, and the computer's name on their
     * groups' summaries, as app lock asks of every alert from now on. Each is
     * replaced quietly, in turn with posting, so none built before app lock
     * was turned on keeps its details. Android sheds an app's updates beyond
     * about five a second, so they are paced, and those shed are tried once
     * more; one still showing its details then goes. Blocks meanwhile, but
     * one asked for while another runs makes that one go again instead, so
     * the app, the watcher and a later check never double the pace.
     */
    fun redact() {
        if (redactions.getAndIncrement() > 0) return
        try {
            do {
                redactions.set(1)
                pass()
            } while (redactions.decrementAndGet() > 0)
        } catch (e: Throwable) {
            redactions.set(0)
            throw e
        }
    }

    private fun pass() {
        repeat(2) {
            val (alerts, groups) = synchronized(posting) { if (!locked()) return; detailed() }
            if (alerts.isEmpty() && groups.isEmpty()) return
            for (group in groups) paced { summarize(group, null, grouped(group)) }
            for (tag in alerts) paced {
                val listed = manager.activeNotifications.firstOrNull { it.id == ALERT && it.tag == tag }
                // Gone meanwhile, as answered, it stays gone.
                val shown = if (tag in showing()) listed?.notification ?: notes[tag] else null
                if (shown != null) {
                    val note = hidden(context, shown)
                    notes[tag] = note
                    manager.notify(tag, ALERT, note)
                }
            }
            Thread.sleep(SETTLE)
        }
        synchronized(posting) {
            if (!locked()) return
            val (alerts, groups) = detailed()
            for (tag in alerts) cancel(tag)
            for (group in groups) manager.cancel("summary:$group", SUMMARY)
        }
    }

    /** One step of [redact], in turn with posting, then a pause. */
    private inline fun paced(step: () -> Unit) {
        synchronized(posting) { if (locked()) step() }
        Thread.sleep(PACE)
    }

    /**
     * The alerts showing their text, as Android shows them or as just posted
     * here, and the groups whose summaries show a computer's name.
     */
    private fun detailed(): Pair<List<String>, List<String>> {
        val listed = manager.activeNotifications
        val showing = showing()
        notes.keys.retainAll(showing.keys)
        fun text(n: Notification?, key: String) = n?.extras?.getCharSequence(key)?.toString()
        val alerts = showing.keys.filter { tag ->
            val shown = listed.firstOrNull { it.id == ALERT && it.tag == tag }?.notification ?: notes[tag]
            shown != null && text(shown, Notification.EXTRA_TEXT) != LOCKED
        }
        val groups = listed.filter { it.id == SUMMARY && text(it.notification, Notification.EXTRA_TITLE) != "Øphio" }
            .mapNotNull { it.tag?.removePrefix("summary:") }
        return alerts to groups
    }

    fun cancel(tag: String) {
        synchronized(posting) {
            clearing.clear(tag)
            notes.remove(tag)
            val showing = manager.activeNotifications
            val group =
                posting.clear(tag, SystemClock.elapsedRealtime())
                    ?: showing.firstOrNull { it.id == ALERT && it.tag == tag }?.notification?.group
            manager.cancel(tag, ALERT)
            if (group == null) return
            // The summary keeps the computer's name it was given.
            val title =
                showing
                    .firstOrNull { it.id == SUMMARY && it.tag == "summary:$group" }
                    ?.notification
                    ?.extras
                    ?.getCharSequence(Notification.EXTRA_TITLE)
                    ?.toString()
            summarize(group, title, grouped(group))
        }
    }

    /** The tags of the alerts showing now, or just posted to show. */
    fun shown(): List<String> = synchronized(posting) { showing().keys.toList() }

    /** The alerts showing, by tag, with their groups. */
    private fun showing(): Map<String, String?> =
        posting.showing(
            manager.activeNotifications
                .filter {
                    it.id == ALERT &&
                        it.tag != null &&
                        it.notification.flags and Notification.FLAG_GROUP_SUMMARY == 0
                }
                .associate { it.tag to it.notification.group },
            SystemClock.elapsedRealtime(),
        )

    private fun opener(id: String, route: Any?, showing: Collection<String>): PendingIntent {
        val intent = launch() ?: error("The app cannot be opened.")
        val token = routes.add(id, route, showing)
        intent.data = Uri.parse("notification://open/$token")
        intent.putExtra("notificationToken", token)
        intent.addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        return PendingIntent.getActivity(
            context,
            0,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    fun accept(intent: Intent) {
        val token = intent.getStringExtra("notificationToken") ?: return
        intent.removeExtra("notificationToken")
        routes.open(token)
    }

    fun takeOpen(): JSONObject? = routes.takeOpen()
}

/** An agent's question as its alert asks it; `reply`, as JSON, says where its answer goes. */
internal class Asking(val text: String, val choices: List<String>, val reply: String) {
    companion object {
        fun of(question: JSONObject): Asking {
            val choices = question.optJSONArray("choices") ?: JSONArray()
            return Asking(
                question.getString("text"),
                (0 until choices.length()).map { choices.getString(it) },
                question.getJSONObject("reply").toString(),
            )
        }
    }
}

/**
 * An answer from a question's alert: a choice tapped, or words written in
 * its reply box. Taken by the computer, the alert clears; otherwise it asks
 * again, saying why. Only Android sends these, by the alert's own intents.
 */
class Answers : android.content.BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val tag = intent.getStringExtra(Notifications.TAG) ?: return
        val text =
            intent.getStringExtra(Notifications.ANSWER)
                ?: RemoteInput.getResultsFromIntent(intent)?.getCharSequence(Notifications.ANSWER)?.toString()
                ?: return
        val app = context.applicationContext
        val pending = goAsync()
        Thread {
            try {
                AlertWatcher.reply(app, tag, text)
            } catch (_: Throwable) {
            } finally {
                pending.finish()
            }
        }.start()
    }
}

/**
 * The alerts showing, as Android will list them. Android posts and clears
 * on its own thread: an alert just posted is listed only once it shows, and
 * one just cleared until it goes. Each is counted here meanwhile, for up to
 * `wait` ms. The caller takes turns.
 */
internal class Shown(private val wait: Long = 5_000L) {
    private val posted = HashMap<String, Pair<Long, String?>>()
    private val cleared = HashMap<String, Long>()

    fun post(tag: String, group: String?, now: Long) {
        cleared.remove(tag)
        posted[tag] = now to group
    }

    /** Returns the group of `tag` if it was just posted. */
    fun clear(tag: String, now: Long): String? {
        cleared[tag] = now
        return posted.remove(tag)?.second
    }

    /** Of `listed`, Android's alerts with their groups, those showing, and those just posted. */
    fun showing(listed: Map<String, String?>, now: Long): Map<String, String?> {
        posted.entries.removeAll { it.key in listed || now - it.value.first > wait }
        cleared.entries.removeAll { now - it.value > wait }
        return listed.filterKeys { it !in cleared } + posted.mapValues { it.value.second }
    }
}

/**
 * Alerts cleared so far, as answered, and the count at which each tag last
 * was. A post asked for at one count, as its alert was learned of, is
 * refused once its alert is cleared after it, and so is one older than the
 * clears kept, whose alert may have been. One asked for after its clear is
 * posted: the computer still listed it open then. The caller takes turns.
 */
internal class Clears(private val keep: Int = 512) {
    var count = 0L
        private set
    /** The count of the latest clear no longer kept. */
    private var forgotten = 0L
    private val cleared = LinkedHashMap<String, Long>()

    fun clear(tag: String) {
        // Kept in the order cleared, so the oldest goes first.
        cleared.remove(tag)
        cleared[tag] = ++count
        if (cleared.size > keep) {
            val oldest = cleared.entries.first()
            forgotten = oldest.value
            cleared.remove(oldest.key)
        }
    }

    /** Whether `tag`, asked for at `since`, was or may have been cleared since. */
    fun refuses(tag: String, since: Long): Boolean =
        since < forgotten || (cleared[tag] ?: Long.MIN_VALUE) > since
}

/**
 * Where each alert's tap leads, by an unguessable one-use token, so an
 * exported launch intent cannot inject a route or trigger a command. An
 * alert has one at a time; past the most kept, the oldest of alerts no
 * longer showing go. The app's and the watcher's share the one store.
 */
internal class Routes(private val store: SharedPreferences, private val keep: Int = 128) {
    /** The token for alert `id`, which shows from now along with `showing`. */
    fun add(id: String, route: Any?, showing: Collection<String>): String {
        val payload = JSONObject().put("id", id).put("route", route ?: JSONObject.NULL).toString()
        require(payload.length <= 16384)
        val leads = JSONObject(payload).opt("route")
        // Ordered by when it was made; the rest is unguessable.
        val token = "%013d-%s".format(System.currentTimeMillis(), UUID.randomUUID())
        synchronized(store) {
            val editor = store.edit()
            val others = ArrayList<Pair<String, String?>>()
            var kept: String? = null
            for ((key, value) in store.all) {
                if (key == OPENED) continue
                val earlier = runCatching { JSONObject(value as String) }.getOrNull()
                val tag = earlier?.optString("id")
                if (tag != id) {
                    others.add(key to tag)
                // Posted again to the same place, as by the app and the
                // watcher both, it keeps its token, so a tap on its way
                // still opens it. Replaced, it takes its earlier route with it.
                } else if (kept == null && same(earlier.opt("route"), leads)) {
                    kept = key
                } else {
                    editor.remove(key)
                }
            }
            if (kept != null) {
                editor.commit()
                return kept
            }
            val over = others.size + 1 - keep
            if (over > 0) {
                others
                    .filter { it.second !in showing }
                    .sortedBy { it.first }
                    .take(over)
                    .forEach { editor.remove(it.first) }
            }
            editor.putString(token, payload).commit()
        }
        return token
    }

    /** The alert with `token` was tapped: its route waits for the app. */
    fun open(token: String) {
        synchronized(store) {
            val payload = store.getString(token, null) ?: return
            store.edit().remove(token).putString(OPENED, payload).commit()
        }
    }

    fun takeOpen(): JSONObject? =
        synchronized(store) {
            val value = store.getString(OPENED, null) ?: return null
            store.edit().remove(OPENED).commit()
            JSONObject(value)
        }

    private companion object {
        const val OPENED = "opened"

        /** Whether two routes say the same, in whatever order their keys were written. */
        fun same(a: Any?, b: Any?): Boolean =
            when {
                a is JSONObject && b is JSONObject ->
                    a.length() == b.length() &&
                        a.keys().asSequence().all { b.has(it) && same(a.opt(it), b.opt(it)) }
                a is JSONArray && b is JSONArray ->
                    a.length() == b.length() && (0 until a.length()).all { same(a.opt(it), b.opt(it)) }
                else -> a == b
            }
    }
}
