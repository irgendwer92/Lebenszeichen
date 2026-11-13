package io.github.irgendwer92.lebenszeichen.droid

import android.content.Context
import androidx.work.Operation
import java.security.SecureRandom
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/** The path of a request: opened by a wake signal, closed by a reaction. */
object Requests {
    const val REACTION_TAP = "tap"
    const val REACTION_DISMISS = "dismiss"
    const val REACTION_FULLSCREEN_TAP = "fullscreen-tap"
    const val REACTION_FULLSCREEN_CLOSE = "fullscreen-close"

    /** Readable and unique enough for a test: time plus four random hex digits. */
    fun newId(): String {
        val time = SimpleDateFormat("yyyyMMdd-HHmmss", Locale.US).format(Date())
        val random = String.format(Locale.US, "%04x", SecureRandom().nextInt(0x10000))
        return "r$time-$random"
    }

    /**
     * Calls the Rust core and shows the request. Used for wake signals and
     * for the local trigger alike.
     */
    fun open(context: Context, requestId: String, source: String) {
        val app = context.applicationContext
        EventLog.add(app, requestId, "core", CoreBridge.describe("wake", requestId))
        RequestPresenter.show(app, requestId, source)
    }

    /**
     * Handles a reaction of the user. Only the first reaction to a request
     * sends an answer. Returns the pending enqueue operation, or `null` if
     * the request was answered before.
     */
    fun react(context: Context, requestId: String, reaction: String): Operation? {
        val app = context.applicationContext
        RequestPresenter.cancel(app, requestId)
        if (!Store.claimAnswer(app, requestId)) {
            EventLog.add(app, requestId, "reaction-ignored", "$reaction, bereits beantwortet")
            return null
        }
        EventLog.add(app, requestId, "reaction", reaction)
        val body = CoreBridge.describe(reaction, requestId)
        EventLog.add(app, requestId, "core", body)
        return AnswerWorker.enqueue(app, requestId, body)
    }
}
