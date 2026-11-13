package io.github.irgendwer92.lebenszeichen.droid

/**
 * Calls into the Rust core through JNI.
 *
 * The native library is the one Tauri builds for the app. It is loaded here
 * directly, because after a wake signal no activity and no WebView exist and
 * Tauri is not running. The name must match `[lib] name` in
 * `app/src-tauri/Cargo.toml`, the function is implemented in
 * `app/src-tauri/src/jni_bridge.rs`.
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

    @JvmStatic
    external fun marker(event: String, requestId: String): String?

    /**
     * Returns the marker line of the Rust core. If the core cannot be
     * reached, the returned text says so instead of hiding the failure.
     */
    fun describe(event: String, requestId: String): String {
        loadError?.let { return "rust-core-unavailable load-error event=$event request=$requestId ($it)" }
        return try {
            marker(event, requestId) ?: "rust-core-unavailable null-result event=$event request=$requestId"
        } catch (e: UnsatisfiedLinkError) {
            "rust-core-unavailable link-error event=$event request=$requestId (${e.message})"
        }
    }
}
