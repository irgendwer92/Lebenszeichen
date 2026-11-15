//! Shows open requests on Linux: as a foreground window under X11, as a
//! notification over the D-Bus interface `org.freedesktop.Notifications`
//! otherwise, and whenever the window cannot be opened.
//!
//! What counts as an answer is what the person did (see `docs/konzept.md`):
//! the button, a click on the notification or the window, and closing either
//! of them by hand. When a notification expires or the app withdraws one,
//! nothing is sent. The notification plugin of Tauri does not tell why a
//! notification went away, hence D-Bus directly.
//!
//! A worker compares the open requests with what is on screen whenever
//! something may have changed, and at least every [`TICK`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lz_client::ctx::Status;
use tauri::{AppHandle, Emitter};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::Value;

use super::alert;
use crate::commands::{EventSink, LINE_EVENT, new_ctx, runtime};

/// Longest pause between two comparisons, so that expired requests vanish.
const TICK: Duration = Duration::from_secs(30);

/// Key of the button that sends the sign of life.
const RESPOND: &str = "respond";

/// Reason in `NotificationClosed`: closed by the user.
const CLOSED_BY_USER: u32 = 2;

/// Urgency "critical": stays until the user acts. KDE Plasma shows it even
/// during "Nicht stören"; that is intended (see `docs/konzept.md`).
const CRITICAL: u8 = 2;

/// An open request: name of the contact and its sending time.
type Key = (String, u64);

/// What shows a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Handle {
    /// A notification, by the id the notification service gave it.
    Notification(u32),
    /// A foreground window, by the number in its label.
    Window(u32),
}

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Shown(Handle),
    /// Gone without an answer, or being answered; not shown again.
    Quiet,
}

/// What is on screen for which request.
#[derive(Debug, Default)]
struct Screen {
    entries: HashMap<Key, Entry>,
    handles: HashMap<Handle, Key>,
}

/// How a notification or a window went away.
#[derive(Debug)]
enum Gone {
    Action(String),
    Closed(u32),
    /// A click into the foreground window.
    WindowClicked,
    /// The foreground window closed by the person; the app itself closes
    /// windows without this event.
    WindowClosed,
}

impl Gone {
    fn answers(&self) -> bool {
        match self {
            // "default" is the click on the notification itself.
            Gone::Action(key) => key == "default" || key == RESPOND,
            Gone::Closed(reason) => *reason == CLOSED_BY_USER,
            Gone::WindowClicked | Gone::WindowClosed => true,
        }
    }
}

impl Screen {
    /// Compares with the open requests. Forgets requests that are no longer
    /// open and returns what to withdraw and the requests to show.
    fn compare(&mut self, open: &[Key]) -> (Vec<Handle>, Vec<Key>) {
        let Screen { entries, handles } = self;
        let mut withdraw = Vec::new();
        entries.retain(|key, entry| {
            let keep = open.contains(key);
            if !keep && let Entry::Shown(handle) = entry {
                handles.remove(handle);
                withdraw.push(*handle);
            }
            keep
        });
        let show = open
            .iter()
            .filter(|key| !entries.contains_key(*key))
            .cloned()
            .collect();
        (withdraw, show)
    }

    fn shown(&mut self, key: Key, handle: Handle) {
        self.handles.insert(handle, key.clone());
        self.entries.insert(key, Entry::Shown(handle));
    }

    /// Takes back [`Screen::shown`] when showing failed.
    fn not_shown(&mut self, handle: Handle) {
        if let Some(key) = self.handles.remove(&handle) {
            self.entries.remove(&key);
        }
    }

    /// Foreground windows on screen, to place a new one beside them.
    fn windows(&self) -> u32 {
        let count = self
            .handles
            .keys()
            .filter(|handle| matches!(handle, Handle::Window(_)))
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    /// The contact whose request a window shows.
    fn name(&self, handle: Handle) -> Option<&str> {
        self.handles.get(&handle).map(|(name, _)| name.as_str())
    }

    /// Notes that a notification or window went away. Returns the request to
    /// answer, if that counts as an answer. Unknown handles, such as
    /// withdrawn ones or a second signal for the same notification, return
    /// nothing.
    fn gone(&mut self, handle: Handle, how: &Gone) -> Option<Key> {
        let key = self.handles.remove(&handle)?;
        self.entries.insert(key.clone(), Entry::Quiet);
        how.answers().then_some(key)
    }

    /// The answer could not be sent; the request is shown again.
    fn answer_failed(&mut self, key: &Key) {
        self.entries.remove(key);
    }
}

/// Keeps what is on screen in line with the open requests.
pub struct Notifier {
    shared: Shared,
}

impl Notifier {
    /// Something may have changed; compare again soon.
    pub fn poke(&self) {
        let _ = self.shared.wake.send(());
    }

    /// Name of the contact whose request the foreground window shows.
    pub fn alert_name(&self, window: u32) -> Option<String> {
        let screen = self.shared.screen();
        screen.name(Handle::Window(window)).map(str::to_owned)
    }

    /// A click into a foreground window.
    pub fn alert_clicked(&self, window: u32) {
        self.shared
            .gone(Handle::Window(window), &Gone::WindowClicked);
    }
}

/// Shared parts of the threads.
#[derive(Clone)]
struct Shared {
    app: AppHandle,
    path: PathBuf,
    /// The notification service, if the session has one.
    proxy: Option<Proxy<'static>>,
    /// Whether requests come as foreground windows (X11).
    foreground: bool,
    next_window: Arc<AtomicU32>,
    screen: Arc<Mutex<Screen>>,
    wake: mpsc::Sender<()>,
}

/// Starts showing open requests. Without a notification service and
/// without X11 nothing is shown; the main window still lists every request.
pub fn start(app: AppHandle, path: PathBuf) -> Option<Notifier> {
    let proxy = Connection::session()
        .and_then(|connection| {
            Proxy::new(
                &connection,
                "org.freedesktop.Notifications",
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
            )
        })
        .inspect_err(|error| eprintln!("Keine Benachrichtigungen: {error}"))
        .ok();
    let foreground = alert::possible();
    if proxy.is_none() && !foreground {
        return None;
    }
    let (wake, woken) = mpsc::channel();
    let shared = Shared {
        app,
        path,
        proxy,
        foreground,
        next_window: Arc::default(),
        screen: Arc::default(),
        wake,
    };
    if shared.proxy.is_some() {
        listen_for::<(u32, u32)>(shared.clone(), "NotificationClosed", |(id, reason)| {
            (id, Gone::Closed(reason))
        });
        listen_for::<(u32, String)>(shared.clone(), "ActionInvoked", |(id, key)| {
            (id, Gone::Action(key))
        });
    }
    let worker = shared.clone();
    std::thread::spawn(move || {
        while let Ok(()) | Err(RecvTimeoutError::Timeout) = woken.recv_timeout(TICK) {
            while woken.try_recv().is_ok() {}
            worker.update();
        }
    });
    let notifier = Notifier { shared };
    notifier.poke();
    Some(notifier)
}

/// Follows one signal of the notification service on a thread of its own.
fn listen_for<T>(shared: Shared, signal: &'static str, read: fn(T) -> (u32, Gone))
where
    T: serde::de::DeserializeOwned + zbus::zvariant::Type + 'static,
{
    std::thread::spawn(move || {
        let Some(proxy) = &shared.proxy else {
            return;
        };
        let signals = match proxy.receive_signal(signal) {
            Ok(signals) => signals,
            Err(error) => {
                eprintln!("Signal {signal} nicht verfügbar: {error}");
                return;
            }
        };
        for message in signals {
            if let Ok(body) = message.body().deserialize::<T>() {
                let (id, how) = read(body);
                shared.gone(Handle::Notification(id), &how);
            }
        }
    });
}

impl Shared {
    /// Brings what is on screen in line with the open requests.
    fn update(&self) {
        let Some(open) = self.open_requests() else {
            return;
        };
        let (withdraw, show) = self.screen().compare(&open);
        for handle in withdraw {
            self.withdraw(handle);
        }
        for key in show {
            self.show(key);
        }
    }

    /// Shows a request: as a foreground window if possible, else as a
    /// notification.
    fn show(&self, key: Key) {
        if self.foreground {
            let number = self.next_window.fetch_add(1, Ordering::Relaxed);
            let handle = Handle::Window(number);
            // Noted before the window exists: its page asks for the name
            // right away.
            let stacked = {
                let mut screen = self.screen();
                let stacked = screen.windows();
                screen.shown(key.clone(), handle);
                stacked
            };
            let shared = self.clone();
            let on_close = move || shared.gone(handle, &Gone::WindowClosed);
            match alert::open(&self.app, number, stacked, on_close) {
                Ok(()) => return,
                Err(error) => {
                    eprintln!("Vordergrund-Fenster fehlgeschlagen: {error}");
                    self.screen().not_shown(handle);
                }
            }
        }
        match self.notify(&key.0) {
            Some(Ok(id)) => self.screen().shown(key, Handle::Notification(id)),
            Some(Err(error)) => eprintln!("Benachrichtigung fehlgeschlagen: {error}"),
            None => {}
        }
    }

    /// Takes a notification or window away without answering.
    fn withdraw(&self, handle: Handle) {
        match handle {
            Handle::Notification(id) => self.close(id),
            Handle::Window(number) => alert::close(&self.app, number),
        }
    }

    fn open_requests(&self) -> Option<Vec<Key>> {
        if !self.path.exists() {
            return Some(Vec::new());
        }
        let ctx = new_ctx(self.path.clone(), Box::new(std::io::sink())).ok()?;
        let overview = ctx.overview().ok()?;
        Some(
            overview
                .contacts
                .into_iter()
                .filter(|contact| contact.status == Status::Paired)
                .filter_map(|contact| Some((contact.name, contact.incoming?.since)))
                .collect(),
        )
    }

    fn gone(&self, handle: Handle, how: &Gone) {
        let Some(key) = self.screen().gone(handle, how) else {
            return;
        };
        match (handle, how) {
            // Some services leave the notification open after the button.
            (Handle::Notification(id), Gone::Action(_)) => self.close(id),
            // A clicked window goes; a closed one is going anyway.
            (Handle::Window(number), Gone::WindowClicked) => alert::close(&self.app, number),
            _ => {}
        }
        let shared = self.clone();
        std::thread::spawn(move || shared.answer(key));
    }

    /// Sends the sign of life; its text goes to the window like that of the
    /// background fetch.
    fn answer(&self, key: Key) {
        let sink = EventSink::new(self.app.clone());
        let name = key.0.clone();
        let outcome = runtime().and_then(|runtime| {
            let mut ctx = new_ctx(self.path.clone(), Box::new(sink))?;
            runtime
                .block_on(ctx.respond(&name))
                .map_err(|error| error.to_string())
        });
        if let Err(error) = outcome {
            let _ = self.app.emit(
                LINE_EVENT,
                format!("Lebenszeichen an {name} nicht gesendet: {error}"),
            );
            self.screen().answer_failed(&key);
        }
        let _ = self.wake.send(());
    }

    /// Shows a notification; `None` without a notification service.
    fn notify(&self, name: &str) -> Option<zbus::Result<u32>> {
        let proxy = self.proxy.as_ref()?;
        let hints: HashMap<&str, Value<'_>> = HashMap::from([
            ("urgency", Value::U8(CRITICAL)),
            ("suppress-sound", Value::Bool(true)),
        ]);
        // The summary is plain text, so the name needs no escaping there.
        let summary = format!("{name} fragt nach einem Lebenszeichen");
        let body = "Klicken oder Schließen sendet dein Lebenszeichen.";
        let actions = [
            "default",
            "Lebenszeichen senden",
            RESPOND,
            "Lebenszeichen senden",
        ];
        Some(proxy.call(
            "Notify",
            &(
                "Lebenszeichen",
                0u32,
                "",
                summary.as_str(),
                body,
                &actions[..],
                hints,
                0i32,
            ),
        ))
    }

    fn close(&self, id: u32) {
        if let Some(proxy) = &self.proxy {
            let _: zbus::Result<()> = proxy.call("CloseNotification", &(id,));
        }
    }

    fn screen(&self) -> std::sync::MutexGuard<'_, Screen> {
        self.screen.lock().expect("lock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::{Entry, Gone, Handle, Key, Screen};

    const NOTE: Handle = Handle::Notification(7);
    const WINDOW: Handle = Handle::Window(0);

    fn key(name: &str, since: u64) -> Key {
        (name.to_owned(), since)
    }

    #[test]
    fn shows_new_requests_once() {
        let mut screen = Screen::default();
        let open = [key("Bert", 10)];
        let (withdraw, show) = screen.compare(&open);
        assert!(withdraw.is_empty());
        assert_eq!(show, open);
        screen.shown(key("Bert", 10), NOTE);
        assert_eq!(screen.compare(&open), (vec![], vec![]));
    }

    #[test]
    fn withdraws_requests_that_are_no_longer_open() {
        for handle in [NOTE, WINDOW] {
            let mut screen = Screen::default();
            screen.shown(key("Bert", 10), handle);
            // Answered in the main window, or a new request replaced the old one.
            let (withdraw, show) = screen.compare(&[key("Bert", 20)]);
            assert_eq!(withdraw, vec![handle]);
            assert_eq!(show, vec![key("Bert", 20)]);
            // A withdrawn notification reports back with reason 3; it answers
            // nothing.
            assert_eq!(screen.gone(handle, &Gone::Closed(3)), None);
        }
    }

    #[test]
    fn only_the_user_answers() {
        for (handle, how, answers) in [
            (NOTE, Gone::Closed(1), false), // expired
            (NOTE, Gone::Closed(2), true),  // closed by the user
            (NOTE, Gone::Closed(3), false), // withdrawn by the app
            (NOTE, Gone::Closed(4), false), // other
            (NOTE, Gone::Action("default".into()), true),
            (NOTE, Gone::Action("respond".into()), true),
            (NOTE, Gone::Action("other".into()), false),
            (WINDOW, Gone::WindowClicked, true),
            (WINDOW, Gone::WindowClosed, true),
        ] {
            let mut screen = Screen::default();
            screen.shown(key("Bert", 10), handle);
            let answer = screen.gone(handle, &how);
            assert_eq!(answer.is_some(), answers, "{how:?}");
            // Gone either way: not shown again while the request is open.
            assert_eq!(screen.entries[&key("Bert", 10)], Entry::Quiet);
            assert_eq!(screen.compare(&[key("Bert", 10)]), (vec![], vec![]));
        }
    }

    #[test]
    fn answers_once_when_two_events_arrive() {
        let mut screen = Screen::default();
        screen.shown(key("Bert", 10), NOTE);
        assert!(screen.gone(NOTE, &Gone::Action("respond".into())).is_some());
        assert_eq!(screen.gone(NOTE, &Gone::Closed(2)), None);

        // A click, then the window closes.
        screen.shown(key("Carla", 10), WINDOW);
        assert!(screen.gone(WINDOW, &Gone::WindowClicked).is_some());
        assert_eq!(screen.gone(WINDOW, &Gone::WindowClosed), None);
    }

    #[test]
    fn shows_again_after_a_failed_answer() {
        let mut screen = Screen::default();
        screen.shown(key("Bert", 10), WINDOW);
        let answer = screen.gone(WINDOW, &Gone::WindowClosed).unwrap();
        screen.answer_failed(&answer);
        assert_eq!(screen.compare(&[key("Bert", 10)]).1, vec![key("Bert", 10)]);
    }

    #[test]
    fn windows_know_their_request() {
        let mut screen = Screen::default();
        screen.shown(key("Bert", 10), NOTE);
        screen.shown(key("Carla", 10), WINDOW);
        screen.shown(key("Dora", 10), Handle::Window(1));
        assert_eq!(screen.windows(), 2);
        assert_eq!(screen.name(WINDOW), Some("Carla"));
        // A window that could not be opened leaves nothing behind.
        screen.not_shown(Handle::Window(1));
        assert_eq!(screen.windows(), 1);
        assert_eq!(screen.compare(&[key("Dora", 10)]).1, vec![key("Dora", 10)]);
        // After the answer, a window no longer names a request.
        screen.gone(WINDOW, &Gone::WindowClicked);
        assert_eq!(screen.name(WINDOW), None);
    }
}
