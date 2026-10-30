// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.app.Service
import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.ConnectivityManager
import android.net.Network
import android.os.IBinder
import android.provider.Settings

/**
 * Keeps the alert watcher running while the app is closed, with no notice of
 * its own. Android lets a service run in the background without one only for
 * an app whose battery use is Unrestricted; otherwise it stops this soon after
 * the app leaves the screen, and alerts arrive only while the app is open.
 */
class AlertService : Service() {
    companion object {
        private fun prefs(context: Context) =
            context.getSharedPreferences("alerts", Context.MODE_PRIVATE)

        /** Whether the person turned alerts on. */
        fun wanted(context: Context) = prefs(context).getBoolean("run", false)

        fun want(context: Context, run: Boolean): Boolean =
            prefs(context).edit().putBoolean("run", run).commit()

        /** False when Android does not allow it to start now, as in the background without Unrestricted battery use. */
        fun start(context: Context): Boolean =
            try {
                context.startService(Intent(context, AlertService::class.java))
                true
            } catch (_: Exception) {
                false
            }
    }

    private var network: ConnectivityManager.NetworkCallback? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int =
        if (run()) START_STICKY else START_NOT_STICKY

    /**
     * Swiping the app away from Recents ends its process. Android starts this
     * again in a new process, and may then call only this, not onStartCommand.
     */
    override fun onTaskRemoved(rootIntent: Intent?) {
        run()
    }

    /** Starts the watcher; false when it can't start now. */
    private fun run(): Boolean {
        if (!wanted(this)) {
            stopSelf()
            return false
        }
        try {
            AlertWatcher.attach(this)
            // Restarting the phone or updating the app clears the alerts shown.
            val boot = Settings.Global.getInt(contentResolver, Settings.Global.BOOT_COUNT, -1)
            val updated = packageManager.getPackageInfo(packageName, 0).lastUpdateTime
            check(AlertWatcher.start(dataDir.absolutePath, "$boot:$updated"))
        } catch (_: Exception) {
            // The scheduled check tries again.
            stopSelf()
            return false
        }
        watchNetwork()
        return true
    }

    /** A connection made on one network rarely survives a move to another. */
    private fun watchNetwork() {
        if (network != null) return
        try {
            val connectivity = getSystemService(ConnectivityManager::class.java)
            val callback =
                object : ConnectivityManager.NetworkCallback() {
                    // The network in use when it was registered is no move. With
                    // none, as when it starts offline, the first to come is one.
                    private var current: Network? = connectivity.activeNetwork

                    override fun onAvailable(available: Network) {
                        val moved = available != current
                        current = available
                        if (moved) AlertWatcher.nudge()
                    }

                    override fun onLost(lost: Network) {
                        if (lost == current) current = null
                    }
                }
            connectivity.registerDefaultNetworkCallback(callback)
            network = callback
        } catch (_: Exception) {}
    }

    override fun onDestroy() {
        network?.let {
            try {
                getSystemService(ConnectivityManager::class.java).unregisterNetworkCallback(it)
            } catch (_: Exception) {}
        }
        network = null
        AlertWatcher.stop()
        super.onDestroy()
    }
}

/** Starts alerts again after the phone restarts or the app updates. */
class AlertStarter : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (
            intent.action != Intent.ACTION_BOOT_COMPLETED &&
                intent.action != Intent.ACTION_MY_PACKAGE_REPLACED
        )
            return
        if (intent.action == Intent.ACTION_MY_PACKAGE_REPLACED) AppUpdate.replaced(context)
        if (AlertService.wanted(context)) AlertService.start(context)
        AlertCheck.schedule(context)
    }
}

/** Every so often, starts alerts again if Android stopped them. */
class AlertCheck : JobService() {
    companion object {
        private const val JOB = 8301

        fun schedule(context: Context) {
            val jobs = context.getSystemService(JobScheduler::class.java)
            if (!AlertService.wanted(context)) {
                jobs.cancel(JOB)
                return
            }
            if (jobs.getPendingJob(JOB) != null) return
            jobs.schedule(
                JobInfo.Builder(JOB, ComponentName(context, AlertCheck::class.java))
                    .setPeriodic(15 * 60_000L)
                    .setPersisted(true)
                    .build()
            )
        }
    }

    override fun onStartJob(params: JobParameters?): Boolean {
        // Android allows this only when the app may run without battery limits.
        if (AlertService.wanted(this) && !AlertWatcher.running()) AlertService.start(this)
        return false
    }

    override fun onStopJob(params: JobParameters?): Boolean = false
}

/**
 * Finishes hiding the alerts' details after app lock is turned on, should the
 * app's process end before its own pass could, as when swiped away.
 */
class AlertRedaction : JobService() {
    companion object {
        private const val JOB = 8302

        fun schedule(context: Context) {
            context
                .getSystemService(JobScheduler::class.java)
                .schedule(
                    JobInfo.Builder(JOB, ComponentName(context, AlertRedaction::class.java))
                        // After the app's own pass would have finished.
                        .setMinimumLatency(30_000L)
                        .setOverrideDeadline(60_000L)
                        .build()
                )
        }
    }

    override fun onStartJob(params: JobParameters?): Boolean {
        Thread {
                runCatching { Notifications(applicationContext).redact() }
                jobFinished(params, false)
            }
            .start()
        return true
    }

    override fun onStopJob(params: JobParameters?): Boolean = true
}
