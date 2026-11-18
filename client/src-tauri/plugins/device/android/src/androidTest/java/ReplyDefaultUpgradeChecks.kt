// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

internal object ReplyDefaultUpgradeChecks {
    fun run() {
        var eligible = 0
        for (sdk in listOf(28, 29, 30, 37))
            for (generation in listOf(1, 2, 3))
                for (tier in listOf("off", "silent", "alert"))
                    for (explicit in listOf(false, true))
                        for (userImportance in listOf(false, true))
                            for (userSound in listOf(false, true)) {
                                val got = ReplyDefaultUpgrade.allowed(sdk, generation, tier, explicit, userImportance, userSound)
                                val untouched = generation == 1 && tier == "silent" && !explicit && !userImportance && !userSound
                                check(got == (sdk >= 30 && untouched)) {
                                    "Changed a protected Reply choice: $sdk $generation $tier $explicit $userImportance $userSound"
                                }
                                if (got) eligible++
                            }
        check(eligible == 2)
    }
}

/** Also runnable as a plain JVM check without a device. */
fun main() {
    ReplyDefaultUpgradeChecks.run()
    println("Reply default upgrade: 288 combinations passed")
}
