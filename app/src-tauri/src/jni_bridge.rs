//! JNI entry points for the Kotlin layer.
//!
//! These functions are called by `CoreBridge.kt` after it has loaded this
//! library with `System.loadLibrary`. They must not touch Tauri: when a wake
//! signal starts the process there is no activity, no WebView and no Tauri
//! runtime. They only forward to `lz-core`.

use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::jstring;

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
