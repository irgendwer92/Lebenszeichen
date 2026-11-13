package io.github.irgendwer92.lebenszeichen.droid

import android.Manifest
import android.app.Activity
import android.app.NotificationManager
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.provider.Settings
import android.webkit.WebView
import androidx.core.app.NotificationManagerCompat
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import org.unifiedpush.android.connector.UnifiedPush

/**
 * Commands for the interface of the prototype. Nothing here is needed to
 * receive and answer a request; that works without this class and without
 * the WebView.
 */
@TauriPlugin
class LebenszeichenPlugin(private val activity: Activity) : Plugin(activity) {
    /**
     * UnifiedPush recommends to renew the registration regularly, so that a
     * removed distributor is noticed. Done on every start of the interface.
     */
    override fun load(webView: WebView) {
        RequestPresenter.ensureChannel(activity)
        try {
            if (UnifiedPush.getAckDistributor(activity) != null) {
                UnifiedPush.register(activity, messageForDistributor = MESSAGE_FOR_DISTRIBUTOR)
            }
        } catch (e: Exception) {
            EventLog.add(activity, PushServiceImpl.NO_REQUEST, "error", "Registrierung nicht erneuert: $e")
        }
    }

    @Command
    fun status(invoke: Invoke) {
        val power = activity.getSystemService(Context.POWER_SERVICE) as PowerManager
        val manager = activity.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        val channelImportance =
            if (Build.VERSION.SDK_INT >= 26) {
                manager.getNotificationChannel(RequestPresenter.CHANNEL_ID)?.importance ?: -1
            } else {
                -1
            }
        val result = JSObject()
        result.put("sdk", Build.VERSION.SDK_INT)
        result.put("device", "${Build.MANUFACTURER} ${Build.MODEL}, Android ${Build.VERSION.RELEASE}")
        result.put("distributors", JSArray(UnifiedPush.getDistributors(activity)))
        result.put("distributor", UnifiedPush.getSavedDistributor(activity) ?: "")
        result.put("distributorAcknowledged", UnifiedPush.getAckDistributor(activity) != null)
        result.put("endpoint", Store.endpoint(activity))
        result.put("answerUrl", Store.answerUrl(activity))
        result.put("notificationsEnabled", NotificationManagerCompat.from(activity).areNotificationsEnabled())
        result.put("channelImportance", channelImportance)
        result.put("fullScreenIntentAllowed", RequestPresenter.canUseFullScreenIntent(activity))
        result.put("batteryUnrestricted", power.isIgnoringBatteryOptimizations(activity.packageName))
        result.put("core", CoreBridge.describe("status", "ui"))
        invoke.resolve(result)
    }

    @Command
    fun register(invoke: Invoke) {
        val wanted = invoke.getArgs().getString("distributor", "") ?: ""
        val distributors = UnifiedPush.getDistributors(activity)
        val saved = UnifiedPush.getSavedDistributor(activity)
        val chosen =
            when {
                wanted in distributors -> wanted
                saved != null && saved in distributors -> saved
                NTFY_PACKAGE in distributors -> NTFY_PACKAGE
                else -> distributors.firstOrNull()
            }
        if (chosen == null) {
            EventLog.add(activity, PushServiceImpl.NO_REQUEST, "registration-failed", "kein Verteiler installiert")
            invoke.reject("Kein UnifiedPush-Verteiler gefunden. Bitte die ntfy-App installieren und einmal öffnen.")
            return
        }
        try {
            UnifiedPush.saveDistributor(activity, chosen)
            UnifiedPush.register(activity, messageForDistributor = MESSAGE_FOR_DISTRIBUTOR)
            EventLog.add(activity, PushServiceImpl.NO_REQUEST, "register", "Verteiler $chosen")
            invoke.resolve()
        } catch (e: Exception) {
            EventLog.add(activity, PushServiceImpl.NO_REQUEST, "registration-failed", e.toString())
            invoke.reject("Registrierung fehlgeschlagen: $e")
        }
    }

    @Command
    fun unregister(invoke: Invoke) {
        try {
            UnifiedPush.unregister(activity)
            Store.setEndpoint(activity, "")
            EventLog.add(activity, PushServiceImpl.NO_REQUEST, "unregister", "von der App abgemeldet")
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject("Abmelden fehlgeschlagen: $e")
        }
    }

    @Command
    fun setAnswerUrl(invoke: Invoke) {
        val url = (invoke.getArgs().getString("url", "") ?: "").trim()
        if (url.isNotEmpty() && !url.startsWith("https://") && !url.startsWith("http://")) {
            invoke.reject("Die Adresse muss mit https:// oder http:// beginnen.")
            return
        }
        Store.setAnswerUrl(activity, url)
        EventLog.add(activity, PushServiceImpl.NO_REQUEST, "answer-url", if (url.isEmpty()) "gelöscht" else url)
        invoke.resolve()
    }

    /** Runs the display path without a wake signal, optionally after a delay to lock the screen. */
    @Command
    fun triggerLocal(invoke: Invoke) {
        val delaySeconds = invoke.getArgs().getInteger("delaySeconds", 0).coerceIn(0, 120)
        val app = activity.applicationContext
        val requestId = Requests.newId()
        EventLog.add(app, requestId, "local-trigger", "Verzögerung $delaySeconds s")
        Handler(Looper.getMainLooper()).postDelayed(
            { Requests.open(app, requestId, "lokal") },
            delaySeconds * 1000L,
        )
        invoke.resolve()
    }

    /** The result is not awaited; the interface reads the state again afterwards. */
    @Command
    fun requestNotificationPermission(invoke: Invoke) {
        if (Build.VERSION.SDK_INT >= 33 &&
            activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            activity.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), PERMISSION_REQUEST_CODE)
        }
        invoke.resolve()
    }

    @Command
    fun openSettings(invoke: Invoke) {
        val target = invoke.getArgs().getString("target", "") ?: ""
        val packageUri = Uri.parse("package:${activity.packageName}")
        val appDetails = Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, packageUri)
        val intent =
            when (target) {
                "notifications" ->
                    if (Build.VERSION.SDK_INT >= 26) {
                        Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                            .putExtra(Settings.EXTRA_APP_PACKAGE, activity.packageName)
                    } else {
                        appDetails
                    }
                "fullscreen" ->
                    if (Build.VERSION.SDK_INT >= 34) {
                        Intent(Settings.ACTION_MANAGE_APP_USE_FULL_SCREEN_INTENT, packageUri)
                    } else {
                        appDetails
                    }
                "battery" -> Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, packageUri)
                else -> appDetails
            }
        try {
            activity.startActivity(intent)
            invoke.resolve()
        } catch (e: ActivityNotFoundException) {
            try {
                activity.startActivity(appDetails)
                invoke.resolve()
            } catch (e2: ActivityNotFoundException) {
                invoke.reject("Die Einstellungen ließen sich nicht öffnen.")
            }
        }
    }

    @Command
    fun copyText(invoke: Invoke) {
        val text = invoke.getArgs().getString("text", "") ?: ""
        val clipboard = activity.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        clipboard.setPrimaryClip(ClipData.newPlainText("Lebenszeichen", text))
        invoke.resolve()
    }

    @Command
    fun readLog(invoke: Invoke) {
        val result = JSObject()
        result.put("lines", JSArray(EventLog.read(activity, MAX_LOG_LINES)))
        invoke.resolve(result)
    }

    @Command
    fun clearLog(invoke: Invoke) {
        EventLog.clear(activity)
        invoke.resolve()
    }

    companion object {
        private const val NTFY_PACKAGE = "io.heckel.ntfy"
        private const val MESSAGE_FOR_DISTRIBUTOR = "Lebenszeichen"
        private const val PERMISSION_REQUEST_CODE = 4711
        private const val MAX_LOG_LINES = 400
    }
}
