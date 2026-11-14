package io.github.irgendwer92.lebenszeichen.droid

import android.content.Context
import java.nio.ByteBuffer
import org.json.JSONObject

/**
 * Calls into the Rust core through JNI.
 *
 * The native library is the one Tauri builds for the app. It is loaded here
 * directly, because after a wake signal no activity and no WebView exist and
 * Tauri is not running. The name must match `[lib] name` in
 * `app/src-tauri/Cargo.toml`; the functions are in
 * `app/src-tauri/src/jni_bridge.rs` and answer with JSON.
 *
 * Every call may block on the network; never call from the main thread.
 */
object CoreBridge {
    private const val LIBRARY = "lebenszeichen_lib"

    private val loadError: String? =
        try {
            System.loadLibrary(LIBRARY)
            null
        } catch (e: UnsatisfiedLinkError) {
            e.message ?: "UnsatisfiedLinkError"
        }

    @JvmStatic private external fun nativeFetch(context: Context): String?

    @JvmStatic private external fun nativeOpenRequests(context: Context): String?

    @JvmStatic private external fun nativeRespond(context: Context, name: String, since: Long): String?

    @JvmStatic private external fun nativeSetPushUrl(context: Context, url: String?): String?

    @JvmStatic private external fun nativeDecodeQr(buffer: ByteBuffer, width: Int, height: Int, rowStride: Int): String?

    /**
     * Looks for an invitation in the brightness plane of a camera frame.
     * [buffer] must be a direct buffer (the Y plane of CameraX is). It is
     * only read, in place. `null` if nothing was found or the core is
     * not available.
     */
    fun decodeQr(buffer: ByteBuffer, width: Int, height: Int, rowStride: Int): String? {
        if (loadError != null) return null
        return try {
            nativeDecodeQr(buffer, width, height, rowStride)
        } catch (e: UnsatisfiedLinkError) {
            null
        }
    }

    /** Fetches from the relays, then lists the open requests. */
    fun fetch(context: Context): JSONObject = call { nativeFetch(context.applicationContext) }

    /** The open requests as stored, without the network. */
    fun openRequests(context: Context): JSONObject = call { nativeOpenRequests(context.applicationContext) }

    /** Answers the request of [name] sent at [since]; see `background::Outcome`. */
    fun respond(context: Context, name: String, since: Long): JSONObject =
        call { nativeRespond(context.applicationContext, name, since) }

    /** Sets the push address, or removes it with `null`. */
    fun setPushUrl(context: Context, url: String?): JSONObject = call { nativeSetPushUrl(context.applicationContext, url) }

    /** Turns every way of failing into an answer with `error`. */
    private fun call(native: () -> String?): JSONObject {
        loadError?.let { return failure("Rust-Kern nicht geladen: $it") }
        return try {
            native()?.let { JSONObject(it) } ?: failure("Rust-Kern lieferte nichts")
        } catch (e: UnsatisfiedLinkError) {
            failure("Rust-Kern nicht erreichbar: ${e.message}")
        } catch (e: org.json.JSONException) {
            failure("Antwort des Rust-Kerns nicht lesbar: ${e.message}")
        }
    }

    private fun failure(error: String): JSONObject = JSONObject().put("error", error)
}

/**
 * The error of an answer of the core, or `null`. Not `optString`: that turns
 * a JSON `null` into the text "null" on Android.
 */
fun JSONObject.errorText(): String? = if (isNull("error")) null else optString("error").ifEmpty { null }
