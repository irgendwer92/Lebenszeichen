package io.github.irgendwer92.lebenszeichen.droid

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import java.util.concurrent.TimeUnit

/**
 * Receives the tap on a request notification and its dismissal.
 *
 * A receiver only lives for a few seconds, so the answer is not sent here.
 * It is handed to WorkManager, and the receiver stays alive just until
 * WorkManager has stored the job.
 */
class ReactionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val requestId = intent.getStringExtra(RequestPresenter.EXTRA_REQUEST_ID) ?: return
        val reaction = intent.getStringExtra(RequestPresenter.EXTRA_REACTION) ?: return
        val app = context.applicationContext
        val pending = goAsync()
        Thread {
            try {
                Requests.react(app, requestId, reaction)?.result?.get(8, TimeUnit.SECONDS)
            } catch (e: Exception) {
                EventLog.add(app, requestId, "error", "Reaktion nicht verarbeitet: $e")
            } finally {
                pending.finish()
            }
        }.start()
    }
}
