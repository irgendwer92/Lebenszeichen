package io.github.irgendwer92.lebenszeichen.droid

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager
import androidx.work.ExistingWorkPolicy
import androidx.work.OneTimeWorkRequest
import androidx.work.WorkManager
import androidx.work.Worker
import androidx.work.WorkerParameters
import java.util.concurrent.TimeUnit

/**
 * The battery check of the setting "Hand over a state when the battery is
 * low". Kotlin only reads the level; the core decides everything
 * (thresholds, once per crossing, setting off means nothing happens), so
 * [check] may be called as often as convenient. Never call it from the
 * main thread (the core reads its state file).
 */
object Battery {
    /**
     * Reads the level and asks the core; sends the shared state at once if
     * one waits. `false` if the core says the setting is off (answer field
     * `battery`): then the job chain need not go on.
     */
    fun check(context: Context): Boolean {
        val app = context.applicationContext
        val sticky = app.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        val manager = app.getSystemService(Context.BATTERY_SERVICE) as BatteryManager
        // The sticky broadcast first: it is what the status bar shows, and
        // it follows `adb shell dumpsys battery set …` for tests, while the
        // BatteryManager properties read the hardware. They serve only
        // without a broadcast.
        val level = sticky?.getIntExtra(BatteryManager.EXTRA_LEVEL, -1) ?: -1
        val scale = sticky?.getIntExtra(BatteryManager.EXTRA_SCALE, -1) ?: -1
        val percent =
            if (level >= 0 && scale > 0) {
                level * 100 / scale
            } else {
                manager.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY)
            }
        if (percent !in 0..100) return true
        val charging =
            if (sticky != null) {
                val status = sticky.getIntExtra(BatteryManager.EXTRA_STATUS, -1)
                status == BatteryManager.BATTERY_STATUS_CHARGING ||
                    status == BatteryManager.BATTERY_STATUS_FULL ||
                    sticky.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0) != 0
            } else {
                manager.isCharging
            }
        val answer = CoreBridge.checkBattery(app, percent, charging)
        if (answer.optBoolean("applied")) {
            EventLog.add(app, Requests.NO_REQUEST, "battery-state", if (charging) "charging" else "low")
            val wait = if (answer.has("report_wait")) answer.optLong("report_wait") else null
            when (wait) {
                null -> ReportWorker.schedule(app, null)
                0L -> ReportWorker.now(app)
                else -> ReportWorker.schedule(app, wait)
            }
        }
        // The shared state waits in the outbox; a fetch sends it.
        if (answer.optBoolean("send")) FetchWorker.enqueue(app, "battery")
        return !answer.has("battery") || answer.optBoolean("battery")
    }
}

/**
 * Checks the battery every 10 minutes: a one-time job that plans the next
 * one, as periodic jobs cannot run more often than every 15 minutes. Under
 * Doze and battery saver it may run late. No network is needed; the fetch
 * that [Battery.check] may queue has its own constraints.
 *
 * The chain stops when the core's answer says the setting is off
 * (`battery: false`); [start] and [ensure] (app start, boot, the window
 * changing something) start it again.
 */
class BatteryWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
    override fun doWork(): Result {
        val app = applicationContext
        val goOn =
            try {
                Battery.check(app)
            } catch (e: Exception) {
                EventLog.add(app, Requests.NO_REQUEST, "error", "battery not checked")
                true
            }
        if (goOn) schedule(app, INTERVAL_MINUTES)
        return Result.success()
    }

    companion object {
        private const val NAME = "battery"
        private const val INTERVAL_MINUTES = 10L

        /** Checks now and then every 10 minutes. */
        fun start(context: Context) = schedule(context.applicationContext, 0)

        /** Makes sure a chain exists, without checking at once. */
        fun ensure(context: Context) {
            val app = context.applicationContext
            val request =
                OneTimeWorkRequest.Builder(BatteryWorker::class.java)
                    .setInitialDelay(INTERVAL_MINUTES, TimeUnit.MINUTES)
                    .build()
            WorkManager.getInstance(app).enqueueUniqueWork(NAME, ExistingWorkPolicy.KEEP, request)
        }

        private fun schedule(app: Context, delayMinutes: Long) {
            val request =
                OneTimeWorkRequest.Builder(BatteryWorker::class.java)
                    .setInitialDelay(delayMinutes, TimeUnit.MINUTES)
                    .build()
            WorkManager.getInstance(app).enqueueUniqueWork(NAME, ExistingWorkPolicy.REPLACE, request)
        }
    }
}

/** Restarts the battery chain after boot (WorkManager also keeps its jobs, this is a safety net). */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action == Intent.ACTION_BOOT_COMPLETED) BatteryWorker.start(context)
    }
}
