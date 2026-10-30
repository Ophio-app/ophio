// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.app.ActivityManager.RunningAppProcessInfo
import android.app.ApplicationExitInfo

/** Which past exits the phone offers to report. Needs no device state. */
internal object ExitChecks {
    fun run() {
        val shown = RunningAppProcessInfo.IMPORTANCE_FOREGROUND
        val visible = RunningAppProcessInfo.IMPORTANCE_VISIBLE
        val notVisible = listOf(
            RunningAppProcessInfo.IMPORTANCE_FOREGROUND_SERVICE,
            0, // Android may not have recorded the process importance.
            RunningAppProcessInfo.IMPORTANCE_PERCEPTIBLE,
            RunningAppProcessInfo.IMPORTANCE_SERVICE,
            RunningAppProcessInfo.IMPORTANCE_TOP_SLEEPING,
            RunningAppProcessInfo.IMPORTANCE_CACHED,
            RunningAppProcessInfo.IMPORTANCE_GONE,
        )
        for (importance in listOf(shown, visible) + notVisible) {
            check(Reports.exitCode(ApplicationExitInfo.REASON_CRASH, 0, importance) == "crash")
            check(Reports.exitCode(ApplicationExitInfo.REASON_CRASH_NATIVE, 0, importance) == "crash_native")
            check(Reports.exitCode(ApplicationExitInfo.REASON_ANR, 0, importance) == "anr")
            check(Reports.exitCode(ApplicationExitInfo.REASON_INITIALIZATION_FAILURE, 0, importance) == "initialization_failure")
            for (status in listOf(4, 6, 7, 8, 11)) {
                check(Reports.exitCode(ApplicationExitInfo.REASON_SIGNALED, status, importance) == "signaled")
            }
            for (status in listOf(0, 9, 15)) {
                check(Reports.exitCode(ApplicationExitInfo.REASON_SIGNALED, status, importance) == null)
            }
            for (reason in listOf(
                ApplicationExitInfo.REASON_UNKNOWN, ApplicationExitInfo.REASON_EXIT_SELF,
                ApplicationExitInfo.REASON_USER_REQUESTED, ApplicationExitInfo.REASON_USER_STOPPED,
                ApplicationExitInfo.REASON_PERMISSION_CHANGE, ApplicationExitInfo.REASON_PACKAGE_STATE_CHANGE,
                ApplicationExitInfo.REASON_PACKAGE_UPDATED, ApplicationExitInfo.REASON_OTHER,
            )) {
                check(Reports.exitCode(reason, 0, importance) == null)
            }
        }
        // Android closing the app while it was out of sight is not a crash.
        for ((reason, code) in listOf(
            ApplicationExitInfo.REASON_LOW_MEMORY to "low_memory",
            ApplicationExitInfo.REASON_FREEZER to "freezer",
            ApplicationExitInfo.REASON_DEPENDENCY_DIED to "dependency_died",
            ApplicationExitInfo.REASON_EXCESSIVE_RESOURCE_USAGE to "excessive_resource_usage",
        )) {
            for (importance in notVisible) {
                check(Reports.exitCode(reason, 0, importance) == null) {
                    "reason $reason counted without a visible activity (importance $importance)"
                }
            }
            check(Reports.exitCode(reason, 0, shown) == code) { "reason $reason changed on screen" }
            check(Reports.exitCode(reason, 0, visible) == code)
        }
    }
}
