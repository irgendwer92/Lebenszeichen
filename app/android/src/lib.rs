//! Rust side of the Android layer.
//!
//! All logic is Kotlin (`kotlin/`). This crate only exists so that Tauri
//! adds the Android library module to the generated Gradle project and
//! routes the commands of the interface to the Kotlin class.

use tauri::{
    Runtime,
    plugin::{Builder, TauriPlugin},
};

#[cfg(target_os = "android")]
const ANDROID_PACKAGE: &str = "io.github.irgendwer92.lebenszeichen.droid";

/// Keeps the handle of the Kotlin plugin alive for the lifetime of the app.
#[cfg(target_os = "android")]
struct AndroidLayer<R: Runtime>(#[allow(dead_code)] tauri::plugin::PluginHandle<R>);

/// Initializes the plugin. On other platforms than Android it does nothing.
#[must_use]
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("lebenszeichen")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                use tauri::Manager;
                let handle =
                    _api.register_android_plugin(ANDROID_PACKAGE, "LebenszeichenPlugin")?;
                _app.manage(AndroidLayer(handle));
            }
            Ok(())
        })
        .build()
}
