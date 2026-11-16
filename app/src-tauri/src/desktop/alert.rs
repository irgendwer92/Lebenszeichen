//! Foreground window for an open request, under X11 and on Windows (see
//! `docs/concept.md`):
//! above all other windows, on every workspace, and without keyboard focus,
//! so that a key pressed while typing elsewhere never counts as a sign of
//! life. A click into the window or closing it answers.
//!
//! Under Wayland an app may not bring itself to the front; there requests
//! come as notifications only. On Windows they come as notifications while
//! Do Not Disturb is on, so that the window respects it.

use tauri::{AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder, WindowEvent};

/// Labels of foreground windows; `capabilities/alert.json` matches them.
const PREFIX: &str = "alert-";

/// Size of the window in logical pixels.
const WIDTH: f64 = 420.0;
const HEIGHT: f64 = 240.0;

/// Shift of each further window, so that several stay visible.
const STACK_OFFSET: i32 = 32;

pub fn label(number: u32) -> String {
    format!("{PREFIX}{number}")
}

/// The number of a foreground window, from its label.
pub fn number(label: &str) -> Option<u32> {
    label.strip_prefix(PREFIX)?.parse().ok()
}

/// Whether the app runs on X11, where a window may put itself in front.
#[cfg(target_os = "linux")]
pub fn possible() -> bool {
    let var = |name| std::env::var(name).ok().filter(|v| !v.is_empty());
    on_x11(var("GDK_BACKEND"), var("WAYLAND_DISPLAY"), var("DISPLAY"))
}

/// Whether Windows accepts notifications right now: not during Do Not
/// Disturb, a presentation or a full-screen program.
#[cfg(windows)]
pub fn possible() -> bool {
    use windows::Win32::UI::Shell::{QUNS_ACCEPTS_NOTIFICATIONS, SHQueryUserNotificationState};
    unsafe { SHQueryUserNotificationState() }.is_ok_and(|state| state == QUNS_ACCEPTS_NOTIFICATIONS)
}

/// GTK picks Wayland if the session offers it, unless `GDK_BACKEND` names
/// another backend first; XWayland counts as X11 then.
#[cfg(any(target_os = "linux", test))]
fn on_x11(backend: Option<String>, wayland: Option<String>, display: Option<String>) -> bool {
    let first = backend
        .as_deref()
        .and_then(|list| list.split(',').next())
        .map(str::trim);
    let x11 = match first {
        Some("x11") => true,
        Some("wayland") => false,
        _ => wayland.is_none(),
    };
    x11 && display.is_some()
}

/// Opens the window for one request. `stacked` windows are already on
/// screen; the new one goes a little beside them. `on_close` runs when the
/// person closes the window, not when the app does.
pub fn open(
    app: &AppHandle,
    number: u32,
    stacked: u32,
    on_close: impl Fn() + Send + Sync + 'static,
) -> tauri::Result<()> {
    let window =
        WebviewWindowBuilder::new(app, label(number), WebviewUrl::App("alert.html".into()))
            .title("Lebenszeichen")
            .inner_size(WIDTH, HEIGHT)
            .resizable(false)
            .minimizable(false)
            .maximizable(false)
            .always_on_top(true)
            .visible_on_all_workspaces(true)
            .skip_taskbar(true)
            // Neither focused at the start nor ever after.
            .focused(false)
            .focusable(false)
            .center()
            .build()?;
    // Given to the builder, GTK applies it before the window exists, and
    // the window manager never learns of it; once more on the real window.
    let _ = window.set_visible_on_all_workspaces(true);
    if stacked > 0
        && let Ok(position) = window.outer_position()
    {
        let shift = STACK_OFFSET.saturating_mul(i32::try_from(stacked).unwrap_or(i32::MAX));
        let _ = window.set_position(PhysicalPosition::new(
            position.x.saturating_add(shift),
            position.y.saturating_add(shift),
        ));
    }
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { .. } = event {
            on_close();
        }
    });
    Ok(())
}

/// Closes a window without counting it as an answer: `destroy` sends no
/// close request.
pub fn close(app: &AppHandle, number: u32) {
    if let Some(window) = app.get_webview_window(&label(number)) {
        let _ = window.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::{label, number, on_x11};

    fn some(text: &str) -> Option<String> {
        Some(text.to_owned())
    }

    #[test]
    fn labels_round_trip() {
        assert_eq!(label(3), "alert-3");
        assert_eq!(number("alert-3"), Some(3));
        assert_eq!(number("main"), None);
        assert_eq!(number("alert-x"), None);
    }

    #[test]
    fn x11_only_where_gtk_uses_it() {
        // Plain X11 session.
        assert!(on_x11(None, None, some(":0")));
        // Wayland session: GTK picks Wayland.
        assert!(!on_x11(None, some("wayland-0"), some(":0")));
        // Forced onto XWayland.
        assert!(on_x11(some("x11"), some("wayland-0"), some(":0")));
        assert!(on_x11(some("x11,wayland"), some("wayland-0"), some(":0")));
        assert!(!on_x11(some("wayland,x11"), None, some(":0")));
        // No display at all.
        assert!(!on_x11(some("x11"), None, None));
    }
}
