//! JNI entry points for the Kotlin layer (`CoreBridge.kt`), and the Android
//! setup of the certificate check.
//!
//! The entry points run after a wake signal or a reaction to a notification,
//! often in a process without activity, WebView and Tauri runtime. They only
//! call `background` and hand its answer over as JSON. Every one of them
//! takes the application context: from it come the state file and the setup
//! of `rustls-platform-verifier`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use jni::objects::{JClass, JObject, JString};
use jni::sys::{jlong, jstring};
use jni::{JNIEnv, JavaVM};
use serde::Serialize;

use crate::background;

/// Name of the state file in the files directory of the app.
const STATE_FILE: &str = "device.json";

/// The state file of this device: `device.json` in `Context.getFilesDir()`.
/// Tauri and the background path both use this function, so they always
/// agree.
fn state_path(env: &mut JNIEnv<'_>, context: &JObject<'_>) -> Result<PathBuf, String> {
    let dir = env
        .call_method(context, "getFilesDir", "()Ljava/io/File;", &[])
        .and_then(|value| value.l())
        .map_err(|e| e.to_string())?;
    let absolute = env
        .call_method(&dir, "getAbsolutePath", "()Ljava/lang/String;", &[])
        .and_then(|value| value.l())
        .map_err(|e| e.to_string())?;
    let text: String = env
        .get_string(&JString::from(absolute))
        .map_err(|e| e.to_string())?
        .into();
    Ok(PathBuf::from(text).join(STATE_FILE))
}

/// Sets up `rustls-platform-verifier`; later calls do nothing. Without it,
/// every TLS connection fails.
fn init_tls(env: &mut JNIEnv<'_>, context: &JObject<'_>) -> Result<(), String> {
    // SAFETY: the reference stays valid for this call; `init_with_env`
    // keeps a global reference of its own.
    let context = unsafe { JObject::from_raw(context.as_raw()) };
    rustls_platform_verifier::android::init_with_env(env, context)
        .map_err(|e| format!("Zertifikatsprüfung nicht eingerichtet: {e}"))
}

/// Runs `with` on the context of the running app that Tao keeps
/// (`ndk_glue::onCreate`), for the Tauri side.
fn with_tauri_context<T>(
    with: impl FnOnce(&mut JNIEnv<'_>, &JObject<'_>) -> Result<T, String>,
) -> Result<T, String> {
    let android = ndk_context::android_context();
    // SAFETY: Tao stores the Java VM and a global reference to the
    // application context before the app starts; both stay valid for the
    // life of the process. Neither type frees anything when dropped.
    let vm = unsafe { JavaVM::from_raw(android.vm().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let context = unsafe { JObject::from_raw(android.context().cast()) };
    with(&mut env, &context)
}

/// Sets up the certificate check for the running app and returns its state
/// file, for `commands::setup`.
pub fn tauri_setup() -> Result<PathBuf, String> {
    with_tauri_context(|env, context| {
        init_tls(env, context)?;
        state_path(env, context)
    })
}

#[derive(Serialize)]
struct Failure {
    error: String,
}

/// Runs an entry point: TLS set up, state file found, panics caught. A
/// panic must not cross into Java; it becomes an error in the answer.
fn entry<T: Serialize>(
    env: &mut JNIEnv<'_>,
    context: &JObject<'_>,
    run: impl FnOnce(&mut JNIEnv<'_>, PathBuf) -> T,
) -> jstring {
    let answer = catch_unwind(AssertUnwindSafe(|| {
        let path = init_tls(env, context).and_then(|()| state_path(env, context));
        match path {
            Ok(path) => serde_json::to_string(&run(env, path)),
            Err(error) => serde_json::to_string(&Failure { error }),
        }
    }))
    .unwrap_or_else(|_| {
        serde_json::to_string(&Failure {
            error: "Interner Fehler im Kern".into(),
        })
    })
    .unwrap_or_else(|_| r#"{"error":"Antwort nicht lesbar"}"#.into());
    env.new_string(answer)
        .map(|text| text.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Reads a Java string; a null reference becomes `None`.
fn optional_string(env: &mut JNIEnv<'_>, value: &JString<'_>) -> Option<String> {
    if value.is_null() {
        return None;
    }
    env.get_string(value).map(Into::into).ok()
}

/// `CoreBridge.nativeFetch(context): String` – fetch, then the open requests.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_irgendwer92_lebenszeichen_droid_CoreBridge_nativeFetch<
    'local,
>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    context: JObject<'local>,
) -> jstring {
    entry(&mut env, &context, |_, path| background::fetch(&path))
}

/// `CoreBridge.nativeOpenRequests(context): String` – without the network.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_irgendwer92_lebenszeichen_droid_CoreBridge_nativeOpenRequests<
    'local,
>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    context: JObject<'local>,
) -> jstring {
    entry(&mut env, &context, |_, path| {
        background::open_requests(&path)
    })
}

/// `CoreBridge.nativeRespond(context, name, since): String`
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_irgendwer92_lebenszeichen_droid_CoreBridge_nativeRespond<
    'local,
>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    context: JObject<'local>,
    name: JString<'local>,
    since: jlong,
) -> jstring {
    entry(&mut env, &context, |env, path| {
        let name = optional_string(env, &name).unwrap_or_default();
        background::respond(&path, &name, u64::try_from(since).unwrap_or(0))
    })
}

/// `CoreBridge.nativeSetPushUrl(context, url: String?): String` – `null` removes it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_irgendwer92_lebenszeichen_droid_CoreBridge_nativeSetPushUrl<
    'local,
>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    context: JObject<'local>,
    url: JString<'local>,
) -> jstring {
    entry(&mut env, &context, |env, path| {
        let url = optional_string(env, &url);
        background::set_push_url(&path, url.as_deref())
    })
}
