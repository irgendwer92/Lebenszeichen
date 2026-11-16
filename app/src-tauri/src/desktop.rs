//! Desktop side of the app (M3): tray, notifications and foreground
//! windows. The commands of the interface are in `commands`.

use std::path::PathBuf;

#[cfg(any(target_os = "linux", windows))]
mod alert;
#[cfg(any(target_os = "linux", windows))]
mod notify;

use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WindowEvent, Wry};

use tauri_plugin_autostart::ManagerExt;

use crate::{lang, settings};

use crate::commands::spawn_sync;

/// Label of the main window.
const MAIN_WINDOW: &str = "main";

/// Launch argument of "Start at login": the app goes straight into the tray.
pub const HIDDEN_ARG: &str = "--hidden";

/// What only the desktop keeps.
pub struct Desktop {
    #[cfg(any(target_os = "linux", windows))]
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
    #[cfg(any(target_os = "linux", windows))]
    if let Some(desktop) = app.try_state::<Desktop>()
        && let Some(notifier) = &desktop.notifier
    {
        notifier.poke();
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    let _ = app;
}

/// Tray, closing into the tray, and the display of requests, for the state
/// file at `path`.
pub fn setup(app: &mut tauri::App, path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let tray = build_tray(app, path.clone())?;
    keep_running_when_closed(app);
    show_unless_hidden(app);
    offer_autostart(app, &path);
    app.manage(Desktop {
        tray,
        #[cfg(any(target_os = "linux", windows))]
        notifier: notify::start(app.handle().clone(), path),
    });
    #[cfg(not(any(target_os = "linux", windows)))]
    let _ = path;
    Ok(())
}

/// Shows the main window unless the app was started with `--hidden`. The
/// window is created invisible (`visible: false` in the platform
/// configuration), so a start into the tray never flashes a window.
fn show_unless_hidden(app: &tauri::App) {
    if !std::env::args().any(|arg| arg == HIDDEN_ARG) {
        show_window(app.handle());
    }
}

/// Turns "Start at login" on at the very first start, once; if the person
/// turns it off later, it stays off. The flag is only set when it worked, so
/// a failure is tried again at the next start.
fn offer_autostart(app: &tauri::App, state: &std::path::Path) {
    // Development builds never put themselves into autostart.
    if cfg!(debug_assertions) {
        return;
    }
    let file = settings::path(state);
    if settings::load(&file).autostart_offered {
        return;
    }
    let outcome = app
        .autolaunch()
        .enable()
        .map_err(|error| error.to_string())
        .and_then(|()| {
            settings::update(&file, |settings| settings.autostart_offered = true)
                .map_err(|error| error.to_string())
        });
    if let Err(error) = outcome {
        eprintln!(
            "{}: {error}",
            lang::texts().text("app-error-autostart", &[])
        );
    }
}

/// Brings the window back, for the tray and for a second start of the app.
pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Puts the app into the tray. On Windows a left click shows the window and
/// the menu stays on the right click; tray icons on Linux get no clicks, so
/// there everything goes through the menu.
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
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        })
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
#[cfg(any(target_os = "linux", windows))]
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
    #[cfg(any(target_os = "linux", windows))]
    {
        let (notifier, number) = alert_of(&window, &state)?;
        let t = lang::texts();
        let name = notifier
            .alert_name(number)
            .ok_or_else(|| t.text("app-error-request-closed", &[]))?;
        Ok(AlertText {
            lang: t.language().tag(),
            title: t.text(
                "app-alert-title",
                &[("name", lz_client::i18n::Arg::Text(&name))],
            ),
            hint: t.text("app-alert-hint", &[]),
        })
    }
    #[cfg(not(any(target_os = "linux", windows)))]
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
    #[cfg(any(target_os = "linux", windows))]
    {
        let (notifier, number) = alert_of(&window, &state)?;
        notifier.alert_clicked(number);
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (window, state);
        Err(lang::texts().text("app-error-no-alert-here", &[]))
    }
}
