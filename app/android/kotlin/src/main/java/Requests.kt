package io.github.irgendwer92.lebenszeichen.droid

import android.content.Context
import org.json.JSONObject

/** A request of a contact that waits for an answer, as the core lists it. */
data class OpenRequest(val name: String, val since: Long, val expiresAt: Long, val title: String) {
    /** Tells requests apart, also two requests of the same contact. */
    val key: String get() = "$name@$since"
}

/**
 * The path of a request: the core lists it, the presenter shows it, a
 * reaction answers it.
 */
object Requests {
    const val REACTION_TAP = "tap"
    const val REACTION_DISMISS = "dismiss"
    const val REACTION_FULLSCREEN_TAP = "fullscreen-tap"
    const val REACTION_FULLSCREEN_CLOSE = "fullscreen-close"
    const val REACTION_FULLSCREEN_LEAVE = "fullscreen-leave"

    private val lock = Any()

    /** Reads the `requests` of an answer of the core. */
    fun parse(answer: JSONObject): List<OpenRequest> {
        val list = answer.optJSONArray("requests") ?: return emptyList()
        return (0 until list.length()).map { i ->
            val item = list.getJSONObject(i)
            OpenRequest(
                name = item.getString("name"),
                since = item.getLong("since"),
                expiresAt = item.getLong("expires_at"),
                title = item.getString("title"),
            )
        }
    }

    /**
     * Makes the notifications match the open requests in [answer]: shows new
     * ones, withdraws those that are answered, expired or gone, and plans a
     * check at the next expiry. Requests whose answer is on its way are
     * left alone.
     */
    fun update(context: Context, answer: JSONObject, source: String) {
        val app = context.applicationContext
        answer.errorText()?.let {
            EventLog.add(app, NO_REQUEST, "core-error", "$source: $it")
        }
        if (!answer.has("requests")) return
        val open = parse(answer)
        synchronized(lock) {
            val waiting = open.filterNot { Store.isAnswered(app, it.key) }
            val visible = RequestPresenter.visible(app)
            for ((name, key) in visible) {
                if (waiting.none { it.key == key }) {
                    RequestPresenter.cancel(app, name)
                    EventLog.add(app, key, "withdrawn", source)
                }
            }
            for (request in waiting) {
                if (visible[request.name] != request.key) RequestPresenter.show(app, request)
            }
            Store.forgetAnswersExcept(app, open.map { it.key }.toSet())
            RequestActivity.closeAllExcept(waiting.map { it.key }.toSet())
        }
        ExpiryWorker.schedule(app, open.minOfOrNull { it.expiresAt })
    }

    /** Lists the stored requests and updates the notifications; blocks. */
    fun refresh(context: Context, source: String) {
        update(context, CoreBridge.openRequests(context), source)
    }

    /**
     * Handles a reaction of the user. Only the first reaction to a request
     * sends an answer.
     */
    fun react(context: Context, name: String, since: Long, reaction: String) {
        val app = context.applicationContext
        val key = "$name@$since"
        RequestPresenter.cancel(app, name)
        if (!Store.claimAnswer(app, key)) {
            EventLog.add(app, key, "reaction-ignored", "$reaction, already answered")
            return
        }
        EventLog.add(app, key, "reaction", reaction)
        AnswerWorker.enqueue(app, name, since)
    }

    const val NO_REQUEST = "-"
}
