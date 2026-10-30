// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.app.Notification
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import org.json.JSONArray
import org.json.JSONObject

/**
 * The alert ledgers, in a scratch store, and app lock's redaction, in this
 * check app's own shade. Grant it POST_NOTIFICATIONS first.
 */
internal object AlertChecks {
    fun run(context: Context) {
        clears()
        shown()
        routes(context)
        redaction(context)
        answering(context)
    }

    private fun until(what: String, done: () -> Boolean) {
        val end = System.currentTimeMillis() + 5_000
        while (!done()) {
            check(System.currentTimeMillis() < end) { "Timed out: $what" }
            Thread.sleep(50)
        }
    }

    /**
     * An agent's question shows its words and takes an answer: short
     * choices as buttons, from Android 12, and a reply box. Sending takes
     * the answers away so it goes once; not sent, it asks again saying why;
     * app lock hides it and takes them away.
     */
    private fun answering(context: Context) {
        val lock = context.getSharedPreferences("app-lock", Context.MODE_PRIVATE)
        lock.edit().clear().commit()
        val shade = Notifications(context) { Intent(Intent.ACTION_VIEW, Uri.parse("checks://open")) }
        val manager = context.getSystemService(NotificationManager::class.java)
        val body = "Studio · ophio"
        val question = "Create the repo as private?"
        val reply = JSONObject().put("profileId", "p1").put("approvalId", "a1").put("revision", 3)
        fun asking(vararg choices: String) =
            Asking.of(JSONObject().put("text", question).put("choices", JSONArray(choices.toList())).put("reply", reply))
        fun listed(tag: String) = manager.activeNotifications.firstOrNull { it.tag == tag }?.notification
        fun text(tag: String) = listed(tag)?.extras?.getCharSequence(Notification.EXTRA_TEXT)?.toString()
        fun titles(n: Notification?) = n?.actions.orEmpty().map { it.title.toString() }
        fun offered(action: Notification.Action) = action.remoteInputs.single().choices.orEmpty().map { it.toString() }
        val tags = listOf("checks/a1", "checks/a2", "checks/a3")
        try {
            check(shade.show(tags[0], "Question from Codex", body, null, asking = asking("Yes", "No")) == Notifications.Posted.SHOWN)
            until("question shown") { listed(tags[0]) != null }
            val shown = listed(tags[0])!!
            check(text(tags[0]) == question) { "the question isn't its text" }
            check(shown.extras.getCharSequence(Notification.EXTRA_BIG_TEXT).toString() == "$question\n\n$body")
            check(JSONObject(shown.extras.getString(Notifications.REPLY)!!).getInt("revision") == 3) { "where to answer is lost" }
            check(!shown.allowSystemGeneratedContextualActions) { "Android may suggest answers" }
            val answers = shown.actions.orEmpty()
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                check(titles(shown) == listOf("Yes", "No", "Answer")) { "buttons: ${titles(shown)}" }
                check(answers.all { it.isAuthenticationRequired }) { "answered while the phone is locked" }
                check(offered(answers[2]).isEmpty())
            } else {
                check(titles(shown) == listOf("Answer")) { "buttons: ${titles(shown)}" }
                check(offered(answers[0]) == listOf("Yes", "No"))
            }

            // Past two, the choices are offered in the reply box.
            check(shade.show(tags[1], "Question from Codex", body, null, asking = asking("A", "B", "C")) == Notifications.Posted.SHOWN)
            until("choices shown") { listed(tags[1]) != null }
            check(titles(listed(tags[1])) == listOf("Answer") && offered(listed(tags[1])!!.actions[0]) == listOf("A", "B", "C"))

            // On its way, it says so, and can't be answered twice.
            val asked = shade.sending(tags[0]) ?: error("not sending")
            check(asked.actions.size == answers.size)
            check(asked.extras.getCharSequence(Notification.EXTRA_TEXT).toString() == question) { "the question as asked is lost" }
            until("sending shown") { text(tags[0]) == Notifications.SENDING }
            check(listed(tags[0])!!.actions.isNullOrEmpty())
            check(shade.sending(tags[0]) == null) { "answered twice" }
            // Not sent, it asks again, saying why, answers and all.
            shade.unsent(tags[0], asked, "It was answered or changed elsewhere.")
            until("asked again") { text(tags[0]) == "Not sent: It was answered or changed elsewhere." }
            check(
                listed(tags[0])!!.extras.getCharSequence(Notification.EXTRA_BIG_TEXT).toString() ==
                    "Not sent: It was answered or changed elsewhere.\n\n$question\n\n$body",
            ) { "asked again without the question" }
            check(titles(listed(tags[0])) == answers.map { it.title.toString() })
            // Answered elsewhere meanwhile, it stays gone.
            val again = shade.sending(tags[0]) ?: error("not sending again")
            shade.cancel(tags[0])
            shade.unsent(tags[0], again, "The computer did not answer in time.")
            Thread.sleep(500)
            check(listed(tags[0]) == null) { "raised again once answered" }

            // App lock hides a question's words and takes its answers away.
            lock.edit().putBoolean("enabled", true).commit()
            shade.redact()
            until("question hidden") { text(tags[1]) == Notifications.LOCKED }
            check(listed(tags[1])!!.actions.isNullOrEmpty()) { "answered while app lock is on" }
            check(shade.show(tags[2], "Question from Codex", body, null, asking = asking("Yes")) == Notifications.Posted.SHOWN)
            until("locked question shown") { listed(tags[2]) != null }
            check(text(tags[2]) == Notifications.LOCKED && listed(tags[2])!!.actions.isNullOrEmpty())
            android.util.Log.i("AlertChecks", "questions answered from ${titles(shown)}")
        } finally {
            for (tag in tags) shade.cancel(tag)
            lock.edit().clear().commit()
        }
    }

    /** App lock turned on hides the details of alerts already showing. */
    private fun redaction(context: Context) {
        val lock = context.getSharedPreferences("app-lock", Context.MODE_PRIVATE)
        lock.edit().clear().commit()
        val shade = Notifications(context) { Intent(Intent.ACTION_VIEW, Uri.parse("checks://open")) }
        check(shade.enabled()) { "Grant this check app POST_NOTIFICATIONS first." }
        val manager = context.getSystemService(NotificationManager::class.java)
        val group = "checks:finished"
        // A burst, as a reconnect raises, so Android may shed some updates.
        val tags = listOf("checks/1", "checks/2", "checks/3", "checks/q") + (0 until 16).map { "checks/b$it" }
        val again = (0 until 8).map { "checks/r$it" }
        // The note that counts a project's alerts names the project in its title.
        val folded = "checks/project:p"
        val jobs = context.getSystemService(android.app.job.JobScheduler::class.java)
        val body = "Studio · ophio · Fix the parser"
        fun listed() = manager.activeNotifications.filter { it.tag in tags || it.tag in again || it.tag == folded || it.tag == "summary:$group" }
        fun text(n: android.service.notification.StatusBarNotification, key: String) =
            n.notification.extras.getCharSequence(key)?.toString()
        try {
            for (tag in tags.take(3)) {
                check(shade.show(tag, "Task finished", body, null, "finished", group, "Studio") == Notifications.Posted.SHOWN) { "not shown: $tag" }
            }
            for (tag in tags.drop(3)) {
                check(shade.show(tag, "Question from Claude", body, null) == Notifications.Posted.SHOWN) { "not shown: $tag" }
            }
            until("details shown") {
                val shown = listed()
                shown.size == tags.size + 1 && shown.all { text(it, Notification.EXTRA_TEXT) == body || text(it, Notification.EXTRA_TITLE) == "Studio" }
            }
            check(shade.show(folded, "ophio has 2 notifications", body, null) == Notifications.Posted.SHOWN) { "not shown: $folded" }
            until("project's note shown") { listed().any { it.tag == folded && text(it, Notification.EXTRA_TITLE) == "ophio has 2 notifications" } }
            // A person then turns app lock on, as AppLock does: saved, then
            // the alerts showing are hidden, paced so Android sheds none.
            Thread.sleep(3_000)
            lock.edit().putBoolean("enabled", true).commit()
            shade.redact()
            until("details hidden") {
                listed().none { text(it, Notification.EXTRA_TEXT) == body || text(it, Notification.EXTRA_BIG_TEXT) == body ||
                    text(it, Notification.EXTRA_TITLE) == "Studio" }
            }
            // Hidden, not lost: each keeps its tap, quietly replaced.
            val kept = listed().filter { it.tag in tags }
            check(kept.size == tags.size) { "${tags.size - kept.size} lost" }
            for (n in kept) {
                check(text(n, Notification.EXTRA_TEXT) == Notifications.LOCKED) { "${n.tag} not hidden" }
                check(n.notification.contentIntent != null) { "${n.tag} lost its tap" }
                check(n.notification.flags and Notification.FLAG_ONLY_ALERT_ONCE != 0) { "${n.tag} alerts again" }
            }
            fun named(title: String) = listed().any { it.tag == folded && text(it, Notification.EXTRA_TITLE) == title }
            until("project's name hidden") { named("Øphio") }
            // Posted from now on, an alert is hidden from the start.
            check(shade.show("checks/q", "Question from Claude", body, null) == Notifications.Posted.SHOWN) { "not shown again" }
            until("new hidden") { listed().any { it.tag == "checks/q" && text(it, Notification.EXTRA_TEXT) == Notifications.LOCKED } }
            check(shade.show(folded, "ophio has 3 notifications", body, null) == Notifications.Posted.SHOWN) { "not shown again: $folded" }
            until("project's new count hidden") { named("Øphio") && !named("ophio has 3 notifications") }
            shade.cancel(folded)
            android.util.Log.i("AlertChecks", "redaction kept ${kept.size} of ${tags.size} alerts")

            // Turned on as the app's process ended, before its own pass ran:
            // a later check is waiting, and the next process hides them, asked
            // from two places at once, at no faster a pace, so none is lost.
            for (tag in tags) shade.cancel(tag)
            lock.edit().putBoolean("enabled", false).commit()
            for (tag in again) {
                check(shade.show(tag, "Question from Claude", body, null) == Notifications.Posted.SHOWN) { "not shown: $tag" }
            }
            until("details shown again") { listed().count { text(it, Notification.EXTRA_TEXT) == body } == again.size }
            // Past what this process recalls of its posts, as a new one.
            Thread.sleep(6_000)
            lock.edit().putBoolean("enabled", true).commit()
            AlertRedaction.schedule(context)
            check(jobs.getPendingJob(8302) != null) { "no later check" }
            val other = Thread { Notifications(context).redact() }.apply { start() }
            Notifications(context).redact()
            other.join()
            val resumed = listed().filter { it.tag in again }
            check(resumed.size == again.size) { "${again.size - resumed.size} lost after a restart" }
            for (n in resumed) check(text(n, Notification.EXTRA_TEXT) == Notifications.LOCKED) { "${n.tag} not hidden after a restart" }
        } finally {
            for (tag in tags + again + folded) shade.cancel(tag)
            manager.cancel("summary:$group", 2)
            jobs.cancel(8302)
            lock.edit().clear().commit()
        }
    }

    private fun clears() {
        val clears = Clears(keep = 4)
        val since = clears.count
        clears.clear("a")
        check(clears.refuses("a", since) && !clears.refuses("b", since))
        // Learned of again after it was cleared, as reopened, it is posted.
        check(!clears.refuses("a", clears.count))
        // Cleared again, it moves to the newest and is kept longest.
        val later = clears.count
        clears.clear("a")
        repeat(3) { clears.clear("o$it") }
        check(clears.refuses("a", later))
        // A post queued before clears no longer kept is refused: its own
        // may have been among them.
        val queued = clears.count
        clears.clear("b")
        repeat(4) { clears.clear("x$it") }
        check(clears.refuses("b", queued) && clears.refuses("c", queued))
        check(!clears.refuses("c", clears.count))
    }

    private fun shown() {
        val shown = Shown(wait = 100)
        val g = "computer"
        // One just posted counts, in its group, before Android lists it.
        shown.post("a", g, 0)
        check(shown.showing(emptyMap(), 1) == mapOf("a" to g))
        val listed = mapOf("a" to g, "b" to g, "c" to g)
        check(shown.showing(listed, 2).size == 3)
        // Cleared at once, before Android stops listing them, none counts:
        // the group's count falls to nothing.
        for (tag in listOf("a", "b", "c")) shown.clear(tag, 3)
        check(shown.showing(listed, 4).isEmpty())
        // Gone from the list, they stay gone; posted again, one counts.
        check(shown.showing(emptyMap(), 5).isEmpty())
        shown.post("b", g, 6)
        check(shown.showing(emptyMap(), 7) == mapOf("b" to g))
        // Cleared before Android listed it, it gives back its group.
        check(shown.clear("b", 8) == g && shown.showing(emptyMap(), 9).isEmpty())
        // Neither lasts past the wait: Android's list is the truth then.
        shown.post("d", g, 10)
        shown.clear("e", 10)
        check(shown.showing(mapOf("e" to null), 200) == mapOf("e" to null))
    }

    private fun routes(context: Context) {
        val store = context.getSharedPreferences("alert-checks-routes", Context.MODE_PRIVATE)
        store.edit().clear().commit()
        try {
            val routes = Routes(store, keep = 8)
            // Showing alerts keep their routes however many others are made.
            val showing = (0 until 4).map { "p/$it" }
            val tokens = showing.associateWith { routes.add(it, JSONObject().put("to", it), emptyList()) }
            repeat(40) { routes.add("q/$it", null, showing) }
            check(store.all.size <= 8)
            // Posted again to the same place, in any key order, it keeps
            // its token, so a tap on its way still opens it.
            val once = routes.add("r/0", JSONObject().put("a", 1).put("b", "c"), showing)
            check(routes.add("r/0", JSONObject().put("b", "c").put("a", 1), showing) == once)
            check(routes.add("p/1", JSONObject().put("to", "p/1"), showing) == tokens.getValue("p/1"))
            // A replaced alert's earlier route goes with it.
            val first = tokens.getValue("p/0")
            val replaced = routes.add("p/0", JSONObject().put("to", "again"), showing)
            check(store.getString(first, null) == null && store.all.size <= 8)
            for (tag in showing.drop(1)) {
                routes.open(tokens.getValue(tag))
                val opened = routes.takeOpen() ?: error("lost")
                check(opened.getString("id") == tag && opened.getJSONObject("route").getString("to") == tag)
            }
            routes.open(replaced)
            check(routes.takeOpen()?.getJSONObject("route")?.getString("to") == "again")
            // One use: tapped again, it opens nothing.
            routes.open(replaced)
            check(routes.takeOpen() == null)
        } finally {
            store.edit().clear().commit()
            context.deleteSharedPreferences("alert-checks-routes")
        }
    }
}
