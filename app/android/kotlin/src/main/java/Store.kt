package io.github.irgendwer92.lebenszeichen.droid

import android.content.Context
import android.content.SharedPreferences

/**
 * Small state of the Kotlin layer. Contacts, keys and requests live in the
 * state file of the core; this only remembers what Android needs between
 * processes: the push endpoint and which requests already have an answer
 * on its way.
 */
object Store {
    private const val PREFS = "lebenszeichen"
    private const val KEY_ENDPOINT = "endpoint"
    private const val PREFIX_ANSWERED = "answered_"
    private val lock = Any()

    private fun prefs(context: Context): SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    fun endpoint(context: Context): String = prefs(context).getString(KEY_ENDPOINT, "") ?: ""

    fun setEndpoint(context: Context, url: String) {
        prefs(context).edit().putString(KEY_ENDPOINT, url).commit()
    }

    fun isAnswered(context: Context, key: String): Boolean = prefs(context).getBoolean(PREFIX_ANSWERED + key, false)

    /**
     * Marks the request as answered. Only the first caller gets `true`; that
     * is what limits every request to one answer. Written synchronously so
     * the mark survives if the process dies right afterwards.
     */
    fun claimAnswer(context: Context, key: String): Boolean =
        synchronized(lock) {
            val prefs = prefs(context)
            if (prefs.getBoolean(PREFIX_ANSWERED + key, false)) {
                false
            } else {
                prefs.edit().putBoolean(PREFIX_ANSWERED + key, true).commit()
                true
            }
        }

    /** The answer failed for good: the request may be answered again. */
    fun releaseAnswer(context: Context, key: String) {
        prefs(context).edit().remove(PREFIX_ANSWERED + key).commit()
    }

    /** Drops the marks of requests that are no longer open. */
    fun forgetAnswersExcept(context: Context, open: Set<String>) {
        val prefs = prefs(context)
        val stale =
            prefs.all.keys.filter { it.startsWith(PREFIX_ANSWERED) && it.removePrefix(PREFIX_ANSWERED) !in open }
        if (stale.isNotEmpty()) {
            val edit = prefs.edit()
            stale.forEach { edit.remove(it) }
            edit.commit()
        }
    }
}
