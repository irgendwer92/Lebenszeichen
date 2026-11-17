package io.github.irgendwer92.lebenszeichen.droid

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import org.json.JSONObject

/** An answer of a contact to an own request, as the core describes it. */
data class Answer(val name: String, val at: Long, val title: String, val text: String) {
    companion object {
        fun parse(item: JSONObject): Answer =
            Answer(
                name = item.getString("name"),
                at = item.getLong("at"),
                title = item.getString("title"),
                text = item.getString("text"),
            )

        /** Reads the `answers` of an answer of the core. */
        fun parseAll(answer: JSONObject): List<Answer> {
            val list = answer.optJSONArray("answers") ?: return emptyList()
            return (0 until list.length()).map { parse(list.getJSONObject(it)) }
        }
    }
}

/**
 * Says that a contact answered an own request: an ordinary notification
 * with sound on a channel of its own, which can be turned off without
 * touching the channel of requests. One per contact; a newer answer
 * replaces the older one. It answers nothing; a tap opens the app.
 */
object AnswerPresenter {
    /**
     * Android never lets an app change sound or vibration of an existing
     * channel. If the defaults change, the id needs a new suffix.
     */
    const val CHANNEL_ID = "answers_v1"

    /** All answers share this id and differ by tag: the name of the contact. */
    private const val NOTIFICATION_ID = 2

    private fun manager(context: Context): NotificationManager =
        context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

    /**
     * Default importance: sound, no banner. Name and description follow the
     * language chosen in the app.
     */
    fun ensureChannel(context: Context) {
        if (Build.VERSION.SDK_INT < 26) return
        val texts = Texts.localized(context)
        val channel =
            NotificationChannel(
                CHANNEL_ID,
                texts.getString(R.string.lz_answer_channel_name),
                NotificationManager.IMPORTANCE_DEFAULT,
            ).apply {
                description = texts.getString(R.string.lz_answer_channel_description)
                setShowBadge(false)
            }
        manager(context).createNotificationChannel(channel)
    }

    fun show(context: Context, answer: Answer) {
        val app = context.applicationContext
        val key = "${answer.name}@${answer.at}"
        if (!NotificationManagerCompat.from(app).areNotificationsEnabled()) {
            EventLog.add(app, key, "answer-not-shown", "notifications are not allowed")
            return
        }
        ensureChannel(app)
        val open =
            app.packageManager.getLaunchIntentForPackage(app.packageName)?.let {
                PendingIntent.getActivity(app, 0, it, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
            }
        val notification =
            NotificationCompat.Builder(app, CHANNEL_ID)
                .setSmallIcon(R.drawable.ic_lz_request)
                .setContentTitle(answer.title)
                .setContentText(answer.text)
                // Before Android 8 there are no channels: priority and defaults apply instead.
                .setPriority(NotificationCompat.PRIORITY_DEFAULT)
                .setDefaults(NotificationCompat.DEFAULT_SOUND)
                // With the name on the lock screen too.
                .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
                .setWhen(answer.at * 1000)
                .setShowWhen(true)
                .setAutoCancel(true)
                .apply { open?.let { setContentIntent(it) } }
                .build()
        try {
            manager(app).notify(answer.name, NOTIFICATION_ID, notification)
            EventLog.add(app, key, "answer-shown", answer.text)
        } catch (e: SecurityException) {
            EventLog.add(app, key, "answer-not-shown", e.message ?: "")
        }
    }
}
