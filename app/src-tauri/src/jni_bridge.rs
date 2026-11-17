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

use jni::objects::{JByteBuffer, JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jint, jlong, jstring};
use jni::{JNIEnv, JavaVM};
use lz_client::i18n::{Arg, Language};
use serde::Serialize;

use crate::background;
use crate::lang;

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
    rustls_platform_verifier::android::init_with_env(env, context).map_err(|e| {
        lang::texts().text(
            "app-error-certificates",
            &[("error", Arg::Text(&e.to_string()))],
        )
    })
}

/// The first locale of the application context as a language tag, such as
/// `de-DE`: `context.getResources().getConfiguration().getLocales().get(0)
/// .toLanguageTag()`.
fn locale_tag(env: &mut JNIEnv<'_>, context: &JObject<'_>) -> Result<String, String> {
    let result = (|| -> jni::errors::Result<String> {
        let resources = env
            .call_method(
                context,
                "getResources",
                "()Landroid/content/res/Resources;",
                &[],
            )?
            .l()?;
        let configuration = env
            .call_method(
                &resources,
                "getConfiguration",
                "()Landroid/content/res/Configuration;",
                &[],
            )?
            .l()?;
        let locales = env
            .call_method(
                &configuration,
                "getLocales",
                "()Landroid/os/LocaleList;",
                &[],
            )?
            .l()?;
        let locale = env
            .call_method(&locales, "get", "(I)Ljava/util/Locale;", &[JValue::Int(0)])?
            .l()?;
        let tag = env
            .call_method(&locale, "toLanguageTag", "()Ljava/lang/String;", &[])?
            .l()?;
        Ok(env.get_string(&JString::from(tag))?.into())
    })();
    result.map_err(|error| {
        // A pending Java exception would break the next call.
        let _ = env.exception_clear();
        error.to_string()
    })
}

/// The language of the system on Android; English if it has no translation
/// or the locale cannot be read.
fn system_language(env: &mut JNIEnv<'_>, context: &JObject<'_>) -> Language {
    locale_tag(env, context)
        .ok()
        .and_then(|tag| Language::from_tag(&tag))
        .unwrap_or_default()
}

/// The language in effect for the state file at `state`: the setting of this
/// device, or the one of the system.
fn effective_language(
    env: &mut JNIEnv<'_>,
    context: &JObject<'_>,
    state: &std::path::Path,
) -> Language {
    lang::effective(lang::load(&lang::settings_path(state)), || {
        system_language(env, context)
    })
}

/// The language of the system, for the Tauri side.
pub fn tauri_system_language() -> Language {
    with_tauri_context(|env, context| Ok(system_language(env, context))).unwrap_or_default()
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
        // The language first, so that the errors of the setup have it too.
        let path = state_path(env, context).and_then(|path| {
            lang::set_current(effective_language(env, context, &path));
            init_tls(env, context).map(|()| path)
        });
        match path {
            Ok(path) => serde_json::to_string(&run(env, path)),
            Err(error) => serde_json::to_string(&Failure { error }),
        }
    }))
    .unwrap_or_else(|_| {
        serde_json::to_string(&Failure {
            error: lang::texts().text("app-error-internal", &[]),
        })
    })
    .unwrap_or_else(|_| {
        serde_json::to_string(&Failure {
            error: lang::texts().text("app-error-answer-unreadable", &[]),
        })
        .unwrap_or_default()
    });
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

/// `CoreBridge.nativeReportState(context, action, token, reason, minutes): String`
/// – a state report from another app (`StateReceiver`). `action` is `"set"`
/// or `"clear"`; `minutes` is the text of the extra, whatever type the
/// sender used. Answers `{"applied":…,"report_wait":…}`; a report that is
/// not accepted is dropped without saying why.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_irgendwer92_lebenszeichen_droid_CoreBridge_nativeReportState<
    'local,
>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    context: JObject<'local>,
    action: JString<'local>,
    token: JString<'local>,
    reason: JString<'local>,
    minutes: JString<'local>,
) -> jstring {
    entry(&mut env, &context, |env, path| {
        let action = optional_string(env, &action);
        let token = optional_string(env, &token);
        let reason = optional_string(env, &reason);
        let minutes = optional_string(env, &minutes);
        let call = match action.as_deref() {
            Some("set") => background::External::Set {
                reason: reason.as_deref(),
                minutes: minutes.as_deref(),
            },
            Some("clear") => background::External::Clear,
            _ => {
                return background::Applied {
                    applied: false,
                    report_wait: None,
                };
            }
        };
        background::report_from_app(&path, token.as_deref(), call)
    })
}

/// `CoreBridge.nativeLanguage(context): String?` – the language in effect,
/// `"en"` or `"de"`: the setting of this device, or for `system` the first
/// locale of the context; languages without a translation give `"en"`. Used
/// to localize the notifications and the full-screen display. `null` only if
/// the files directory cannot be found or on an internal error, like the
/// other entry points.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_irgendwer92_lebenszeichen_droid_CoreBridge_nativeLanguage<
    'local,
>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    context: JObject<'local>,
) -> jstring {
    let tag = catch_unwind(AssertUnwindSafe(|| {
        let path = state_path(&mut env, &context).ok()?;
        Some(effective_language(&mut env, &context, &path).tag())
    }))
    .ok()
    .flatten();
    match tag {
        Some(tag) => env
            .new_string(tag)
            .map(|text| text.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        None => std::ptr::null_mut(),
    }
}

/// `CoreBridge.nativeDecodeQr(buffer, width, height, rowStride): String?`
///
/// Looks for a code (invitation, device or setup code) in the brightness plane of a camera frame. The
/// buffer must be direct (CameraX planes are); it is read in place, never
/// copied or kept. `null` if there is none.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_irgendwer92_lebenszeichen_droid_CoreBridge_nativeDecodeQr<
    'local,
>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    buffer: JByteBuffer<'local>,
    width: jint,
    height: jint,
    row_stride: jint,
) -> jstring {
    let found = catch_unwind(AssertUnwindSafe(|| {
        let address = env.get_direct_buffer_address(&buffer).ok()?;
        let capacity = env.get_direct_buffer_capacity(&buffer).ok()?;
        if address.is_null() {
            return None;
        }
        // SAFETY: address and capacity describe the memory of a direct
        // buffer that the caller keeps alive during this call, and nothing
        // writes to it meanwhile (CameraX holds the frame until it is closed).
        let luma = unsafe { std::slice::from_raw_parts(address, capacity) };
        lz_client::qr::decode_code(
            luma,
            usize::try_from(width).ok()?,
            usize::try_from(height).ok()?,
            usize::try_from(row_stride).ok()?,
        )
    }))
    .ok()
    .flatten();
    match found {
        Some(text) => env
            .new_string(text)
            .map(|text| text.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        None => std::ptr::null_mut(),
    }
}

/// `CoreBridge.nativeSetForeground(on)`: the app went to the background or
/// came back; the live connection of the window follows.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_irgendwer92_lebenszeichen_droid_CoreBridge_nativeSetForeground<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    on: jboolean,
) {
    let _ = catch_unwind(|| crate::commands::set_foreground(on != 0));
}
