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
struct AndroidLayer<R: Runtime>(tauri::plugin::PluginHandle<R>);

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

/// Shows the notification of an answer to an own request
/// (`LebenszeichenPlugin.showAnswer`). Does nothing on other platforms.
///
/// # Errors
///
/// If the Kotlin layer is missing or rejects the call.
pub fn show_answer<R: Runtime>(
    app: &tauri::AppHandle<R>,
    answer: impl serde::Serialize,
) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        use tauri::Manager;
        let layer = app
            .try_state::<AndroidLayer<R>>()
            .ok_or_else(|| "no Android layer".to_owned())?;
        layer
            .0
            .run_mobile_plugin::<serde::de::IgnoredAny>("showAnswer", answer)
            .map(drop)
            .map_err(|error| error.to_string())
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, answer);
        Ok(())
    }
}
