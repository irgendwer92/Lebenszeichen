//! Tauri shell of Lebenszeichen.
//!
//! In milestone M1 this is a throwaway prototype for Android. The interface
//! only shows state and settings; everything that has to work while the app
//! is not running lives in the Kotlin layer (`app/android`), which calls the
//! Rust core through [`jni_bridge`] without starting Tauri.

#[cfg(target_os = "android")]
mod jni_bridge;

/// Starts the Tauri application.
///
/// # Panics
///
/// Panics if Tauri cannot start, for example without a WebView.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_lebenszeichen::init())
        .run(tauri::generate_context!())
        .expect("error while running Lebenszeichen");
}
