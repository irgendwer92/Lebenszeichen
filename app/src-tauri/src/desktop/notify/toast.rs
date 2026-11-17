//! Notifications on Windows, as toasts.
//!
//! Scenario `reminder`: the toast stays on screen until the person acts, and
//! it needs a button, which sends the sign of life. `Activated` (click or
//! button) and `Dismissed` with `UserCanceled` answer; `TimedOut` and
//! `ApplicationHidden` (withdrawn by the app) do not.
//!
//! Windows shows toasts only for a known AppUserModelID. The Start menu
//! shortcut of the installer carries the app identifier; for a build that
//! was not installed the app registers the identifier itself under
//! `HKCU\Software\Classes\AppUserModelId`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use tauri::AppHandle;
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{
    ToastDismissalReason, ToastDismissedEventArgs, ToastFailedEventArgs, ToastNotification,
    ToastNotificationManager, ToastNotifier,
};
use windows::Win32::System::Com::CoIncrementMTAUsage;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
use windows::core::{HSTRING, IInspectable, Ref, w};

use super::{Event, Gone, Text};

type OnEvent = Arc<dyn Fn(u32, Event) + Send + Sync>;

/// Group of all toasts of requests; with the id as tag, it finds a toast in
/// the notification center.
const GROUP: &str = "requests";
type Toasts = Arc<Mutex<HashMap<u32, ToastNotification>>>;

/// The toasts of this app.
pub struct Service {
    /// The app identifier. The notifier is made anew on the thread that
    /// uses it: one made on the main thread may not be called from another
    /// (`RPC_E_WRONG_THREAD`).
    app_id: HSTRING,
    /// Toasts on screen, by id, so that the app can withdraw them; removed
    /// when they go.
    toasts: Toasts,
    next: AtomicU32,
    on_event: OnceLock<OnEvent>,
}

impl Service {
    /// The toast notifier for the app identifier, or why Windows offers
    /// none.
    pub fn connect(app: &AppHandle) -> Result<Self, String> {
        // Keeps a multithreaded apartment alive, so that the worker thread
        // may call WinRT without joining an apartment itself.
        unsafe { CoIncrementMTAUsage() }.map_err(|error| error.to_string())?;
        let id = app.config().identifier.as_str();
        register(id, &app.package_info().name);
        let app_id = HSTRING::from(id);
        // Only to learn here whether Windows offers notifications at all.
        notifier(&app_id).map_err(|error| error.to_string())?;
        Ok(Self {
            app_id,
            toasts: Arc::default(),
            next: AtomicU32::new(1),
            on_event: OnceLock::new(),
        })
    }

    /// Where events of the toasts go.
    pub fn listen(&self, on_event: impl Fn(u32, Event) + Send + Sync + 'static) {
        let _ = self.on_event.set(Arc::new(on_event));
    }

    /// Shows a toast; returns its id.
    pub fn show(&self, text: &Text) -> Result<u32, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.try_show(id, text)
            .map(|()| id)
            .map_err(|error| error.to_string())
    }

    fn try_show(&self, id: u32, text: &Text) -> windows::core::Result<()> {
        let xml = XmlDocument::new()?;
        xml.LoadXml(&HSTRING::from(toast_xml(text)))?;
        let toast = ToastNotification::CreateToastNotification(&xml)?;
        toast.SetTag(&tag(id))?;
        toast.SetGroup(&HSTRING::from(GROUP))?;
        if let Some(on_event) = self.on_event.get() {
            let report = Report {
                id,
                toasts: self.toasts.clone(),
                on_event: on_event.clone(),
            };
            let activated = report.clone();
            toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
                move |_, _| {
                    activated.send(Event::Gone(Gone::Clicked));
                    Ok(())
                },
            ))?;
            let dismissed = report.clone();
            toast.Dismissed(&TypedEventHandler::<
                ToastNotification,
                ToastDismissedEventArgs,
            >::new(
                move |_, args: Ref<ToastDismissedEventArgs>| {
                    let reason = args.ok()?.Reason()?;
                    dismissed.send(Event::Gone(dismissal(reason)));
                    Ok(())
                },
            ))?;
            toast.Failed(
                &TypedEventHandler::<ToastNotification, ToastFailedEventArgs>::new(
                    move |_, args: Ref<ToastFailedEventArgs>| {
                        let why = args
                            .ok()
                            .and_then(ToastFailedEventArgs::ErrorCode)
                            .map_or_else(|error| error, windows::core::Error::from)
                            .to_string();
                        report.send(Event::Failed(why));
                        Ok(())
                    },
                ),
            )?;
        }
        lock(&self.toasts).insert(id, toast.clone());
        notifier(&self.app_id)
            .and_then(|notifier| notifier.Show(&toast))
            .inspect_err(|_| {
                lock(&self.toasts).remove(&id);
            })
    }

    /// Withdraws a toast; Windows reports it as `ApplicationHidden`. On a
    /// thread of its own, because a click into the foreground window
    /// arrives on the main thread, which may not use toasts made elsewhere.
    ///
    /// A toast that Focus Assist held back, or that the person left in the
    /// notification center, is not on screen, and `Hide` does not reach it;
    /// it goes from the history of the notification center. Either way one
    /// of the two finds nothing, so their errors do not count.
    pub fn close(&self, id: u32) {
        let toast = lock(&self.toasts).remove(&id);
        let app_id = self.app_id.clone();
        std::thread::spawn(move || {
            if let Some(toast) = toast {
                let _ = notifier(&app_id).and_then(|notifier| notifier.Hide(&toast));
            }
            let _ = ToastNotificationManager::History().and_then(|history| {
                history.RemoveGroupedTagWithId(&tag(id), &HSTRING::from(GROUP), &app_id)
            });
        });
    }
}

/// Tag of a toast: its id.
fn tag(id: u32) -> HSTRING {
    HSTRING::from(id.to_string())
}

/// The toast notifier of the app, for the calling thread.
fn notifier(app_id: &HSTRING) -> windows::core::Result<ToastNotifier> {
    ToastNotificationManager::CreateToastNotifierWithId(app_id)
}

/// Passes on what happened to one toast.
#[derive(Clone)]
struct Report {
    id: u32,
    toasts: Toasts,
    on_event: OnEvent,
}

impl Report {
    /// Forgets the toast and reports on a thread of its own, so that the
    /// handler of Windows returns at once and never waits for a lock that
    /// `Service::show` holds.
    fn send(&self, event: Event) {
        lock(&self.toasts).remove(&self.id);
        let (id, on_event) = (self.id, self.on_event.clone());
        std::thread::spawn(move || on_event(id, event));
    }
}

/// Only closing by the person counts as an answer.
fn dismissal(reason: ToastDismissalReason) -> Gone {
    if reason == ToastDismissalReason::UserCanceled {
        Gone::Dismissed
    } else {
        Gone::Vanished
    }
}

/// A poisoned lock still holds a usable map; a panic in a handler of Windows
/// would abort the app.
fn lock(toasts: &Toasts) -> MutexGuard<'_, HashMap<u32, ToastNotification>> {
    toasts.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Registers the app identifier with its display name, so that Windows
/// shows toasts of a build that was not installed.
fn register(id: &str, name: &str) {
    let key = HSTRING::from(format!(r"Software\Classes\AppUserModelId\{id}"));
    let value: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let Ok(size) = u32::try_from(value.len() * 2) else {
        return;
    };
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &key,
            w!("DisplayName"),
            REG_SZ.0,
            Some(value.as_ptr().cast()),
            size,
        )
    };
    if let Err(error) = status.ok() {
        eprintln!("Could not register the app for notifications: {error}");
    }
}

/// The toast: silent, staying until the person acts, with one button.
fn toast_xml(text: &Text) -> String {
    format!(
        concat!(
            r#"<toast scenario="reminder" launch="respond">"#,
            r#"<visual><binding template="ToastGeneric">"#,
            "<text>{}</text><text>{}</text>",
            "</binding></visual>",
            r#"<actions><action content="{}" arguments="respond" activationType="foreground"/></actions>"#,
            r#"<audio silent="true"/>"#,
            "</toast>",
        ),
        escape(&text.title),
        escape(&text.hint),
        escape(&text.action),
    )
}

/// Escapes text for XML content and attribute values.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Text, ToastDismissalReason, dismissal, escape, toast_xml};

    #[test]
    fn only_the_person_dismisses() {
        assert!(dismissal(ToastDismissalReason::UserCanceled).answers());
        assert!(!dismissal(ToastDismissalReason::ApplicationHidden).answers());
        assert!(!dismissal(ToastDismissalReason::TimedOut).answers());
    }

    #[test]
    fn names_cannot_break_the_xml() {
        assert_eq!(escape(r#"<b>&"x'"#), "&lt;b&gt;&amp;&quot;x&apos;");
        let xml = toast_xml(&Text {
            title: "A&B</text><text>".into(),
            hint: "hint".into(),
            action: r#"go" x="1"#.into(),
        });
        assert!(xml.contains("<text>A&amp;B&lt;/text&gt;&lt;text&gt;</text>"));
        assert!(xml.contains(r#"content="go&quot; x=&quot;1""#));
        assert_eq!(xml.matches("<text>").count(), 2);
    }
}
