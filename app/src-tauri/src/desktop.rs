//! Desktop side of the app (M3): tray, notifications and foreground
//! windows. The commands of the interface are in `commands`.

use std::path::PathBuf;

#[cfg(target_os = "linux")]
mod alert;
#[cfg(target_os = "linux")]
mod notify;

use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, WindowEvent};

use crate::commands::spawn_sync;

/// Label of the main window.
const MAIN_WINDOW: &str = "main";

/// What only the desktop keeps.
pub struct Desktop {
    #[cfg(target_os = "linux")]
    notifier: Option<notify::Notifier>,
}

/// Requests may have come or gone; the notifications follow.
pub fn poke(app: &AppHandle) {
    #[cfg(target_os = "linux")]
    if let Some(desktop) = app.try_state::<Desktop>()
        && let Some(notifier) = &desktop.notifier
    {
        notifier.poke();
    }
    #[cfg(not(target_os = "linux"))]
    let _ = app;
}

/// Tray, closing into the tray, and the display of requests, for the state
/// file at `path`.
pub fn setup(app: &mut tauri::App, path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    build_tray(app, path.clone())?;
    keep_running_when_closed(app);
    app.manage(Desktop {
        #[cfg(target_os = "linux")]
        notifier: notify::start(app.handle().clone(), path),
    });
    #[cfg(not(target_os = "linux"))]
    let _ = path;
    Ok(())
}

/// Brings the window back, for the tray and for a second start of the app.
pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Puts the app into the tray. Tray icons on Linux get no clicks, so
/// everything goes through the menu.
fn build_tray(app: &tauri::App, path: PathBuf) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Fenster zeigen", true, None::<&str>)?;
    let sync = MenuItem::with_id(app, "sync", "Jetzt abrufen", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Beenden", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &sync, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Lebenszeichen")
        .menu(&menu)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "show" => show_window(app),
            "sync" => spawn_sync(app.clone(), path.clone()),
            "quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// Closing the window only hides it, so that fetching goes on. The app ends
/// through "Beenden" in the tray.
fn keep_running_when_closed(app: &tauri::App) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let hide = window.clone();
        window.on_window_event(move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = hide.hide();
            }
        });
    }
}

/// Text of a foreground window.
#[derive(Serialize)]
pub struct AlertText {
    title: String,
    hint: &'static str,
}

/// The request a foreground window shows, from its label. A window can only
/// ever see and answer its own request.
#[cfg(target_os = "linux")]
fn alert_of<'a>(
    window: &tauri::WebviewWindow,
    state: &'a tauri::State<'_, Desktop>,
) -> Result<(&'a notify::Notifier, u32), String> {
    let number = alert::number(window.label()).ok_or("Kein Vordergrund-Fenster.")?;
    let notifier = state
        .notifier
        .as_ref()
        .ok_or("Keine Anzeige von Anfragen.")?;
    Ok((notifier, number))
}

#[tauri::command]
pub fn alert_text(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Desktop>,
) -> Result<AlertText, String> {
    #[cfg(target_os = "linux")]
    {
        let (notifier, number) = alert_of(&window, &state)?;
        let name = notifier
            .alert_name(number)
            .ok_or("Die Anfrage ist nicht mehr offen.")?;
        Ok(AlertText {
            title: format!("{name} fragt nach einem Lebenszeichen"),
            hint: "Klicken oder Schließen sendet dein Lebenszeichen.",
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (window, state);
        Err("Vordergrund-Fenster gibt es hier noch nicht.".into())
    }
}

/// A click into a foreground window: sends the sign of life for its request.
#[tauri::command]
pub fn alert_answer(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Desktop>,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let (notifier, number) = alert_of(&window, &state)?;
        notifier.alert_clicked(number);
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (window, state);
        Err("Vordergrund-Fenster gibt es hier noch nicht.".into())
    }
}
