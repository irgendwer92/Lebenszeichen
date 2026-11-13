package io.github.irgendwer92.lebenszeichen.droid

import android.content.Context
import android.content.SharedPreferences

/** Small persistent state of the prototype. */
object Store {
    private const val PREFS = "lebenszeichen_m1"
    private const val KEY_ANSWER_URL = "answer_url"
    private const val KEY_ENDPOINT = "endpoint"
    private const val PREFIX_ANSWERED = "answered_"
    private val lock = Any()

    private fun prefs(context: Context): SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    fun answerUrl(context: Context): String = prefs(context).getString(KEY_ANSWER_URL, "") ?: ""

    fun setAnswerUrl(context: Context, url: String) {
        prefs(context).edit().putString(KEY_ANSWER_URL, url).commit()
    }

    fun endpoint(context: Context): String = prefs(context).getString(KEY_ENDPOINT, "") ?: ""

    fun setEndpoint(context: Context, url: String) {
        prefs(context).edit().putString(KEY_ENDPOINT, url).commit()
    }

    fun isAnswered(context: Context, requestId: String): Boolean =
        prefs(context).getBoolean(PREFIX_ANSWERED + requestId, false)

    /**
     * Marks the request as answered. Only the first caller gets `true`; that
     * is what limits every request to one answer. Written synchronously so
     * the mark survives if the process dies right afterwards.
     */
    fun claimAnswer(context: Context, requestId: String): Boolean =
        synchronized(lock) {
            val prefs = prefs(context)
            val key = PREFIX_ANSWERED + requestId
            if (prefs.getBoolean(key, false)) {
                false
            } else {
                prefs.edit().putBoolean(key, true).commit()
                true
            }
        }
}
