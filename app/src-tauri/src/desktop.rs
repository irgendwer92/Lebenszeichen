//! Desktop side of the app (M3): Tauri commands for the interface and the
//! background fetch, both on top of `lz-client`.
//!
//! The client runs on single-threaded runtimes, so every command gets its own
//! thread with its own runtime, and fetching runs on one long-lived thread.
//! They all share the state file, whose lock keeps them consistent.

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lz_client::ctx::{self, Clock, ContactView, Ctx, LastResponse, Overview, Status};
use lz_client::relay::Http;
use lz_client::show::{no_hints, reported, when};
use lz_core::store::JsonFile;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

/// How long a one-off fetch waits for further messages.
const SYNC_IDLE: Duration = Duration::from_millis(1500);

/// Pause before fetching starts again after it stopped, for example because
/// there is no state file yet.
const LISTEN_RETRY: Duration = Duration::from_secs(5);

/// Event with one line of text from the background fetch.
const LINE_EVENT: &str = "lz-line";

/// Where the state of this device lives.
pub struct Desktop {
    path: PathBuf,
}

/// Picks the state file: `LZ_STATE` if set, else `device.json` in the data
/// directory of the app. The desktop app is a device of its own and does not
/// share the state file of `lz`.
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    lz_client::install_tls_provider();
    let path = match std::env::var_os("LZ_STATE").filter(|v| !v.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => app.path().app_data_dir()?.join("device.json"),
    };
    spawn_listener(app.handle().clone(), path.clone());
    app.manage(Desktop { path });
    Ok(())
}

/// Collects the text a command writes, line by line.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl Lines {
    fn take(&self) -> Vec<String> {
        let bytes = std::mem::take(&mut *self.0.lock().expect("lock poisoned"));
        String::from_utf8_lossy(&bytes)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect()
    }
}

impl Write for Lines {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("lock poisoned").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Sends every complete line to the window as [`LINE_EVENT`].
struct EventSink {
    app: AppHandle,
    pending: Vec<u8>,
}

impl Write for EventSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line).trim_end().to_owned();
            if !line.is_empty() {
                // Without a window nobody is listening; that is fine.
                let _ = self.app.emit(LINE_EVENT, line);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn new_ctx(path: PathBuf, out: Box<dyn Write>) -> Result<Ctx<Http>, String> {
    Ok(Ctx {
        store: JsonFile::new(path),
        relay: Http::new().map_err(|error| error.to_string())?,
        clock: Clock::System,
        out,
        verbose: false,
        idle: SYNC_IDLE,
        // The window has buttons for every next step.
        hints: no_hints,
    })
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())
}

/// Fetches messages in the background for as long as the app runs.
///
/// `listen` returns only on errors with the state file, most often because
/// the device is not set up yet; then it starts again after a pause.
fn spawn_listener(app: AppHandle, path: PathBuf) {
    std::thread::spawn(move || {
        loop {
            let sink = EventSink {
                app: app.clone(),
                pending: Vec::new(),
            };
            if path.exists()
                && let Ok(runtime) = runtime()
                && let Ok(mut ctx) = new_ctx(path.clone(), Box::new(sink))
            {
                let _ = runtime.block_on(ctx.listen());
            }
            std::thread::sleep(LISTEN_RETRY);
        }
    });
}

/// Result of a command: its value and the text it wrote.
#[derive(Serialize)]
pub struct Reply<T> {
    value: T,
    lines: Vec<String>,
}

/// Runs a command of `lz-client` on a thread of its own.
async fn run<T, F>(path: PathBuf, command: F) -> Result<Reply<T>, String>
where
    T: Send + 'static,
    F: AsyncFnOnce(&mut Ctx<Http>) -> ctx::Result<T> + Send + 'static,
{
    let (done, result) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let outcome = (|| {
            let lines = Lines::default();
            let mut ctx = new_ctx(path, Box::new(lines.clone()))?;
            let value = runtime()?
                .block_on(command(&mut ctx))
                .map_err(|error| error.to_string())?;
            Ok(Reply {
                value,
                lines: lines.take(),
            })
        })();
        let _ = done.send(outcome);
    });
    result
        .await
        .unwrap_or_else(|_| Err("Interner Fehler: Befehl abgebrochen.".into()))
}

fn path(state: &tauri::State<'_, Desktop>) -> PathBuf {
    state.path.clone()
}

/// Whether the device is set up, and where its state lives.
#[derive(Serialize)]
pub struct SetupState {
    ready: bool,
    path: String,
}

#[tauri::command]
pub fn setup_state(state: tauri::State<'_, Desktop>) -> SetupState {
    SetupState {
        ready: state.path.exists(),
        path: state.path.display().to_string(),
    }
}

/// Creates the device, like `lz init`.
#[tauri::command]
pub async fn init_device(
    state: tauri::State<'_, Desktop>,
    relay: String,
    access_token: String,
) -> Result<Reply<()>, String> {
    let path = path(&state);
    off_thread(move || {
        let mut out = Lines::default();
        let token = Some(access_token.trim()).filter(|t| !t.is_empty());
        let relay = Some(relay.trim()).filter(|r| !r.is_empty());
        let store = JsonFile::new(path);
        ctx::init(&store, relay, token, &mut out).map_err(|error| error.to_string())?;
        Ok(Reply {
            value: (),
            lines: out.take(),
        })
    })
    .await
}

/// Runs a blocking function on a thread of its own.
async fn off_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, result) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = done.send(work());
    });
    result.await.expect("worker thread ended without a result")
}

/// One contact, ready for display.
#[derive(Serialize)]
pub struct Card {
    name: String,
    /// `paired`, `awaiting_confirmation`, `awaiting_hello_ok` or `ended`.
    status: &'static str,
    /// Verification code while pairing.
    code: Option<String>,
    /// Text about the open request of the contact, if any.
    incoming: Option<String>,
    /// Text about the own open request, if any.
    outgoing: Option<String>,
    can_request: bool,
    /// "wieder möglich ab …" while the button is locked.
    next_request: Option<String>,
    last_response: Option<String>,
}

#[derive(Serialize)]
pub struct InvitationCard {
    name: String,
    text: String,
}

#[derive(Serialize)]
pub struct Cards {
    contacts: Vec<Card>,
    invitations: Vec<InvitationCard>,
}

fn card(view: ContactView, now: u64) -> Card {
    let (status, code) = match view.status {
        Status::Paired => ("paired", None),
        Status::AwaitingConfirmation { code } => ("awaiting_confirmation", Some(code)),
        Status::AwaitingHelloOk { code } => ("awaiting_hello_ok", Some(code)),
        Status::Ended => ("ended", None),
    };
    let paired = status == "paired";
    let incoming = view.incoming.filter(|_| paired).map(|open| {
        let mut text = format!(
            "fragt seit {} nach einem Lebenszeichen (läuft ab {})",
            when(open.since, now),
            when(open.expires_at, now)
        );
        if let Some(reason) = open.reported {
            text.push_str(&format!("; gemeldet: {}", reported(reason)));
        }
        text
    });
    let can_request = paired && view.next_request_at <= now;
    Card {
        name: view.name,
        status,
        code,
        incoming,
        outgoing: view
            .outgoing_since
            .filter(|_| paired)
            .map(|since| format!("eigene Anfrage offen seit {}", when(since, now))),
        can_request,
        next_request: (paired && !can_request)
            .then(|| format!("wieder möglich ab {}", when(view.next_request_at, now))),
        last_response: view.last_response.map(|last| match last {
            LastResponse::Manual { at } => format!("✓ {}", when(at, now)),
            LastResponse::Reported { reason } => reported(reason),
        }),
    }
}

fn cards(overview: Overview) -> Cards {
    let now = overview.now;
    Cards {
        contacts: overview
            .contacts
            .into_iter()
            .map(|view| card(view, now))
            .collect(),
        invitations: overview
            .invitations
            .into_iter()
            .map(|invitation| InvitationCard {
                text: if invitation.expired {
                    "Einladung abgelaufen; eine rechtzeitige Annahme wird noch abgeholt".into()
                } else {
                    format!("Einladung offen bis {}", when(invitation.expires_at, now))
                },
                name: invitation.name,
            })
            .collect(),
    }
}

#[tauri::command]
pub async fn overview(state: tauri::State<'_, Desktop>) -> Result<Reply<Cards>, String> {
    run(path(&state), async |ctx| ctx.overview().map(cards)).await
}

/// Fetches waiting messages once, in addition to the background fetch.
#[tauri::command]
pub async fn sync(state: tauri::State<'_, Desktop>) -> Result<Reply<()>, String> {
    run(path(&state), async |ctx| ctx.sync().await.map(drop)).await
}

#[tauri::command]
pub async fn invite(
    state: tauri::State<'_, Desktop>,
    name: String,
    voucher: bool,
) -> Result<Reply<String>, String> {
    run(path(&state), async move |ctx| {
        ctx.invite(name.trim(), voucher).await
    })
    .await
}

#[tauri::command]
pub async fn accept(
    state: tauri::State<'_, Desktop>,
    name: String,
    code: String,
) -> Result<Reply<String>, String> {
    run(path(&state), async move |ctx| {
        ctx.accept(name.trim(), &code).await
    })
    .await
}

/// Generates a command that takes the name of a contact.
macro_rules! name_command {
    ($($(#[$doc:meta])* $command:ident => $method:ident;)*) => {$(
        $(#[$doc])*
        #[tauri::command]
        pub async fn $command(
            state: tauri::State<'_, Desktop>,
            name: String,
        ) -> Result<Reply<()>, String> {
            run(path(&state), async move |ctx| ctx.$method(&name).await).await
        }
    )*};
}

name_command! {
    /// Confirms a contact whose verification code matched.
    confirm => confirm;
    /// Rejects a contact whose verification code did not match.
    reject => reject;
    /// Asks for a sign of life.
    request => request;
    /// Answers the open request.
    respond => respond;
    /// Ends a contact or withdraws an invitation.
    remove => remove;
}

#[cfg(test)]
mod tests {
    use lz_client::ctx::{ContactView, IncomingView, LastResponse, Status};

    use super::card;

    const NOW: u64 = 1_794_600_000;
    const HOUR: u64 = 3600;

    fn paired() -> ContactView {
        ContactView {
            name: "Bert".into(),
            status: Status::Paired,
            incoming: None,
            outgoing_since: None,
            next_request_at: NOW,
            last_response: None,
        }
    }

    #[test]
    fn request_button_follows_the_rules() {
        let open = card(paired(), NOW);
        assert!(open.can_request);
        assert!(open.next_request.is_none());

        let locked = card(
            ContactView {
                next_request_at: NOW + 6 * HOUR,
                last_response: Some(LastResponse::Manual { at: NOW }),
                ..paired()
            },
            NOW,
        );
        assert!(!locked.can_request);
        assert!(
            locked
                .next_request
                .as_deref()
                .is_some_and(|t| t.starts_with("wieder möglich ab "))
        );
        assert!(locked.last_response.is_some_and(|t| t.starts_with('✓')));
    }

    #[test]
    fn incoming_request_and_reported_state() {
        let view = ContactView {
            incoming: Some(IncomingView {
                since: NOW,
                expires_at: NOW + 24 * HOUR,
                reported: Some("schläft".into()),
            }),
            ..paired()
        };
        let text = card(view, NOW).incoming.unwrap();
        assert!(text.contains("„schläft“ (gemeldet)"), "{text}");
    }

    #[test]
    fn pairing_shows_code_and_never_allows_requests() {
        let view = ContactView {
            status: Status::AwaitingConfirmation {
                code: "123456".into(),
            },
            ..paired()
        };
        let card = card(view, NOW);
        assert_eq!(card.status, "awaiting_confirmation");
        assert_eq!(card.code.as_deref(), Some("123456"));
        assert!(!card.can_request);
        assert!(card.next_request.is_none());
    }
}
