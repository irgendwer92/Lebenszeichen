//! Tauri shell of Lebenszeichen.
//!
//! On Android this is still the throwaway prototype of milestone M1: the
//! interface only shows state and settings; everything that has to work while
//! the app is not running lives in the Kotlin layer (`app/android`), which
//! calls the Rust core through `jni_bridge` without starting Tauri.
//!
//! On the desktop (milestone M3) the interface talks to `lz-client` through
//! the commands in `desktop`.

#[cfg(desktop)]
mod desktop;
#[cfg(target_os = "android")]
mod jni_bridge;

/// Starts the Tauri application.
///
/// # Panics
///
/// Panics if Tauri cannot start, for example without a WebView.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_lebenszeichen::init());
    #[cfg(desktop)]
    let builder = builder
        .setup(desktop::setup)
        .invoke_handler(tauri::generate_handler![
            desktop::setup_state,
            desktop::init_device,
            desktop::overview,
            desktop::sync,
            desktop::invite,
            desktop::accept,
            desktop::confirm,
            desktop::reject,
            desktop::request,
            desktop::respond,
            desktop::remove,
        ]);
    builder
        .run(tauri::generate_context!())
        .expect("error while running Lebenszeichen");
}
