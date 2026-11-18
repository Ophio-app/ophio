// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

/** Kept independent of Android so the conservative upgrade rule is testable. */
internal object ReplyDefaultUpgrade {
    fun allowed(
        sdk: Int,
        generation: Int,
        tier: String,
        explicit: Boolean,
        userImportance: Boolean,
        userSound: Boolean,
    ): Boolean = sdk >= 30 && generation == 1 && tier == "silent" &&
        !explicit && !userImportance && !userSound
}
