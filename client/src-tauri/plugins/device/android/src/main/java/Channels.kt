// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.app.NotificationChannel
import android.app.NotificationChannelGroup
import android.app.NotificationManager
import android.content.Context
import android.os.Build
import org.json.JSONArray
import org.json.JSONObject

/**
 * One Android notification category for each notification type, so
 * Android's own settings list exactly the types the app's Settings does.
 *
 * The category is where this phone's choice for a type lives: Alert is high
 * importance (sound and a pop-up), Silent is low (in the shade only), and
 * Off is a category switched off. The app reads it back each time, so a
 * change made in Android's settings is the choice from then on.
 *
 * Android lets an app lower a category's importance but never raise it, so
 * a choice raised in the app replaces the category with a fresh one at the
 * new level. Each type's current category id carries a generation number,
 * kept here, as a deleted category can't be made again under its old id
 * without bringing back its old settings.
 */
internal object Channels {
    /** Where a project's or a task's own choice raises a type above the phone's. */
    const val FOLLOW = "follow"
    /** Øphio's own word about its notifications, which never sounds. */
    const val NOTICES = "notices"
    private const val PREFS = "alert-channels"
    private const val OPHIO = "ophio"

    data class Type(val id: String, val name: String, val description: String, val group: String, val tier: String)

    @Volatile private var known: List<Type>? = null

    /** Every type, as the app's core names them. */
    fun types(): List<Type> =
        known ?: run {
            val table = try {
                JSONArray(AlertWatcher.types())
            } catch (_: Throwable) {
                JSONArray()
            }
            val list = (0 until table.length()).map { i ->
                val t = table.getJSONObject(i)
                Type(t.getString("type"), t.getString("name"), t.getString("description"), t.getString("group"), t.getString("tier"))
            }
            if (list.isNotEmpty()) known = list
            list
        }

    private fun type(id: String): Type? = types().firstOrNull { it.id == id }

    private fun prefs(context: Context) = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /** The current category id of type `type`. */
    fun id(context: Context, type: String): String = "type-$type-${prefs(context).getInt(type, 1)}"

    private fun importance(tier: String): Int =
        when (tier) {
            "alert" -> NotificationManager.IMPORTANCE_HIGH
            "silent" -> NotificationManager.IMPORTANCE_LOW
            else -> NotificationManager.IMPORTANCE_NONE
        }

    private fun tierOf(importance: Int): String =
        when {
            importance == NotificationManager.IMPORTANCE_NONE -> "off"
            importance <= NotificationManager.IMPORTANCE_LOW -> "silent"
            else -> "alert"
        }

    fun rank(tier: String): Int = when (tier) { "alert" -> 2; "silent" -> 1; else -> 0 }

    /**
     * Makes sure every category exists, at its recommended level the first
     * time, and that the categories from before each type had its own are
     * gone, with the notifications posted to them.
     */
    @Synchronized fun ensure(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java)
        // The person's choices in the old categories, read before they go:
        // a type that used to show in one takes a quieter choice over.
        val old = JSONObject()
        for (id in listOf("needs-you", "finished", "updates")) {
            manager.getNotificationChannel(id)?.let { old.put(id, it.importance) }
        }
        val imported = if (old.length() == 0) JSONObject() else try {
            JSONObject(AlertWatcher.importTiers(old.toString()))
        } catch (_: Throwable) {
            JSONObject()
        }
        manager.createNotificationChannelGroups(
            listOf(
                NotificationChannelGroup("needs_you", "Needs you"),
                NotificationChannelGroup("worth_a_look", "Worth a look"),
                NotificationChannelGroup("in_the_app", "Only in the app unless you choose"),
                NotificationChannelGroup(OPHIO, "Øphio"),
            )
        )
        for (t in types()) {
            if (manager.getNotificationChannel(id(context, t.id)) == null) {
                create(context, t, imported.optString(t.id).takeIf { it.isNotEmpty() } ?: t.tier)
            }
        }
        upgradeReplyDefault(context, imported.has("reply"))
        for (gone in listOf("attention", "connection", "background", "needs-you", "finished", "updates")) {
            manager.deleteNotificationChannel(gone)
        }
        manager.createNotificationChannel(
            NotificationChannel(FOLLOW, "Projects and tasks you follow", NotificationManager.IMPORTANCE_HIGH).apply {
                description = "A project or a task you chose to hear about, over your choice for its type."
                group = OPHIO
            }
        )
        manager.createNotificationChannel(
            NotificationChannel(NOTICES, "Notes from Øphio", NotificationManager.IMPORTANCE_LOW).apply {
                description = "What changed about Øphio’s notifications, and how many wait."
                group = OPHIO
            }
        )
    }

    /** Only the old, untouched Reply default changes; never another type.
     * Android 10 and earlier cannot tell us whether sound was user-set.
     * Old app taps choosing Silent while already Silent were not recorded:
     * that indistinguishable case adopts the new reply default once. */
    private fun upgradeReplyDefault(context: Context, imported: Boolean) {
        val prefs = prefs(context)
        if (prefs.getBoolean("reply-audible-default-v1", false)) return
        val manager = context.getSystemService(NotificationManager::class.java)
        val channel = manager.getNotificationChannel(id(context, "reply"))
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R && channel != null &&
            ReplyDefaultUpgrade.allowed(
                sdk = Build.VERSION.SDK_INT,
                generation = prefs.getInt("reply", 1),
                tier = tierOf(channel.importance),
                explicit = prefs.contains("choice-reply") || imported,
                userImportance = channel.hasUserSetImportance(),
                userSound = channel.hasUserSetSound(),
            )
        ) {
            set(context, "reply", "alert")
        }
        prefs.edit().putBoolean("reply-audible-default-v1", true).commit()
    }

    private fun create(context: Context, t: Type, tier: String) {
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(id(context, t.id), t.name, importance(tier)).apply {
                description = t.description
                group = t.group
            }
        )
    }

    /** This phone's choice for `type`, as its category says now. */
    fun tier(context: Context, type: String): String {
        val manager = context.getSystemService(NotificationManager::class.java)
        val channel = manager.getNotificationChannel(id(context, type))
        if (channel == null) {
            val t = type(type) ?: return "alert"
            create(context, t, t.tier)
            return t.tier
        }
        return tierOf(channel.importance)
    }

    /** Every type's choice on this phone. */
    fun tiers(context: Context): JSONObject {
        val out = JSONObject()
        for (t in types()) out.put(t.id, tier(context, t.id))
        return out
    }

    /**
     * Sets this phone's choice for `type`. Returns the category id that held
     * it before, when that category was replaced (its notifications go with
     * it), or the same id when it was changed in place.
     */
    @Synchronized fun set(context: Context, type: String, tier: String): String? {
        val t = type(type) ?: return null
        // Remember even a no-op choice, so future default changes respect it.
        prefs(context).edit().putString("choice-$type", tier).commit()
        val manager = context.getSystemService(NotificationManager::class.java)
        val before = id(context, type)
        val channel = manager.getNotificationChannel(before)
        val want = importance(tier)
        if (channel != null && channel.importance == want) return before
        if (channel != null && want < channel.importance) {
            // Lowered in place, unless the person changed it in Android's
            // settings, which Android then keeps.
            create(context, t, tier)
            if (manager.getNotificationChannel(before)?.importance == want) return before
        }
        manager.deleteNotificationChannel(before)
        prefs(context).edit().putInt(type, prefs(context).getInt(type, 1) + 1).commit()
        create(context, t, tier)
        return before
    }
}
