//! JNI entry points for the Kotlin layer, and the Android setup of the
//! certificate check.
//!
//! The entry points are called by `CoreBridge.kt` after it has loaded this
//! library with `System.loadLibrary`. They must not touch Tauri: when a wake
//! signal starts the process there is no activity, no WebView and no Tauri
//! runtime. They only forward to `lz-core`.

use jni::objects::{JClass, JObject, JString};
use jni::sys::jstring;
use jni::{JNIEnv, JavaVM};

/// Sets up `rustls-platform-verifier` with the application context that Tao
/// keeps for the running app. Without it, every TLS connection fails. Later
/// calls do nothing.
pub fn init_tls() -> Result<(), String> {
    let android = ndk_context::android_context();
    // SAFETY: Tao stores the Java VM and a global reference to the
    // application context before the app starts (`ndk_glue::onCreate`);
    // both stay valid for the life of the process. Neither type frees
    // anything when dropped.
    let vm = unsafe { JavaVM::from_raw(android.vm().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let context = unsafe { JObject::from_raw(android.context().cast()) };
    rustls_platform_verifier::android::init_with_env(&mut env, context)
        .map_err(|e| format!("Zertifikatsprüfung nicht eingerichtet: {e}"))
}

/// `CoreBridge.marker(event: String, requestId: String): String`
///
/// Returns `null` if the result cannot be turned into a Java string; the
/// Kotlin side treats that as a failed call.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_irgendwer92_lebenszeichen_droid_CoreBridge_marker<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    event: JString<'local>,
    request_id: JString<'local>,
) -> jstring {
    let event = read_string(&mut env, &event);
    let request_id = read_string(&mut env, &request_id);
    let line = lz_core::prototype::marker(&event, &request_id);
    match env.new_string(line) {
        Ok(text) => text.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Reads a Java string; a null reference or broken string becomes empty.
fn read_string(env: &mut JNIEnv<'_>, value: &JString<'_>) -> String {
    env.get_string(value).map(Into::into).unwrap_or_default()
}
