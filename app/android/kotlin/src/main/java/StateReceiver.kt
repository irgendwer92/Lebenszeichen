package io.github.irgendwer92.lebenszeichen.droid

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * Receives a state report from another app (Tasker, Automate, the Home
 * Assistant Companion app): `<applicationId>.SET_STATE` with the extras
 * `token`, `reason` and `minutes`, or `<applicationId>.CLEAR_STATE` with
 * `token`. The receiver is exported; the protection is the token, which
 * the core checks (and which is off by default). A report that is not
 * accepted is dropped without a trace; nothing of it goes to the log.
 *
 * The file work runs on a thread of its own, held open by `goAsync()`.
 */
class StateReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val action = intent.action ?: return
        val kind =
            when {
                action.endsWith(".SET_STATE") -> "set"
                action.endsWith(".CLEAR_STATE") -> "clear"
                else -> return
            }
        // Automation apps guess types (Home Assistant turns "480" into an
        // Int and "true" into a Boolean), so every extra is read as text.
        val extras = intent.extras
        val token = extras?.get(EXTRA_TOKEN)?.toString()
        val reason = extras?.get(EXTRA_REASON)?.toString()
        val minutes = extras?.get(EXTRA_MINUTES)?.toString()
        val app = context.applicationContext
        val pending = goAsync()
        Thread {
            try {
                val answer = CoreBridge.reportState(app, kind, token, reason, minutes)
                if (answer.optBoolean("applied")) {
                    EventLog.add(app, Requests.NO_REQUEST, "state-report", kind)
                    val wait = if (answer.has("report_wait")) answer.optLong("report_wait") else null
                    when (wait) {
                        null -> ReportWorker.schedule(app, null)
                        0L -> ReportWorker.now(app)
                        else -> ReportWorker.schedule(app, wait)
                    }
                }
            } catch (e: Exception) {
                EventLog.add(app, Requests.NO_REQUEST, "error", "state report not processed")
            } finally {
                pending.finish()
            }
        }.start()
    }

    companion object {
        const val EXTRA_TOKEN = "token"
        const val EXTRA_REASON = "reason"
        const val EXTRA_MINUTES = "minutes"
    }
}
