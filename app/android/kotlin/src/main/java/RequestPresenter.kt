package io.github.irgendwer92.lebenszeichen.droid

import android.app.KeyguardManager
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat

/**
 * Shows a request like an alarm clock: a notification with a full-screen
 * intent. Android starts the full-screen activity only while the device is
 * locked or the screen is off; otherwise it shows a heads-up banner.
 */
object RequestPresenter {
    /**
     * Android never lets an app change sound or vibration of an existing
     * channel. If the defaults change, the id needs a new suffix.
     */
    const val CHANNEL_ID = "requests_silent_v2"

    /**
     * Channels before v2: deleted, because v1 forced full content on the
     * lock screen and a channel cannot be changed afterwards.
     */
    private val OLD_CHANNELS = listOf("requests_silent_v1")

    const val EXTRA_NAME = "io.github.irgendwer92.lebenszeichen.NAME"
    const val EXTRA_SINCE = "io.github.irgendwer92.lebenszeichen.SINCE"
    const val EXTRA_TITLE = "io.github.irgendwer92.lebenszeichen.TITLE"
    const val EXTRA_REACTION = "io.github.irgendwer92.lebenszeichen.REACTION"

    /** Stored in the notification, so that [visible] knows which request it shows. */
    private const val EXTRA_KEY = "io.github.irgendwer92.lebenszeichen.KEY"

    /** All requests share this id and differ by tag: the name of the contact. */
    private const val NOTIFICATION_ID = 1

    private fun manager(context: Context): NotificationManager =
        context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

    /**
     * High importance is required for the full-screen intent and the banner;
     * sound and vibration are switched off on the channel instead. Name and
     * description follow the language chosen in the app; creating the channel
     * again with the same id only updates them.
     */
    fun ensureChannel(context: Context) {
        if (Build.VERSION.SDK_INT < 26) return
        val texts = Texts.localized(context)
        OLD_CHANNELS.forEach { manager(context).deleteNotificationChannel(it) }
        val channel =
            NotificationChannel(
                CHANNEL_ID,
                texts.getString(R.string.lz_notification_channel_name),
                NotificationManager.IMPORTANCE_HIGH,
            ).apply {
                description = texts.getString(R.string.lz_notification_channel_description)
                setSound(null, null)
                enableVibration(false)
                enableLights(false)
                setShowBadge(false)
                // No override: the notification decides (private, with a neutral public version).
            }
        manager(context).createNotificationChannel(channel)
    }

    fun canUseFullScreenIntent(context: Context): Boolean =
        if (Build.VERSION.SDK_INT >= 34) manager(context).canUseFullScreenIntent() else true

    /** The visible request notifications: name of the contact to request key. */
    fun visible(context: Context): Map<String, String> =
        manager(context).activeNotifications
            .filter { it.id == NOTIFICATION_ID && it.tag != null }
            .associate { it.tag to (it.notification.extras.getString(EXTRA_KEY) ?: "") }

    fun show(context: Context, request: OpenRequest) {
        val app = context.applicationContext
        ensureChannel(app)
        val key = request.key
        val texts = Texts.localized(app)

        val notificationsAllowed = NotificationManagerCompat.from(app).areNotificationsEnabled()
        val fullScreenAllowed = canUseFullScreenIntent(app)
        val power = app.getSystemService(Context.POWER_SERVICE) as PowerManager
        val keyguard = app.getSystemService(Context.KEYGUARD_SERVICE) as KeyguardManager
        val state =
            "notifications=$notificationsAllowed, full screen allowed=$fullScreenAllowed, " +
                "screen on=${power.isInteractive}, locked=${keyguard.isKeyguardLocked}"

        if (!notificationsAllowed) {
            EventLog.add(app, key, "not-shown", "notifications are not allowed. $state")
            return
        }

        val fullScreenIntent =
            Intent(app, RequestActivity::class.java)
                .setData(requestUri(request, "fullscreen"))
                .putExtra(EXTRA_NAME, request.name)
                .putExtra(EXTRA_SINCE, request.since)
                .putExtra(EXTRA_TITLE, request.title)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_NO_USER_ACTION)
        val fullScreen =
            PendingIntent.getActivity(
                app,
                0,
                fullScreenIntent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )

        val notification =
            NotificationCompat.Builder(app, CHANNEL_ID)
                .setSmallIcon(R.drawable.ic_lz_request)
                .setContentTitle(request.title)
                .setContentText(texts.getString(R.string.lz_notification_hint))
                .addExtras(android.os.Bundle().apply { putString(EXTRA_KEY, key) })
                // A reminder is held back by "do not disturb" unless the user allows reminders.
                .setCategory(NotificationCompat.CATEGORY_REMINDER)
                // Before Android 8 there are no channels: priority and defaults apply instead.
                .setPriority(NotificationCompat.PRIORITY_MAX)
                .setDefaults(0)
                .setSound(null)
                .setVibrate(null)
                // On the lock screen only "Lebenszeichen?", the name after unlocking.
                // Android honours this when the lock screen hides sensitive content.
                .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
                .setPublicVersion(
                    NotificationCompat.Builder(app, CHANNEL_ID)
                        .setSmallIcon(R.drawable.ic_lz_request)
                        .setContentTitle(texts.getString(R.string.lz_neutral_title))
                        .setContentText(texts.getString(R.string.lz_notification_hint))
                        .setCategory(NotificationCompat.CATEGORY_REMINDER)
                        .build(),
                )
                .setWhen(request.since * 1000)
                .setShowWhen(true)
                .setAutoCancel(true)
                .setContentIntent(reactionIntent(app, request, Requests.REACTION_TAP))
                // Fires when the user swipes the notification away, also with "clear all",
                // but not when the app cancels it.
                .setDeleteIntent(reactionIntent(app, request, Requests.REACTION_DISMISS))
                .setFullScreenIntent(fullScreen, true)
                .build()

        try {
            manager(app).notify(request.name, NOTIFICATION_ID, notification)
            EventLog.add(app, key, "shown", state)
        } catch (e: SecurityException) {
            EventLog.add(app, key, "not-shown", "${e.message}. $state")
        }
    }

    fun cancel(context: Context, name: String) {
        manager(context).cancel(name, NOTIFICATION_ID)
    }

    /** Makes the pending intents of different requests and reactions distinct. */
    private fun requestUri(request: OpenRequest, what: String): Uri =
        Uri.Builder()
            .scheme("lebenszeichen")
            .authority("request")
            .appendPath(request.name)
            .appendPath(request.since.toString())
            .appendPath(what)
            .build()

    private fun reactionIntent(context: Context, request: OpenRequest, reaction: String): PendingIntent {
        val intent =
            Intent(context, ReactionReceiver::class.java)
                .setData(requestUri(request, reaction))
                .putExtra(EXTRA_NAME, request.name)
                .putExtra(EXTRA_SINCE, request.since)
                .putExtra(EXTRA_REACTION, reaction)
        return PendingIntent.getBroadcast(
            context,
            0,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }
}
