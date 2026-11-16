package io.github.irgendwer92.lebenszeichen.droid

import android.Manifest
import android.app.Activity
import android.app.NotificationManager
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import android.webkit.WebView
import androidx.activity.result.ActivityResult
import androidx.core.app.NotificationManagerCompat
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.util.Locale
import org.json.JSONObject
import org.unifiedpush.android.connector.UnifiedPush

/**
 * Commands of the interface that only Android has: wake signals,
 * permissions, the notifications and the event log. Nothing here is needed
 * to receive and answer a request; that works without this class and
 * without the WebView.
 */
@TauriPlugin
class LebenszeichenPlugin(private val activity: Activity) : Plugin(activity) {
    /**
     * Also renews the notification channel, so its name follows a change of
     * the language. UnifiedPush recommends to renew the registration regularly, so that a
     * removed distributor is noticed. Done on every start of the interface.
     */
    override fun load(webView: WebView) {
        RequestPresenter.ensureChannel(activity)
        try {
            if (UnifiedPush.getAckDistributor(activity) != null) {
                UnifiedPush.register(activity, messageForDistributor = MESSAGE_FOR_DISTRIBUTOR)
            }
        } catch (e: Exception) {
            EventLog.add(activity, Requests.NO_REQUEST, "error", "registration not renewed: $e")
        }
    }

    /**
     * In the background Android freezes the process but keeps its sockets;
     * the relay would then see the device as connected and not wake it.
     * So the live connection ends here and starts again on return.
     */
    override fun onPause() {
        CoreBridge.setForeground(false)
    }

    override fun onResume() {
        CoreBridge.setForeground(true)
    }

    /**
     * Makes the notifications follow the state file, after the interface
     * fetched, answered or changed something. Off the main thread, because
     * the core reads the file.
     */
    @Command
    fun refreshRequests(invoke: Invoke) {
        val app = activity.applicationContext
        Thread {
            Requests.refresh(app, "app")
            invoke.resolve()
        }.start()
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
        result.put("notificationsEnabled", NotificationManagerCompat.from(activity).areNotificationsEnabled())
        result.put("channelImportance", channelImportance)
        result.put("fullScreenIntentAllowed", RequestPresenter.canUseFullScreenIntent(activity))
        result.put("batteryUnrestricted", power.isIgnoringBatteryOptimizations(activity.packageName))
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
            EventLog.add(activity, Requests.NO_REQUEST, "registration-failed", "no distributor installed")
            invoke.reject(Texts.get(activity, R.string.lz_error_no_distributor))
            return
        }
        try {
            UnifiedPush.saveDistributor(activity, chosen)
            UnifiedPush.register(activity, messageForDistributor = MESSAGE_FOR_DISTRIBUTOR)
            EventLog.add(activity, Requests.NO_REQUEST, "register", "distributor $chosen")
            invoke.resolve()
        } catch (e: Exception) {
            EventLog.add(activity, Requests.NO_REQUEST, "registration-failed", e.toString())
            invoke.reject(Texts.get(activity, R.string.lz_error_register_failed, e.toString()))
        }
    }

    @Command
    fun unregister(invoke: Invoke) {
        try {
            UnifiedPush.unregister(activity)
            Store.setEndpoint(activity, "")
            EventLog.add(activity, Requests.NO_REQUEST, "unregister", "unregistered by the app")
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject(Texts.get(activity, R.string.lz_error_unregister_failed, e.toString()))
        }
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
                invoke.reject(Texts.get(activity, R.string.lz_error_open_settings))
            }
        }
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

    /** Opens the camera to scan a code; the answer comes in [scanResult]. */
    @Command
    fun scanCode(invoke: Invoke) {
        startActivityForResult(invoke, Intent(activity, ScanActivity::class.java), "scanResult")
    }

    @ActivityCallback
    private fun scanResult(invoke: Invoke, result: ActivityResult) {
        val data = result.data
        val error = data?.getStringExtra(ScanActivity.EXTRA_ERROR)
        if (error != null) {
            invoke.reject(error)
            return
        }
        val answer = JSObject()
        val code = if (result.resultCode == Activity.RESULT_OK) data?.getStringExtra(ScanActivity.EXTRA_CODE) else null
        answer.put("code", code ?: JSONObject.NULL)
        answer.put("kind", code?.let { codeKind(it) } ?: JSONObject.NULL)
        invoke.resolve(answer)
    }

    /** The kind of a code by its prefix, like `CodeKind::of` in the core. */
    private fun codeKind(code: String): String? {
        val text = code.trim().uppercase(Locale.ROOT)
        return when {
            text.startsWith("LZD1-") -> "device"
            text.startsWith("LZS1-") -> "setup"
            text.startsWith("LZ2-") -> "invitation"
            text.startsWith("LZ1-") -> "old_invitation"
            else -> null
        }
    }

    /** Opens the share sheet with a text; resolves when it is started. */
    @Command
    fun shareText(invoke: Invoke) {
        val text = invoke.getArgs().getString("text", "") ?: ""
        val send =
            Intent(Intent.ACTION_SEND)
                .setType("text/plain")
                .putExtra(Intent.EXTRA_TEXT, text)
        try {
            val chooser = Intent.createChooser(send, Texts.get(activity, R.string.lz_share_chooser_title))
            if (activity.isFinishing) {
                invoke.reject(Texts.get(activity, R.string.lz_error_share_failed))
                return
            }
            activity.startActivity(chooser)
            invoke.resolve()
        } catch (e: ActivityNotFoundException) {
            invoke.reject(Texts.get(activity, R.string.lz_error_share_failed))
        }
    }

    /**
     * A device that was just set up as an additional device registers its
     * push address on all its mailboxes: the stored endpoint is sent again.
     */
    @Command
    fun refreshPush(invoke: Invoke) {
        val endpoint = Store.endpoint(activity)
        if (endpoint.isNotEmpty()) {
            PushUrlWorker.enqueue(activity.applicationContext, endpoint)
        }
        invoke.resolve()
    }

    companion object {
        private const val NTFY_PACKAGE = "io.heckel.ntfy"
        /** The name of the app, as the distributor shows it; not translated. */
        private const val MESSAGE_FOR_DISTRIBUTOR = "Lebenszeichen"
        private const val PERMISSION_REQUEST_CODE = 4711
        private const val MAX_LOG_LINES = 400
    }
}
