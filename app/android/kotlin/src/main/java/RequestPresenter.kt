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
    const val CHANNEL_ID = "requests_silent_v1"
    const val EXTRA_REQUEST_ID = "io.github.irgendwer92.lebenszeichen.REQUEST_ID"
    const val EXTRA_REACTION = "io.github.irgendwer92.lebenszeichen.REACTION"

    /** All requests share this id and differ by tag. */
    private const val NOTIFICATION_ID = 1

    private fun manager(context: Context): NotificationManager =
        context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

    /**
     * High importance is required for the full-screen intent and the banner;
     * sound and vibration are switched off on the channel instead.
     */
    fun ensureChannel(context: Context) {
        if (Build.VERSION.SDK_INT < 26) return
        val channel =
            NotificationChannel(CHANNEL_ID, "Anfragen", NotificationManager.IMPORTANCE_HIGH).apply {
                description = "Anfragen nach einem Lebenszeichen. Standardmäßig ohne Ton und Vibration."
                setSound(null, null)
                enableVibration(false)
                enableLights(false)
                setShowBadge(false)
                lockscreenVisibility = android.app.Notification.VISIBILITY_PUBLIC
            }
        manager(context).createNotificationChannel(channel)
    }

    fun canUseFullScreenIntent(context: Context): Boolean =
        if (Build.VERSION.SDK_INT >= 34) manager(context).canUseFullScreenIntent() else true

    fun show(context: Context, requestId: String, source: String) {
        val app = context.applicationContext
        ensureChannel(app)

        val notificationsAllowed = NotificationManagerCompat.from(app).areNotificationsEnabled()
        val fullScreenAllowed = canUseFullScreenIntent(app)
        val power = app.getSystemService(Context.POWER_SERVICE) as PowerManager
        val keyguard = app.getSystemService(Context.KEYGUARD_SERVICE) as KeyguardManager
        val state =
            "Quelle=$source, Benachrichtigungen=$notificationsAllowed, Vollbild erlaubt=$fullScreenAllowed, " +
                "Bildschirm an=${power.isInteractive}, gesperrt=${keyguard.isKeyguardLocked}"

        if (!notificationsAllowed) {
            EventLog.add(app, requestId, "not-shown", "Benachrichtigungen sind nicht erlaubt. $state")
            return
        }

        val fullScreenIntent =
            Intent(app, RequestActivity::class.java)
                .setData(Uri.parse("lebenszeichen://request/$requestId/fullscreen"))
                .putExtra(EXTRA_REQUEST_ID, requestId)
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
                .setContentTitle("Lebenszeichen?")
                .setContentText("Antippen oder wegwischen genügt.")
                // A reminder is held back by "do not disturb" unless the user allows reminders.
                .setCategory(NotificationCompat.CATEGORY_REMINDER)
                // Before Android 8 there are no channels: priority and defaults apply instead.
                .setPriority(NotificationCompat.PRIORITY_MAX)
                .setDefaults(0)
                .setSound(null)
                .setVibrate(null)
                .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
                .setWhen(System.currentTimeMillis())
                .setShowWhen(true)
                .setAutoCancel(true)
                .setContentIntent(reactionIntent(app, requestId, Requests.REACTION_TAP))
                // Fires when the user swipes the notification away, also with "clear all",
                // but not when the app cancels it.
                .setDeleteIntent(reactionIntent(app, requestId, Requests.REACTION_DISMISS))
                .setFullScreenIntent(fullScreen, true)
                .build()

        try {
            manager(app).notify(requestId, NOTIFICATION_ID, notification)
            EventLog.add(app, requestId, "shown", state)
        } catch (e: SecurityException) {
            EventLog.add(app, requestId, "not-shown", "${e.message}. $state")
        }
    }

    fun cancel(context: Context, requestId: String) {
        manager(context).cancel(requestId, NOTIFICATION_ID)
    }

    private fun reactionIntent(context: Context, requestId: String, reaction: String): PendingIntent {
        val intent =
            Intent(context, ReactionReceiver::class.java)
                // The data URI makes the pending intents of different requests distinct.
                .setData(Uri.parse("lebenszeichen://request/$requestId/$reaction"))
                .putExtra(EXTRA_REQUEST_ID, requestId)
                .putExtra(EXTRA_REACTION, reaction)
        return PendingIntent.getBroadcast(
            context,
            0,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }
}
