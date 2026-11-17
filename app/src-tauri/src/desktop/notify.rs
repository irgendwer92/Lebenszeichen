//! Shows open requests on the desktop. On Linux as a foreground window where
//! the app may put one in front (X11), as a notification otherwise and
//! whenever the window cannot be opened. On Windows always as a
//! notification, so that the request also waits in the notification center,
//! and in addition as a foreground window unless Focus Assist or Do Not
//! Disturb is on. When one of the two answers, the app withdraws the other.
//!
//! What counts as an answer is what the person did (see `docs/concept.md`):
//! the button, a click on the notification or the window, and closing either
//! of them by hand. When a notification expires or the app withdraws one,
//! nothing is sent. The notification plugin of Tauri does not tell why a
//! notification went away, hence the notification service of each system
//! directly: D-Bus on Linux (`dbus.rs`), toasts on Windows (`toast.rs`).
//!
//! A worker compares the open requests with what is on screen whenever
//! something may have changed, and at least every [`TICK`].

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lz_client::ctx::Status;
use lz_client::i18n::Arg;
use tauri::{AppHandle, Emitter};

use super::alert;
use crate::answer::Answer;
use crate::commands::{EventSink, LINE_EVENT, new_ctx, runtime};

#[cfg(target_os = "linux")]
mod dbus;
#[cfg(target_os = "linux")]
use dbus as service;
#[cfg(windows)]
mod toast;
#[cfg(windows)]
use toast as service;

/// Longest pause between two comparisons, so that expired requests vanish.
const TICK: Duration = Duration::from_secs(30);

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

/// Whether a request comes as notification and window at once (Windows),
/// or as one of the two (Linux).
const BOTH: bool = cfg!(windows);

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    /// On screen, as one or two handles.
    Shown(Vec<Handle>),
    /// Gone without an answer, or being answered; not shown again.
    Quiet,
}

/// What is on screen for which request.
#[derive(Debug, Default)]
struct Screen {
    entries: HashMap<Key, Entry>,
    handles: HashMap<Handle, Key>,
}

/// How a notification or a window went away. Each notification service
/// translates its own signals into these.
#[derive(Debug)]
enum Gone {
    /// A click on the notification or on its button.
    Clicked,
    /// The notification closed by the person.
    Dismissed,
    /// Expired, withdrawn by the app, or anything else; answers nothing.
    Vanished,
    /// A click into the foreground window.
    WindowClicked,
    /// The foreground window closed by the person; the app itself closes
    /// windows without this event.
    WindowClosed,
}

impl Gone {
    fn answers(&self) -> bool {
        !matches!(self, Gone::Vanished)
    }
}

/// What a notification service reports about one of its notifications.
#[derive(Debug)]
enum Event {
    Gone(Gone),
    /// The notification could not be shown after all, and why (Windows
    /// only).
    #[cfg_attr(not(windows), allow(dead_code))]
    Failed(String),
}

/// Text of a notification.
struct Text {
    title: String,
    hint: String,
    /// What the notification is for.
    kind: Kind,
}

/// A request, which stays and answers when the person acts on it, or an
/// answer to an own request, which is an ordinary notification with sound.
enum Kind {
    /// With the label of the button that sends the sign of life.
    Request { action: String },
    /// With the label of a click, which shows the main window.
    Answer { open: String },
}

impl Text {
    fn for_request(name: &str) -> Self {
        let t = crate::lang::texts();
        Self {
            title: t.text("app-alert-title", &[("name", Arg::Text(name))]),
            hint: t.text("app-alert-hint", &[]),
            kind: Kind::Request {
                action: t.text("app-alert-action", &[]),
            },
        }
    }

    fn for_answer(answer: &Answer) -> Self {
        Self {
            title: answer.title.clone(),
            hint: answer.text.clone(),
            kind: Kind::Answer {
                open: crate::lang::texts().text("app-answer-open", &[]),
            },
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
            if !keep && let Entry::Shown(shown) = entry {
                for handle in shown.drain(..) {
                    handles.remove(&handle);
                    withdraw.push(handle);
                }
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
        match self.entries.get_mut(&key) {
            Some(Entry::Shown(shown)) => shown.push(handle),
            _ => {
                self.entries.insert(key, Entry::Shown(vec![handle]));
            }
        }
    }

    /// Takes back [`Screen::shown`] when showing failed. Without anything
    /// else on screen for the request, the next comparison shows it again.
    fn not_shown(&mut self, handle: Handle) {
        let Some(key) = self.handles.remove(&handle) else {
            return;
        };
        if let Some(Entry::Shown(shown)) = self.entries.get_mut(&key) {
            shown.retain(|other| *other != handle);
            if shown.is_empty() {
                self.entries.remove(&key);
            }
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

    /// Notes that a notification or window went away. If that counts as an
    /// answer, returns the request to answer and what else still shows it,
    /// to be withdrawn. Otherwise the rest stays on screen; once nothing is
    /// left, the request is not shown again. Unknown handles, such as
    /// withdrawn ones or a second signal for the same notification, return
    /// nothing.
    fn gone(&mut self, handle: Handle, how: &Gone) -> Option<(Key, Vec<Handle>)> {
        let key = self.handles.remove(&handle)?;
        let mut rest = match self.entries.remove(&key) {
            Some(Entry::Shown(shown)) => shown,
            _ => Vec::new(),
        };
        rest.retain(|other| *other != handle);
        if how.answers() {
            for other in &rest {
                self.handles.remove(other);
            }
            self.entries.insert(key.clone(), Entry::Quiet);
            Some((key, rest))
        } else {
            let entry = if rest.is_empty() {
                Entry::Quiet
            } else {
                Entry::Shown(rest)
            };
            self.entries.insert(key, entry);
            None
        }
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

    /// Notifies of an answer to an own request. Without a notification
    /// service only the main window shows it, as a line.
    pub fn answer(&self, answer: &Answer) {
        let Some(service) = &self.shared.service else {
            return;
        };
        // Noted under the lock, so that a click cannot come before it is
        // known.
        let mut answers = self.shared.answers();
        match service.show(&Text::for_answer(answer)) {
            Ok(id) => {
                answers.insert(id);
            }
            Err(error) => {
                drop(answers);
                self.shared.report("app-error-notification", &error);
            }
        }
    }
}

/// Shared parts of the threads.
#[derive(Clone)]
struct Shared {
    app: AppHandle,
    path: PathBuf,
    /// The notification service, if the system has one.
    service: Option<Arc<service::Service>>,
    /// Why there is none, reported when a request would need it.
    no_service: Option<String>,
    next_window: Arc<AtomicU32>,
    screen: Arc<Mutex<Screen>>,
    /// Notifications of answers on screen. They answer nothing; a click
    /// shows the main window.
    answers: Arc<Mutex<HashSet<u32>>>,
    wake: mpsc::Sender<()>,
}

/// Starts showing open requests. Without a notification service and
/// without foreground windows nothing is shown; the main window still lists
/// every request.
pub fn start(app: AppHandle, path: PathBuf) -> Option<Notifier> {
    let (service, no_service) = match service::Service::connect(&app) {
        Ok(service) => (Some(Arc::new(service)), None),
        Err(error) => {
            eprintln!("No notifications: {error}");
            (None, Some(error))
        }
    };
    if service.is_none() && !alert::possible() {
        return None;
    }
    let (wake, woken) = mpsc::channel();
    let shared = Shared {
        app,
        path,
        service,
        no_service,
        next_window: Arc::default(),
        screen: Arc::default(),
        answers: Arc::default(),
        wake,
    };
    if let Some(service) = &shared.service {
        let events = shared.clone();
        service.listen(move |id, event| {
            let handle = Handle::Notification(id);
            if events.answer_gone(id, &event) {
                return;
            }
            match event {
                Event::Gone(how) => events.gone(handle, &how),
                Event::Failed(why) => {
                    events.report("app-error-notification", &why);
                    events.screen().not_shown(handle);
                }
            }
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

    /// Shows a request: on Windows as a notification and, if possible, as a
    /// foreground window; on Linux as a foreground window if possible, else
    /// as a notification. Asked for every request, because Focus Assist on
    /// Windows changes while the app runs.
    fn show(&self, key: Key) {
        let window = alert::possible() && self.open_window(&key);
        if BOTH || !window {
            self.notify(key);
        }
    }

    /// Opens a foreground window for a request; whether that worked.
    fn open_window(&self, key: &Key) -> bool {
        let number = self.next_window.fetch_add(1, Ordering::Relaxed);
        let handle = Handle::Window(number);
        // Noted before the window exists: its page asks for the name right
        // away.
        let stacked = {
            let mut screen = self.screen();
            let stacked = screen.windows();
            screen.shown(key.clone(), handle);
            stacked
        };
        let shared = self.clone();
        let on_close = move || shared.gone(handle, &Gone::WindowClosed);
        match alert::open(&self.app, number, stacked, on_close) {
            Ok(()) => true,
            Err(error) => {
                eprintln!("Foreground window failed: {error}");
                self.screen().not_shown(handle);
                false
            }
        }
    }

    /// Shows a request as a notification, if the system has a service.
    fn notify(&self, key: Key) {
        let Some(service) = &self.service else {
            if let Some(why) = &self.no_service {
                self.report("app-error-no-notifications", why);
            }
            return;
        };
        // Noted under the lock, so that an event for the new notification
        // cannot come before it is known.
        let mut screen = self.screen();
        match service.show(&Text::for_request(&key.0)) {
            Ok(id) => screen.shown(key, Handle::Notification(id)),
            Err(error) => {
                drop(screen);
                self.report("app-error-notification", &error);
            }
        }
    }

    /// Shows in the main window why a request could not be shown, so that
    /// it does not vanish without a trace.
    fn report(&self, id: &str, error: &str) {
        let line = crate::lang::texts().text(id, &[("error", Arg::Text(error))]);
        eprintln!("{line}");
        let _ = self.app.emit(LINE_EVENT, line);
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
        let Some((key, rest)) = self.screen().gone(handle, how) else {
            return;
        };
        match (handle, how) {
            // Some services leave the notification open after the button.
            (Handle::Notification(id), Gone::Clicked) => self.close(id),
            // A clicked window goes; a closed one is going anyway.
            (Handle::Window(number), Gone::WindowClicked) => alert::close(&self.app, number),
            _ => {}
        }
        // The other display of the same request goes without answering.
        for other in rest {
            self.withdraw(other);
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
                crate::lang::texts().text(
                    "app-error-answer-failed",
                    &[("name", Arg::Text(&name)), ("error", Arg::Text(&error))],
                ),
            );
            self.screen().answer_failed(&key);
        }
        let _ = self.wake.send(());
    }

    fn close(&self, id: u32) {
        if let Some(service) = &self.service {
            service.close(id);
        }
    }

    /// Handles an event of an answer notification; whether it was one.
    fn answer_gone(&self, id: u32, event: &Event) -> bool {
        if !self.answers().remove(&id) {
            return false;
        }
        if let Event::Gone(Gone::Clicked) = event {
            // Some services leave the notification open after a click.
            self.close(id);
            // Temporary for the test on Windows: shows that the click arrived.
            let _ = self
                .app
                .emit(LINE_EVENT, format!("Answer notification {id} clicked"));
            // From the main thread, like the click on the tray icon.
            let app = self.app.clone();
            let _ = self
                .app
                .run_on_main_thread(move || crate::desktop::show_window(&app));
        }
        true
    }

    fn answers(&self) -> std::sync::MutexGuard<'_, HashSet<u32>> {
        self.answers.lock().expect("lock poisoned")
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
            // A withdrawn notification reports back; it answers nothing.
            assert_eq!(screen.gone(handle, &Gone::Vanished), None);
        }
    }

    #[test]
    fn only_the_user_answers() {
        for (handle, how, answers) in [
            (NOTE, Gone::Clicked, true),
            (NOTE, Gone::Dismissed, true),
            (NOTE, Gone::Vanished, false),
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
        assert!(screen.gone(NOTE, &Gone::Clicked).is_some());
        assert_eq!(screen.gone(NOTE, &Gone::Dismissed), None);

        // A click, then the window closes.
        screen.shown(key("Carla", 10), WINDOW);
        assert!(screen.gone(WINDOW, &Gone::WindowClicked).is_some());
        assert_eq!(screen.gone(WINDOW, &Gone::WindowClosed), None);
    }

    #[test]
    fn shows_again_after_a_failed_answer() {
        let mut screen = Screen::default();
        screen.shown(key("Bert", 10), WINDOW);
        let (answer, _) = screen.gone(WINDOW, &Gone::WindowClosed).unwrap();
        screen.answer_failed(&answer);
        assert_eq!(screen.compare(&[key("Bert", 10)]).1, vec![key("Bert", 10)]);
    }

    #[test]
    fn an_answer_withdraws_the_other_display() {
        for (answering, other) in [(NOTE, WINDOW), (WINDOW, NOTE)] {
            let mut screen = Screen::default();
            screen.shown(key("Bert", 10), WINDOW);
            screen.shown(key("Bert", 10), NOTE);
            let how = match answering {
                Handle::Window(_) => Gone::WindowClicked,
                Handle::Notification(_) => Gone::Clicked,
            };
            assert_eq!(
                screen.gone(answering, &how),
                Some((key("Bert", 10), vec![other]))
            );
            // Its withdrawal reports back and answers nothing.
            assert_eq!(screen.gone(other, &Gone::Vanished), None);
            assert_eq!(screen.entries[&key("Bert", 10)], Entry::Quiet);
            assert_eq!(screen.windows(), 0);
        }
    }

    #[test]
    fn the_other_display_stays_when_one_goes_without_answer() {
        let mut screen = Screen::default();
        screen.shown(key("Bert", 10), WINDOW);
        screen.shown(key("Bert", 10), NOTE);
        // The notification expires; the window still asks.
        assert_eq!(screen.gone(NOTE, &Gone::Vanished), None);
        assert_eq!(screen.entries[&key("Bert", 10)], Entry::Shown(vec![WINDOW]));
        assert_eq!(screen.compare(&[key("Bert", 10)]), (vec![], vec![]));
        assert_eq!(
            screen.gone(WINDOW, &Gone::WindowClosed),
            Some((key("Bert", 10), vec![]))
        );
    }

    #[test]
    fn withdraws_both_displays_of_a_closed_request() {
        let mut screen = Screen::default();
        screen.shown(key("Bert", 10), WINDOW);
        screen.shown(key("Bert", 10), NOTE);
        let (withdraw, _) = screen.compare(&[]);
        assert_eq!(withdraw, vec![WINDOW, NOTE]);
        assert!(screen.handles.is_empty());
    }

    #[test]
    fn a_failed_notification_leaves_the_window() {
        let mut screen = Screen::default();
        screen.shown(key("Bert", 10), WINDOW);
        screen.shown(key("Bert", 10), NOTE);
        screen.not_shown(NOTE);
        assert_eq!(screen.entries[&key("Bert", 10)], Entry::Shown(vec![WINDOW]));
    }

    #[test]
    fn shows_again_after_a_failed_notification() {
        let mut screen = Screen::default();
        screen.shown(key("Bert", 10), NOTE);
        screen.not_shown(NOTE);
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
