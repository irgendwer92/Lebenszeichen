package io.github.irgendwer92.lebenszeichen.droid

import android.content.Context
import android.content.res.Configuration
import androidx.annotation.StringRes
import java.util.Locale

/**
 * Looks up texts in the language the user chose in the app, which may
 * differ from the language of the system. Notifications, channels and the
 * activities use it, because Tauri and its WebView may not be running.
 *
 * Reads the language through [CoreBridge.language] on every call (a small
 * local file, no network), so a change in the settings applies at once.
 */
object Texts {
    /** A context whose resources are in the language of [CoreBridge.language]. */
    fun localized(context: Context): Context {
        val config =
            Configuration(context.resources.configuration).apply {
                setLocale(Locale.forLanguageTag(CoreBridge.language(context)))
            }
        return context.createConfigurationContext(config)
    }

    fun get(context: Context, @StringRes id: Int, vararg args: Any): String = localized(context).getString(id, *args)
}
