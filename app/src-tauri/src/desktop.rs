//! Desktop side of the app (M3): tray, notifications and foreground
//! windows. The commands of the interface are in `commands`.

use std::path::PathBuf;

#[cfg(target_os = "linux")]
mod alert;
#[cfg(target_os = "linux")]
mod notify;

use lz_client::i18n::Arg;
use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, WindowEvent, Wry};

use crate::lang;

use crate::commands::spawn_sync;

/// Label of the main window.
const MAIN_WINDOW: &str = "main";

/// What only the desktop keeps.
pub struct Desktop {
    #[cfg(target_os = "linux")]
    notifier: Option<notify::Notifier>,
    tray: TrayItems,
}

/// The entries of the tray menu, to be relabeled when the language changes.
struct TrayItems {
    show: MenuItem<Wry>,
    sync: MenuItem<Wry>,
    quit: MenuItem<Wry>,
}

impl TrayItems {
    fn relabel(&self) {
        let t = lang::texts();
        let _ = self.show.set_text(t.text("app-tray-show", &[]));
        let _ = self.sync.set_text(t.text("app-tray-sync", &[]));
        let _ = self.quit.set_text(t.text("app-tray-quit", &[]));
    }
}

/// The language in effect changed: the tray menu follows.
pub fn language_changed(app: &AppHandle) {
    if let Some(desktop) = app.try_state::<Desktop>() {
        desktop.tray.relabel();
    }
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
    let tray = build_tray(app, path.clone())?;
    keep_running_when_closed(app);
    app.manage(Desktop {
        tray,
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
fn build_tray(app: &tauri::App, path: PathBuf) -> tauri::Result<TrayItems> {
    let t = lang::texts();
    let item = |id, text_id| MenuItem::with_id(app, id, t.text(text_id, &[]), true, None::<&str>);
    let show = item("show", "app-tray-show")?;
    let sync = item("sync", "app-tray-sync")?;
    let quit = item("quit", "app-tray-quit")?;
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
    Ok(TrayItems { show, sync, quit })
}

/// Closing the window only hides it, so that fetching goes on. The app ends
/// through "Quit" in the tray.
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
    /// The language in effect, for `<html lang>`.
    lang: &'static str,
    title: String,
    hint: String,
}

/// The request a foreground window shows, from its label. A window can only
/// ever see and answer its own request.
#[cfg(target_os = "linux")]
fn alert_of<'a>(
    window: &tauri::WebviewWindow,
    state: &'a tauri::State<'_, Desktop>,
) -> Result<(&'a notify::Notifier, u32), String> {
    let t = lang::texts();
    let number =
        alert::number(window.label()).ok_or_else(|| t.text("app-error-no-alert-window", &[]))?;
    let notifier = state
        .notifier
        .as_ref()
        .ok_or_else(|| t.text("app-error-no-alerts", &[]))?;
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
        let t = lang::texts();
        let name = notifier
            .alert_name(number)
            .ok_or_else(|| t.text("app-error-request-closed", &[]))?;
        Ok(AlertText {
            lang: t.language().tag(),
            title: t.text("app-alert-title", &[("name", Arg::Text(&name))]),
            hint: t.text("app-alert-hint", &[]),
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (window, state);
        Err(lang::texts().text("app-error-no-alert-here", &[]))
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
        Err(lang::texts().text("app-error-no-alert-here", &[]))
    }
}
