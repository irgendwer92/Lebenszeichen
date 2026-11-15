package io.github.irgendwer92.lebenszeichen.droid

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * Receives the tap on a request notification and its dismissal.
 *
 * A receiver only lives for a few seconds, so the answer is not sent here.
 * It is handed to WorkManager, and the receiver stays alive just until
 * WorkManager has stored the job.
 */
class ReactionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val name = intent.getStringExtra(RequestPresenter.EXTRA_NAME) ?: return
        val since = intent.getLongExtra(RequestPresenter.EXTRA_SINCE, -1).takeIf { it >= 0 } ?: return
        val reaction = intent.getStringExtra(RequestPresenter.EXTRA_REACTION) ?: return
        val app = context.applicationContext
        val pending = goAsync()
        Thread {
            try {
                Requests.react(app, name, since, reaction)
            } catch (e: Exception) {
                EventLog.add(app, "$name@$since", "error", "Reaktion nicht verarbeitet: $e")
            } finally {
                pending.finish()
            }
        }.start()
    }
}
