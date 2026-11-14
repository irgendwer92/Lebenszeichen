//! Tauri shell of Lebenszeichen.
//!
//! The interface talks to `lz-client` through the commands in `commands`,
//! on the desktop (M3) and on Android (M4). The desktop adds tray,
//! notifications and foreground windows (`desktop`).
//!
//! On Android, a wake signal can start the process without activity and
//! WebView. That path lives in the Kotlin layer (`app/android`), which calls
//! Rust through `jni_bridge` without starting Tauri.

#[cfg(any(target_os = "android", test))]
mod background;
mod commands;
#[cfg(desktop)]
mod desktop;
#[cfg(target_os = "android")]
mod jni_bridge;

/// Builds the command handler from the commands both systems share and,
/// optionally, a few more.
macro_rules! handler {
    ($($extra:path),* $(,)?) => {
        tauri::generate_handler![
            commands::platform,
            commands::setup_state,
            commands::init_device,
            commands::overview,
            commands::sync,
            commands::invite,
            commands::accept,
            commands::confirm,
            commands::reject,
            commands::request,
            commands::respond,
            commands::remove,
            commands::set_rules,
            $($extra,)*
        ]
    };
}

/// Starts the Tauri application.
///
/// # Panics
///
/// Panics if Tauri cannot start, for example without a WebView.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // A second start only brings the window of the first one back; two
    // instances would fetch for the same state file twice.
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        desktop::show_window(app);
    }));
    let builder = builder
        .plugin(tauri_plugin_lebenszeichen::init())
        .setup(|app| {
            let path = commands::setup(app)?;
            #[cfg(desktop)]
            desktop::setup(app, path)?;
            #[cfg(not(desktop))]
            let _ = path;
            Ok(())
        });
    #[cfg(desktop)]
    let builder = builder.invoke_handler(handler![desktop::alert_text, desktop::alert_answer]);
    #[cfg(not(desktop))]
    let builder = builder.invoke_handler(handler![]);
    builder
        .run(tauri::generate_context!())
        .expect("error while running Lebenszeichen");
}
