//! Notifications for open requests on Linux, over the D-Bus interface
//! `org.freedesktop.Notifications`.
//!
//! The notification plugin of Tauri does not tell why a notification went
//! away, and that is what counts here (see `docs/konzept.md`): the button, a
//! click on the notification and closing it by hand answer the request; when
//! it expires or the app withdraws it, nothing is sent.
//!
//! A worker compares the open requests with the notifications on screen
//! whenever something may have changed, and at least every [`TICK`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lz_client::ctx::Status;
use tauri::{AppHandle, Emitter};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::Value;

use super::{EventSink, LINE_EVENT, new_ctx, runtime};

/// Longest pause between two comparisons, so that expired requests vanish.
const TICK: Duration = Duration::from_secs(30);

/// Key of the button that sends the sign of life.
const RESPOND: &str = "respond";

/// Reason in `NotificationClosed`: closed by the user.
const CLOSED_BY_USER: u32 = 2;

/// Urgency "critical": stays until the user acts. KDE Plasma shows it even
/// during "Nicht stören"; that is intended (see `docs/konzept.md`).
const CRITICAL: u8 = 2;

/// What the notification server can do, from `GetCapabilities`.
#[derive(Clone, Copy, Debug, Default)]
struct Capabilities {
    /// Buttons, and the click on the notification itself.
    actions: bool,
    /// A text below the summary.
    body: bool,
}

impl Capabilities {
    fn from_list(list: &[String]) -> Self {
        Self {
            actions: list.iter().any(|c| c == "actions"),
            body: list.iter().any(|c| c == "body"),
        }
    }
}

/// Text and buttons of a notification.
#[derive(Debug, PartialEq, Eq)]
struct Content {
    summary: String,
    body: &'static str,
    actions: Vec<&'static str>,
}

/// Builds the notification for what the server can do: the text names
/// only the ways of answering that the server offers.
fn content(name: &str, caps: Capabilities) -> Content {
    // The summary is plain text, so the name needs no escaping there.
    let summary = format!("{name} fragt nach einem Lebenszeichen");
    let body = match (caps.body, caps.actions) {
        (false, _) => "",
        (true, true) => "Klicken oder Schließen sendet dein Lebenszeichen.",
        (true, false) => "Schließen sendet dein Lebenszeichen.",
    };
    let actions = if caps.actions {
        vec![
            "default",
            "Lebenszeichen senden",
            RESPOND,
            "Lebenszeichen senden",
        ]
    } else {
        Vec::new()
    };
    Content {
        summary,
        body,
        actions,
    }
}

/// An open request: name of the contact and its sending time.
type Key = (String, u64);

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    /// On screen under this notification id.
    Shown(u32),
    /// Gone without an answer, or being answered; not shown again.
    Quiet,
}

/// What is on screen for which request.
#[derive(Debug, Default)]
struct Screen {
    entries: HashMap<Key, Entry>,
    ids: HashMap<u32, Key>,
}

/// How a notification went away.
#[derive(Debug)]
enum Gone {
    Action(String),
    Closed(u32),
}

impl Gone {
    fn answers(&self) -> bool {
        match self {
            // "default" is the click on the notification itself.
            Gone::Action(key) => key == "default" || key == RESPOND,
            Gone::Closed(reason) => *reason == CLOSED_BY_USER,
        }
    }
}

impl Screen {
    /// Compares with the open requests. Forgets requests that are no longer
    /// open and returns the ids to withdraw and the requests to show.
    fn compare(&mut self, open: &[Key]) -> (Vec<u32>, Vec<Key>) {
        let Screen { entries, ids } = self;
        let mut withdraw = Vec::new();
        entries.retain(|key, entry| {
            let keep = open.contains(key);
            if !keep && let Entry::Shown(id) = entry {
                ids.remove(id);
                withdraw.push(*id);
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

    fn shown(&mut self, key: Key, id: u32) {
        self.ids.insert(id, key.clone());
        self.entries.insert(key, Entry::Shown(id));
    }

    /// Notes that a notification went away. Returns the request to answer,
    /// if that counts as an answer. Unknown ids, such as withdrawn ones or a
    /// second signal for the same notification, return nothing.
    fn gone(&mut self, id: u32, how: &Gone) -> Option<Key> {
        let key = self.ids.remove(&id)?;
        self.entries.insert(key.clone(), Entry::Quiet);
        how.answers().then_some(key)
    }

    /// The answer could not be sent; the request is shown again.
    fn answer_failed(&mut self, key: &Key) {
        self.entries.remove(key);
    }
}

/// Wakes the worker that keeps the notifications up to date.
pub struct Notifier {
    wake: mpsc::Sender<()>,
}

impl Notifier {
    /// Something may have changed; compare again soon.
    pub fn poke(&self) {
        let _ = self.wake.send(());
    }
}

/// Shared parts of the threads.
#[derive(Clone)]
struct Shared {
    app: AppHandle,
    path: PathBuf,
    proxy: Proxy<'static>,
    caps: Capabilities,
    screen: Arc<Mutex<Screen>>,
    wake: mpsc::Sender<()>,
}

/// Connects to the notification service of the session. Without one there
/// are no notifications; the window still shows every request.
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
        .ok()?;
    // Without an answer, assume a server that can only show and close.
    let caps = proxy
        .call::<_, _, Vec<String>>("GetCapabilities", &())
        .map(|list| Capabilities::from_list(&list))
        .unwrap_or_default();
    let (wake, woken) = mpsc::channel();
    let shared = Shared {
        app,
        path,
        proxy,
        caps,
        screen: Arc::default(),
        wake: wake.clone(),
    };
    listen_for::<(u32, u32)>(shared.clone(), "NotificationClosed", |(id, reason)| {
        (id, Gone::Closed(reason))
    });
    listen_for::<(u32, String)>(shared.clone(), "ActionInvoked", |(id, key)| {
        (id, Gone::Action(key))
    });
    std::thread::spawn(move || {
        while let Ok(()) | Err(RecvTimeoutError::Timeout) = woken.recv_timeout(TICK) {
            while woken.try_recv().is_ok() {}
            shared.update();
        }
    });
    let notifier = Notifier { wake };
    notifier.poke();
    Some(notifier)
}

/// Follows one signal of the notification service on a thread of its own.
fn listen_for<T>(shared: Shared, signal: &'static str, read: fn(T) -> (u32, Gone))
where
    T: serde::de::DeserializeOwned + zbus::zvariant::Type + 'static,
{
    std::thread::spawn(move || {
        let signals = match shared.proxy.receive_signal(signal) {
            Ok(signals) => signals,
            Err(error) => {
                eprintln!("Signal {signal} nicht verfügbar: {error}");
                return;
            }
        };
        for message in signals {
            if let Ok(body) = message.body().deserialize::<T>() {
                let (id, how) = read(body);
                shared.gone(id, &how);
            }
        }
    });
}

impl Shared {
    /// Brings the notifications in line with the open requests.
    fn update(&self) {
        let Some(open) = self.open_requests() else {
            return;
        };
        let (withdraw, show) = self.screen().compare(&open);
        for id in withdraw {
            self.close(id);
        }
        for key in show {
            match self.notify(&key.0) {
                Ok(id) => self.screen().shown(key, id),
                Err(error) => eprintln!("Benachrichtigung fehlgeschlagen: {error}"),
            }
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

    fn gone(&self, id: u32, how: &Gone) {
        let Some(key) = self.screen().gone(id, how) else {
            return;
        };
        if matches!(how, Gone::Action(_)) {
            // Some services leave the notification open after the button.
            self.close(id);
        }
        let shared = self.clone();
        std::thread::spawn(move || shared.answer(key));
    }

    /// Sends the sign of life; its text goes to the window like that of the
    /// background fetch.
    fn answer(&self, key: Key) {
        let sink = EventSink {
            app: self.app.clone(),
            pending: Vec::new(),
        };
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

    fn notify(&self, name: &str) -> zbus::Result<u32> {
        let hints: HashMap<&str, Value<'_>> = HashMap::from([
            ("urgency", Value::U8(CRITICAL)),
            ("suppress-sound", Value::Bool(true)),
        ]);
        let content = content(name, self.caps);
        self.proxy.call(
            "Notify",
            &(
                "Lebenszeichen",
                0u32,
                "",
                content.summary.as_str(),
                content.body,
                &content.actions[..],
                hints,
                0i32,
            ),
        )
    }

    fn close(&self, id: u32) {
        let _: zbus::Result<()> = self.proxy.call("CloseNotification", &(id,));
    }

    fn screen(&self) -> std::sync::MutexGuard<'_, Screen> {
        self.screen.lock().expect("lock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::{Capabilities, Entry, Gone, Key, Screen, content};

    #[test]
    fn content_follows_the_capabilities() {
        let caps = |list: &[&str]| {
            Capabilities::from_list(&list.iter().map(|c| c.to_string()).collect::<Vec<_>>())
        };

        let full = content("Bert", caps(&["actions", "body", "persistence"]));
        assert_eq!(full.summary, "Bert fragt nach einem Lebenszeichen");
        assert_eq!(
            full.body,
            "Klicken oder Schließen sendet dein Lebenszeichen."
        );
        assert_eq!(full.actions.len(), 4);

        let no_actions = content("Bert", caps(&["body"]));
        assert_eq!(no_actions.body, "Schließen sendet dein Lebenszeichen.");
        assert!(no_actions.actions.is_empty());

        // Also what a server without an answer to GetCapabilities gets.
        let bare = content("Bert", Capabilities::default());
        assert_eq!(bare.summary, "Bert fragt nach einem Lebenszeichen");
        assert_eq!(bare.body, "");
        assert!(bare.actions.is_empty());
    }

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
        screen.shown(key("Bert", 10), 7);
        assert_eq!(screen.compare(&open), (vec![], vec![]));
    }

    #[test]
    fn withdraws_requests_that_are_no_longer_open() {
        let mut screen = Screen::default();
        screen.shown(key("Bert", 10), 7);
        // Answered in the window, or a new request replaced the old one.
        let (withdraw, show) = screen.compare(&[key("Bert", 20)]);
        assert_eq!(withdraw, vec![7]);
        assert_eq!(show, vec![key("Bert", 20)]);
        // The withdrawal comes back as a signal; it answers nothing.
        assert_eq!(screen.gone(7, &Gone::Closed(3)), None);
    }

    #[test]
    fn only_the_user_answers() {
        for (how, answers) in [
            (Gone::Closed(1), false), // expired
            (Gone::Closed(2), true),  // closed by the user
            (Gone::Closed(3), false), // withdrawn by the app
            (Gone::Closed(4), false), // other
            (Gone::Action("default".into()), true),
            (Gone::Action("respond".into()), true),
            (Gone::Action("other".into()), false),
        ] {
            let mut screen = Screen::default();
            screen.shown(key("Bert", 10), 7);
            let answer = screen.gone(7, &how);
            assert_eq!(answer.is_some(), answers, "{how:?}");
            // Gone either way: not shown again while the request is open.
            assert_eq!(screen.entries[&key("Bert", 10)], Entry::Quiet);
            assert_eq!(screen.compare(&[key("Bert", 10)]), (vec![], vec![]));
        }
    }

    #[test]
    fn answers_once_when_button_and_close_both_arrive() {
        let mut screen = Screen::default();
        screen.shown(key("Bert", 10), 7);
        assert!(screen.gone(7, &Gone::Action("respond".into())).is_some());
        assert_eq!(screen.gone(7, &Gone::Closed(2)), None);
    }

    #[test]
    fn shows_again_after_a_failed_answer() {
        let mut screen = Screen::default();
        screen.shown(key("Bert", 10), 7);
        let answer = screen.gone(7, &Gone::Closed(2)).unwrap();
        screen.answer_failed(&answer);
        assert_eq!(screen.compare(&[key("Bert", 10)]).1, vec![key("Bert", 10)]);
    }
}
