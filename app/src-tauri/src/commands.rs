//! Tauri commands of the interface and the background fetch, on top of
//! `lz-client`, for the desktop (M3) and Android (M4) alike.
//!
//! The client runs on single-threaded runtimes, so every command gets its own
//! thread with its own runtime, and fetching runs on one long-lived thread.
//! They all share the state file, whose lock keeps them consistent, also
//! with the background path of the Android layer.

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lz_client::ctx::{self, Clock, ContactView, Ctx, LastResponse, Overview, Status};
use lz_client::devices::{Check, DeviceView, Dialog};
use lz_client::i18n::{Arg, Language, Texts};
use lz_client::qr::{self, Qr};
use lz_client::relay::{Http, b64};
use lz_client::show::{self, no_hints, reported, when};

use crate::lang::{self, Choice};

/// A text without arguments, in the language in effect.
fn text(id: &str) -> String {
    lang::texts().text(id, &[])
}
use lz_core::store::{JsonFile, Store};
use lz_core::wire::{CodeKind, SetupCode, setup_code};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, oneshot, watch};

/// How long a one-off fetch waits for further messages.
const SYNC_IDLE: Duration = Duration::from_millis(1500);

/// Pause before fetching starts again after it stopped, for example because
/// there is no state file yet.
const LISTEN_RETRY: Duration = Duration::from_secs(5);

/// Event with one line of text from the background fetch.
pub(crate) const LINE_EVENT: &str = "lz-line";

/// Event with the steps of setting up or adding a device.
pub(crate) const DEVICE_EVENT: &str = "lz-device";

/// Where the state of this device lives, and the device flow in progress.
pub struct AppState {
    path: PathBuf,
    flow: Mutex<Option<Flow>>,
}

/// A device flow in progress: how the window answers and aborts it.
struct Flow {
    answers: mpsc::UnboundedSender<bool>,
    cancel: Option<oneshot::Sender<()>>,
}

/// Picks the state file and starts fetching in the background.
///
/// On the desktop `LZ_STATE` may name the file, else it is `device.json` in
/// the data directory of the app; the app is a device of its own and does
/// not share the state file of `lz`. On Android it is `device.json` in the
/// files directory, shared with the background path.
pub fn setup(app: &mut tauri::App) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = state_path(app)?;
    lang::apply(&path);
    app.manage(AppState {
        path: path.clone(),
        flow: Mutex::new(None),
    });
    spawn_listener(app.handle().clone(), path.clone());
    #[cfg(all(desktop, unix))]
    spawn_local_interface(path.clone());
    Ok(path)
}

#[cfg(desktop)]
fn state_path(app: &tauri::App) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(
        match std::env::var_os("LZ_STATE").filter(|v| !v.is_empty()) {
            Some(path) => PathBuf::from(path),
            None => app.path().app_data_dir()?.join("device.json"),
        },
    )
}

/// The same file as the background path uses (`jni_bridge`). Also sets up
/// the certificate check, before the first connection. A device created by
/// a build before 4d lies in the data directory of Tauri; it moves once.
#[cfg(not(desktop))]
fn state_path(app: &tauri::App) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = crate::jni_bridge::tauri_setup()?;
    let earlier = app.path().app_data_dir()?.join("device.json");
    if earlier != path && earlier.exists() && !path.exists() {
        std::fs::rename(&earlier, &path)?;
    }
    Ok(path)
}

/// Requests may have come or gone; the notifications follow.
fn poke(app: &AppHandle) {
    #[cfg(desktop)]
    crate::desktop::poke(app);
    #[cfg(not(desktop))]
    let _ = app;
}

/// Which interface to show: `desktop` or `android`.
#[tauri::command]
pub fn platform() -> &'static str {
    if cfg!(target_os = "android") {
        "android"
    } else {
        "desktop"
    }
}

/// Fetches once for the tray; the lines go to the window like those of the
/// background fetch.
pub(crate) fn spawn_sync(app: AppHandle, path: PathBuf) {
    std::thread::spawn(move || {
        let sink = EventSink::new(app.clone());
        let outcome = runtime().and_then(|runtime| {
            let mut ctx = new_ctx(path, Box::new(sink))?;
            crate::answer::notify_from(&mut ctx, &app);
            runtime
                .block_on(ctx.sync())
                .map(drop)
                .map_err(|error| error.to_string())
        });
        if let Err(error) = outcome {
            let _ = app.emit(LINE_EVENT, error);
        }
    });
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
pub(crate) struct EventSink {
    app: AppHandle,
    pending: Vec<u8>,
}

impl EventSink {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self {
            app,
            pending: Vec::new(),
        }
    }
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
                poke(&self.app);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn new_ctx(path: PathBuf, out: Box<dyn Write>) -> Result<Ctx<Http>, String> {
    Ok(Ctx {
        store: JsonFile::new(path),
        relay: Http::new().map_err(|error| error.to_string())?,
        clock: Clock::System,
        out,
        verbose: false,
        idle: SYNC_IDLE,
        // The window has buttons for every next step.
        hints: no_hints,
        lang: lang::current(),
        on_notice: None,
    })
}

pub(crate) fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())
}

/// Whether the app is in the foreground. Only then the live connection is
/// open: the relay does not wake a device that is connected, and Android
/// freezes the process of an app in the background without closing its
/// connection. The desktop never leaves the foreground.
static FOREGROUND: OnceLock<watch::Sender<bool>> = OnceLock::new();

fn foreground() -> &'static watch::Sender<bool> {
    FOREGROUND.get_or_init(|| watch::Sender::new(true))
}

/// Called by the Android layer when the app goes to the background or
/// comes back (`onPause`, `onResume`).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn set_foreground(on: bool) {
    foreground().send_replace(on);
}

/// Fetches messages in the background while the app is in the foreground.
///
/// `listen` returns only on errors with the state file, most often because
/// the device is not set up yet; then it starts again after a pause. Going
/// to the background drops the connection, which closes it at the relay.
fn spawn_listener(app: AppHandle, path: PathBuf) {
    std::thread::spawn(move || {
        let Ok(runtime) = runtime() else { return };
        let mut active = foreground().subscribe();
        runtime.block_on(async move {
            loop {
                if active.wait_for(|on| *on).await.is_err() {
                    return;
                }
                let sink = EventSink::new(app.clone());
                if path.exists()
                    && let Ok(mut ctx) = new_ctx(path.clone(), Box::new(sink))
                {
                    crate::answer::notify_from(&mut ctx, &app);
                    tokio::select! {
                        _ = ctx.listen() => {}
                        _ = active.wait_for(|on| !*on) => continue,
                    }
                }
                tokio::select! {
                    () = tokio::time::sleep(LISTEN_RETRY) => {}
                    _ = active.wait_for(|on| !*on) => {}
                }
            }
        });
    });
}

/// Serves the local interface (`docs/m8-local-interface.md`) on
/// `$XDG_RUNTIME_DIR/lebenszeichen/app.sock` for as long as the app runs.
/// Only the state file changes; the background fetch sends reported
/// answers when they are due. Without a socket the app works as before.
#[cfg(all(desktop, unix))]
fn spawn_local_interface(path: PathBuf) {
    use lz_client::local::{Local, bind, default_socket_path, serve};
    std::thread::spawn(move || {
        let Some(socket) = default_socket_path("app") else {
            eprintln!("lebenszeichen: no XDG_RUNTIME_DIR, no local interface");
            return;
        };
        let listener = match bind(&socket) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!(
                    "lebenszeichen: local interface {}: {error}",
                    socket.display()
                );
                return;
            }
        };
        let Ok(runtime) = runtime() else { return };
        let local = Local {
            store: JsonFile::new(path),
            clock: Clock::System,
            token: None,
        };
        if let Err(error) = runtime.block_on(serve(listener, local)) {
            eprintln!("lebenszeichen: local interface: {error}");
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
        .unwrap_or_else(|_| Err(text("app-error-command-aborted")))
}

fn path(state: &tauri::State<'_, AppState>) -> PathBuf {
    state.path.clone()
}

/// The texts of the window in the language in effect, and the setting.
#[derive(Serialize)]
pub struct UiTexts {
    /// The language in effect: `en` or `de`.
    lang: &'static str,
    /// The setting of this device: `system`, `en` or `de`.
    language: &'static str,
    /// Raw texts of all `ui-` ids; variables are written `{ $name }`.
    texts: std::collections::BTreeMap<String, String>,
}

fn ui_texts(path: &std::path::Path) -> UiTexts {
    let language = lang::current();
    UiTexts {
        lang: language.tag(),
        language: lang::load(&lang::settings_path(path)).tag(),
        texts: language.raw_texts("ui-"),
    }
}

/// The texts of the window.
#[tauri::command]
pub fn texts(state: tauri::State<'_, AppState>) -> UiTexts {
    ui_texts(&state.path)
}

/// Saves the language setting of this device (`system`, `en` or `de`),
/// makes it the language in effect and returns the new texts.
#[tauri::command]
pub fn set_language(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    language: String,
) -> Result<UiTexts, String> {
    let choice =
        Choice::from_tag(&language).ok_or_else(|| format!("unknown language: {language}"))?;
    lang::save(&lang::settings_path(&state.path), choice).map_err(|error| error.to_string())?;
    lang::apply(&state.path);
    #[cfg(desktop)]
    crate::desktop::language_changed(&app);
    #[cfg(not(desktop))]
    let _ = app;
    Ok(ui_texts(&state.path))
}

/// Whether the app starts at login (desktop only).
#[cfg(desktop)]
#[tauri::command]
pub fn autostart(app: AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch()
        .is_enabled()
        .map_err(|error| format!("{}: {error}", text("app-error-autostart")))
}

/// Turns "Start at login" on or off (desktop only). Counts as the offer
/// having been made, so that the app never turns it on again by itself.
#[cfg(desktop)]
#[tauri::command]
pub fn set_autostart(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    on: bool,
) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    let failed =
        |error: &dyn std::fmt::Display| format!("{}: {error}", text("app-error-autostart"));
    let launcher = app.autolaunch();
    if on {
        launcher.enable()
    } else {
        launcher.disable()
    }
    .map_err(|error| failed(&error))?;
    crate::settings::update(&crate::settings::path(&state.path), |settings| {
        settings.autostart_offered = true;
    })
    .map_err(|error| failed(&error))?;
    launcher.is_enabled().map_err(|error| failed(&error))
}

/// Whether the device is set up, and where its state lives.
#[derive(Serialize)]
pub struct SetupState {
    ready: bool,
    path: String,
    /// Relay address to prefill, from the build.
    default_relay: Option<&'static str>,
}

/// The relay address of the build variable `LZ_DEFAULT_RELAY`, if set.
fn default_relay_of(value: Option<&'static str>) -> Option<&'static str> {
    value.map(str::trim).filter(|relay| !relay.is_empty())
}

#[tauri::command]
pub fn setup_state(state: tauri::State<'_, AppState>) -> SetupState {
    SetupState {
        ready: state.path.exists(),
        path: state.path.display().to_string(),
        default_relay: default_relay_of(option_env!("LZ_DEFAULT_RELAY")),
    }
}

/// Creates a device without relay and access token, which can accept an
/// invitation with a voucher right away.
#[tauri::command]
pub async fn init_with_invitation_only(
    state: tauri::State<'_, AppState>,
) -> Result<Reply<()>, String> {
    let path = path(&state);
    off_thread(move || {
        let mut out = Lines::default();
        ctx::init(&JsonFile::new(path), None, None, lang::current(), &mut out)
            .map_err(|error| error.to_string())?;
        Ok(Reply {
            value: (),
            lines: out.take(),
        })
    })
    .await
}

/// Relay address and access token from a setup code.
fn read_setup_code(input: &str) -> Result<(String, String), String> {
    match CodeKind::of(input) {
        Some(CodeKind::Setup) => {}
        Some(CodeKind::Invitation | CodeKind::OldInvitation) => {
            return Err(text("app-error-code-is-invitation"));
        }
        Some(CodeKind::Device) => return Err(text("app-error-code-is-device")),
        None => return Err(text("app-error-code-not-setup")),
    }
    let code = SetupCode::from_text(input).map_err(|_| text("app-error-setup-code-invalid"))?;
    Ok((code.relay.to_string(), b64(&code.access_token)))
}

/// Creates the device with the relay account of a setup code.
#[tauri::command]
pub async fn init_from_setup_code(
    state: tauri::State<'_, AppState>,
    code: String,
) -> Result<Reply<()>, String> {
    let (relay, token) = read_setup_code(&code)?;
    let path = path(&state);
    off_thread(move || {
        let mut out = Lines::default();
        ctx::init(
            &JsonFile::new(path),
            Some(&relay),
            Some(&token),
            lang::current(),
            &mut out,
        )
        .map_err(|error| error.to_string())?;
        Ok(Reply {
            value: (),
            lines: out.take(),
        })
    })
    .await
}

/// Which kind of code a text is: `invitation`, `old_invitation` (version 1,
/// refused), `device` or `setup`.
fn kind_name(text: &str) -> Option<&'static str> {
    CodeKind::of(text).map(|kind| match kind {
        CodeKind::Invitation => "invitation",
        CodeKind::OldInvitation => "old_invitation",
        CodeKind::Device => "device",
        CodeKind::Setup => "setup",
    })
}

#[tauri::command]
pub fn code_kind(text: String) -> Option<&'static str> {
    kind_name(&text)
}

/// The setup code of this device's relay account, as text and QR code.
#[derive(Debug, Serialize)]
pub struct SetupCodeView {
    text: String,
    qr: Qr,
}

fn own_setup_code(store: &impl Store) -> Result<SetupCodeView, String> {
    let device = store.load().map_err(|error| error.to_string())?;
    let account = device
        .relay
        .and_then(|relay| relay.access_token.map(|token| (relay.url, token)));
    let Some((relay, access_token)) = account else {
        return Err(text("app-error-no-access-token"));
    };
    let code = SetupCode {
        access_token,
        relay,
    };
    let text = code.to_text(&mut [0; setup_code::MAX_TEXT_LEN]).to_owned();
    let qr = qr::modules(&text);
    Ok(SetupCodeView { text, qr })
}

#[tauri::command]
pub async fn setup_code(state: tauri::State<'_, AppState>) -> Result<Reply<SetupCodeView>, String> {
    let path = path(&state);
    off_thread(move || {
        Ok(Reply {
            value: own_setup_code(&JsonFile::new(path))?,
            lines: Vec::new(),
        })
    })
    .await
}

/// Creates the device, like `lz init`.
#[tauri::command]
pub async fn init_device(
    state: tauri::State<'_, AppState>,
    relay: String,
    access_token: String,
) -> Result<Reply<()>, String> {
    let path = path(&state);
    off_thread(move || {
        let mut out = Lines::default();
        let token = Some(access_token.trim()).filter(|t| !t.is_empty());
        let relay = Some(relay.trim()).filter(|r| !r.is_empty());
        let store = JsonFile::new(path);
        ctx::init(&store, relay, token, lang::current(), &mut out)
            .map_err(|error| error.to_string())?;
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
    /// `paired`, `awaiting_reveal`, `awaiting_confirmation`,
    /// `awaiting_hello_ok` or `ended`.
    status: &'static str,
    /// Verification code while pairing, once there is one.
    code: Option<String>,
    /// To show next to the code: over which channel to compare it.
    compare_hint: Option<String>,
    /// Text about the open request of the contact, if any.
    incoming: Option<String>,
    /// Text about the own open request, if any.
    outgoing: Option<String>,
    can_request: bool,
    /// "wieder möglich ab …" while the button is locked.
    next_request: Option<String>,
    last_response: Option<String>,
    /// Settings, only for paired contacts.
    rules: Option<RulesCard>,
}

/// Rules of a contact in the units of the window.
#[derive(Debug, PartialEq, Serialize)]
pub struct RulesCard {
    /// How often the contact may ask: set by the own side.
    interval_hours: f64,
    expiry_hours: f64,
    weekly_limit: u16,
    /// How often the own side may ask the contact, as text.
    peer: String,
    /// Whether this device answers the contact with the reported state.
    reports_allowed: bool,
    /// After how many minutes without a manual answer.
    report_delay_minutes: u32,
}

fn rules_card(view: &ContactView, t: Texts) -> RulesCard {
    let own = &view.own_rules;
    RulesCard {
        interval_hours: f64::from(own.min_interval_minutes) / 60.0,
        expiry_hours: f64::from(own.expiry_minutes) / 60.0,
        weekly_limit: own.weekly_limit,
        peer: show::rules(t, &view.peer_rules),
        reports_allowed: view.reports_allowed,
        report_delay_minutes: view.report_delay_minutes,
    }
}

/// Longest minimum interval and expiry the window offers: one week.
const MAX_HOURS: f64 = 7.0 * 24.0;

/// Highest weekly limit the window offers.
const MAX_WEEKLY: u16 = 999;

/// The value of the window that [`minutes`] converts.
#[derive(Clone, Copy)]
enum Hours {
    Interval,
    Expiry,
}

/// Turns hours from the window into minutes for the protocol. The window
/// offers half hours; `lz rules` can set any minute.
fn minutes(hours: f64, least: f64, what: Hours, t: Texts) -> Result<u32, String> {
    let (range, halves_text) = match what {
        Hours::Interval => ("app-error-interval-range", "app-error-interval-halves"),
        Hours::Expiry => ("app-error-expiry-range", "app-error-expiry-halves"),
    };
    if !(least..=MAX_HOURS).contains(&hours) {
        return Err(t.text(
            range,
            &[
                ("min", Arg::Text(&show_hours(least, t))),
                ("max", Arg::Text(&show_hours(MAX_HOURS, t))),
            ],
        ));
    }
    let halves = hours * 2.0;
    if (halves - halves.round()).abs() > 1e-9 {
        return Err(t.text(halves_text, &[]));
    }
    // At most 336 halves, so the cast is exact.
    Ok(halves.round() as u32 * 30)
}

/// Hours with the decimal separator of the language.
fn show_hours(hours: f64, t: Texts) -> String {
    let text = format!("{hours}");
    if t.language() == Language::De {
        text.replace('.', ",")
    } else {
        text
    }
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
    /// The reported state of this device as text, while it applies.
    state: Option<String>,
}

fn card(view: ContactView, now: u64, t: Texts) -> Card {
    let rules = (view.status == Status::Paired).then(|| rules_card(&view, t));
    let (status, code) = match view.status {
        Status::Paired => ("paired", None),
        Status::AwaitingReveal => ("awaiting_reveal", None),
        Status::AwaitingConfirmation { code } => ("awaiting_confirmation", Some(code)),
        Status::AwaitingHelloOk { code } => ("awaiting_hello_ok", Some(code)),
        Status::Ended => ("ended", None),
    };
    let paired = status == "paired";
    let incoming = view.incoming.filter(|_| paired).map(|open| {
        let since = when(t, open.since, now);
        let expires = when(t, open.expires_at, now);
        let mut args = vec![
            ("name", Arg::Text(&view.name)),
            ("since", Arg::Text(&since)),
            ("expires", Arg::Text(&expires)),
        ];
        let reason = open.reported.map(|reason| reported(t, reason));
        if let Some(reason) = &reason {
            args.push(("reported", Arg::Text(reason)));
            t.text("app-contact-incoming-reported", &args)
        } else {
            t.text("app-contact-incoming", &args)
        }
    });
    let can_request = paired && view.next_request_at <= now;
    Card {
        name: view.name,
        status,
        compare_hint: code.as_ref().map(|_| show::compare_hint(t)),
        code,
        incoming,
        outgoing: view.outgoing_since.filter(|_| paired).map(|since| {
            t.text(
                "app-contact-outgoing",
                &[("since", Arg::Text(&when(t, since, now)))],
            )
        }),
        can_request,
        next_request: (paired && !can_request).then(|| {
            t.text(
                "app-contact-next-request",
                &[("at", Arg::Text(&when(t, view.next_request_at, now)))],
            )
        }),
        last_response: view.last_response.map(|last| match last {
            LastResponse::Manual { at } => format!("✓ {}", when(t, at, now)),
            LastResponse::Reported { reason } => reported(t, reason),
        }),
        rules,
    }
}

fn cards(overview: Overview, t: Texts) -> Cards {
    let now = overview.now;
    Cards {
        state: overview.state.map(|state| {
            t.text(
                "app-reported-state",
                &[
                    ("reason", Arg::Text(&show::reported_state(t, &state.reason))),
                    ("until", Arg::Text(&when(t, state.until, now))),
                ],
            )
        }),
        contacts: overview
            .contacts
            .into_iter()
            .map(|view| card(view, now, t))
            .collect(),
        invitations: overview
            .invitations
            .into_iter()
            .map(|invitation| InvitationCard {
                text: if invitation.expired {
                    t.text("app-invitation-expired", &[])
                } else {
                    t.text(
                        "app-invitation-open",
                        &[("until", Arg::Text(&when(t, invitation.expires_at, now)))],
                    )
                },
                name: invitation.name,
            })
            .collect(),
    }
}

#[tauri::command]
pub async fn overview(state: tauri::State<'_, AppState>) -> Result<Reply<Cards>, String> {
    run(path(&state), async |ctx| {
        ctx.overview()
            .map(|overview| cards(overview, lang::texts()))
    })
    .await
}

/// Fetches waiting messages once, in addition to the background fetch.
#[tauri::command]
pub async fn sync(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<Reply<()>, String> {
    run(path(&state), async move |ctx| {
        crate::answer::notify_from(ctx, &app);
        ctx.sync().await.map(drop)
    })
    .await
}

/// A new invitation: the code as text and as QR code.
#[derive(Serialize)]
pub struct Invitation {
    code: String,
    qr: Qr,
}

#[tauri::command]
pub async fn invite(
    state: tauri::State<'_, AppState>,
    name: String,
    voucher: bool,
) -> Result<Reply<Invitation>, String> {
    run(path(&state), async move |ctx| {
        let code = ctx.invite(name.trim(), voucher).await?;
        let qr = qr::modules(&code);
        Ok(Invitation { code, qr })
    })
    .await
}

#[tauri::command]
pub async fn accept(
    state: tauri::State<'_, AppState>,
    name: String,
    code: String,
) -> Result<Reply<()>, String> {
    run(path(&state), async move |ctx| {
        ctx.accept(name.trim(), &code).await
    })
    .await
}

/// Changes how often a contact may ask. Only the values that changed are
/// given; the others stay as they are.
#[tauri::command]
pub async fn set_rules(
    state: tauri::State<'_, AppState>,
    name: String,
    interval_hours: Option<f64>,
    expiry_hours: Option<f64>,
    weekly_limit: Option<u16>,
) -> Result<Reply<()>, String> {
    let t = lang::texts();
    let interval = interval_hours
        .map(|h| minutes(h, 0.0, Hours::Interval, t))
        .transpose()?;
    let expiry = expiry_hours
        .map(|h| minutes(h, 0.5, Hours::Expiry, t))
        .transpose()?;
    if weekly_limit.is_some_and(|n| n > MAX_WEEKLY) {
        return Err(t.text("app-error-weekly-max", &[("max", Arg::from(MAX_WEEKLY))]));
    }
    run(path(&state), async move |ctx| {
        ctx.rules(&name, interval, expiry, weekly_limit).await
    })
    .await
}

/// Whether this device answers a contact with the reported state, and
/// after how many minutes.
#[tauri::command]
pub async fn set_report_setting(
    state: tauri::State<'_, AppState>,
    name: String,
    allowed: bool,
    delay_minutes: u32,
) -> Result<Reply<()>, String> {
    run(path(&state), async move |ctx| {
        ctx.set_report_setting(&name, allowed, Some(delay_minutes))
            .map(drop)
    })
    .await
}

/// Ends the reported state of this device.
#[tauri::command]
pub async fn clear_state(state: tauri::State<'_, AppState>) -> Result<Reply<()>, String> {
    run(path(&state), async |ctx| ctx.clear_state()).await
}

/// Generates a command that takes the name of a contact.
macro_rules! name_command {
    ($($(#[$doc:meta])* $command:ident => $method:ident;)*) => {$(
        $(#[$doc])*
        #[tauri::command]
        pub async fn $command(
            app: AppHandle,
            state: tauri::State<'_, AppState>,
            name: String,
        ) -> Result<Reply<()>, String> {
            let reply = run(path(&state), async move |ctx| ctx.$method(&name).await).await;
            poke(&app);
            reply
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

#[tauri::command]
pub async fn devices(state: tauri::State<'_, AppState>) -> Result<Reply<Vec<DeviceView>>, String> {
    run(path(&state), async |ctx| ctx.devices()).await
}

/// Renames this device; the other devices of the person learn the new
/// name.
#[tauri::command]
pub async fn device_rename(
    state: tauri::State<'_, AppState>,
    name: String,
) -> Result<Reply<()>, String> {
    run(path(&state), async move |ctx| {
        ctx.rename_device(&name).await
    })
    .await
}

/// Removes another device of this person.
#[tauri::command]
pub async fn device_remove(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    reader: String,
) -> Result<Reply<()>, String> {
    let reply = run(path(&state), async move |ctx| {
        ctx.remove_device_by_reader(&reader).await
    })
    .await;
    poke(&app);
    reply
}

/// One message of [`DEVICE_EVENT`]. `step` is `device_code`,
/// `check_device`, `check_handover` (codes to show), `confirm_device`,
/// `confirm_handover` (codes to compare, answered with `device_confirm`),
/// `progress`, `done`, `error` or `alarm`.
#[derive(Clone, Serialize)]
struct DeviceStep<'a> {
    step: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a str>,
    /// With `device_code`: the code as QR code, like `invite` gives it.
    #[serde(skip_serializing_if = "Option::is_none")]
    qr: Option<Qr>,
}

fn emit_step(app: &AppHandle, step: &str, code: Option<&str>, text: Option<&str>) {
    let qr = (step == "device_code")
        .then_some(code)
        .flatten()
        .map(qr::modules);
    let _ = app.emit(
        DEVICE_EVENT,
        DeviceStep {
            step,
            code,
            text,
            qr,
        },
    );
}

/// The answers of the window and the events of a device flow.
struct DeviceDialog {
    app: AppHandle,
    answers: mpsc::UnboundedReceiver<bool>,
}

impl Dialog for DeviceDialog {
    fn device_code(&mut self, code: &str) {
        emit_step(&self.app, "device_code", Some(code), None);
    }

    fn check_code(&mut self, check: Check, code: &str) {
        let step = match check {
            Check::Device => "check_device",
            Check::Handover => "check_handover",
        };
        emit_step(&self.app, step, Some(code), None);
    }

    async fn confirm(&mut self, check: Check, code: &str) -> bool {
        let step = match check {
            Check::Device => "confirm_device",
            Check::Handover => "confirm_handover",
        };
        // Answers given before the question do not count.
        while self.answers.try_recv().is_ok() {}
        emit_step(&self.app, step, Some(code), None);
        // A closed channel means the flow was cancelled.
        self.answers.recv().await.unwrap_or(false)
    }
}

/// Sends the progress lines of a device flow, without the QR code for
/// terminals and the bare code, which the window shows itself.
struct ProgressSink {
    app: AppHandle,
    pending: Vec<u8>,
}

fn is_progress(line: &str) -> bool {
    !line.is_empty()
        && !line.starts_with(lz_core::wire::device_code::PREFIX)
        && !line.chars().any(|c| matches!(c, '█' | '▀' | '▄'))
}

impl Write for ProgressSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line).trim_end().to_owned();
            if is_progress(&line) {
                emit_step(&self.app, "progress", None, Some(&line));
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

enum FlowKind {
    Join { relay: String, name: String },
    Add { code: String, name: String },
}

/// Starts a device flow on a thread of its own; only one at a time.
fn start_flow(app: AppHandle, state: &AppState, kind: FlowKind) -> Result<(), String> {
    let mut slot = state.flow.lock().expect("lock poisoned");
    if slot.is_some() {
        return Err(text("app-error-flow-running"));
    }
    let (answers, answered) = mpsc::unbounded_channel();
    let (cancel, cancelled) = oneshot::channel();
    *slot = Some(Flow {
        answers,
        cancel: Some(cancel),
    });
    drop(slot);
    let path = state.path.clone();
    std::thread::spawn(move || {
        let outcome = runtime().and_then(|runtime| {
            let sink = ProgressSink {
                app: app.clone(),
                pending: Vec::new(),
            };
            let mut ctx = new_ctx(path, Box::new(sink))?;
            let mut dialog = DeviceDialog {
                app: app.clone(),
                answers: answered,
            };
            runtime.block_on(async {
                let work = async {
                    match &kind {
                        FlowKind::Join { relay, name } => {
                            ctx.join_device(relay, name, None, &mut dialog).await
                        }
                        FlowKind::Add { code, name } => {
                            ctx.add_device(code, name, &mut dialog).await
                        }
                    }
                    .map_err(|error| error.to_string())
                };
                tokio::select! {
                    result = work => result.map(Some),
                    _ = cancelled => Ok(None),
                }
            })
        });
        if let Some(state) = app.try_state::<AppState>() {
            *state.flow.lock().expect("lock poisoned") = None;
        }
        match outcome {
            Ok(Some(())) => {
                emit_step(&app, "done", None, None);
                poke(&app);
            }
            Ok(None) => {}
            Err(error) if error.starts_with(&alarm_prefix()) => {
                emit_step(&app, "alarm", None, Some(&error));
            }
            Err(error) => emit_step(&app, "error", None, Some(&error)),
        }
    });
    Ok(())
}

/// How the texts of `lz-client` begin that report someone else interfering
/// ("Warning:", "Achtung:").
fn alarm_prefix() -> String {
    let prefix = lang::texts().text("error-alarm", &[("text", Arg::Text(""))]);
    prefix.trim_end().to_owned()
}

/// Sets this device up as a further device of a person; progress comes as
/// [`DEVICE_EVENT`].
#[tauri::command]
pub fn device_join(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    relay: String,
    name: String,
) -> Result<(), String> {
    start_flow(app, &state, FlowKind::Join { relay, name })
}

/// Adds a new device to this person with its device code; progress comes
/// as [`DEVICE_EVENT`].
#[tauri::command]
pub fn device_add(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    code: String,
    name: String,
) -> Result<(), String> {
    start_flow(app, &state, FlowKind::Add { code, name })
}

/// The answer to the open question of the device flow.
#[tauri::command]
pub fn device_confirm(state: tauri::State<'_, AppState>, answer: bool) {
    if let Some(flow) = state.flow.lock().expect("lock poisoned").as_ref() {
        let _ = flow.answers.send(answer);
    }
}

/// Aborts the device flow in progress.
#[tauri::command]
pub fn device_cancel(state: tauri::State<'_, AppState>) {
    if let Some(flow) = state.flow.lock().expect("lock poisoned").as_mut()
        && let Some(cancel) = flow.cancel.take()
    {
        let _ = cancel.send(());
    }
}

#[cfg(test)]
mod tests {
    use lz_client::ctx::{ContactView, IncomingView, LastResponse, Status};

    use lz_core::wire::Rules;

    use lz_client::ctx;
    use lz_client::show;
    use lz_core::store::JsonFile;

    use lz_client::i18n::Language;

    use super::{
        Hours, card, default_relay_of, is_progress, kind_name, minutes, own_setup_code,
        read_setup_code, rules_card,
    };

    fn de() -> lz_client::i18n::Texts {
        Language::De.texts()
    }

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
            own_rules: Rules::DEFAULT,
            peer_rules: Rules::DEFAULT,
            reports_allowed: false,
            report_delay_minutes: 15,
        }
    }

    #[test]
    fn rules_in_hours() {
        let view = ContactView {
            peer_rules: Rules {
                min_interval_minutes: 90,
                expiry_minutes: 60,
                weekly_limit: 2,
            },
            reports_allowed: true,
            report_delay_minutes: 30,
            ..paired()
        };
        let card = rules_card(&view, de());
        assert!(card.reports_allowed);
        assert_eq!(card.report_delay_minutes, 30);
        assert_eq!(card.interval_hours, 6.0);
        assert_eq!(card.expiry_hours, 24.0);
        assert_eq!(card.weekly_limit, 7);
        assert!(card.peer.contains("höchstens 2 pro Woche"), "{}", card.peer);
    }

    #[test]
    fn hours_become_minutes() {
        let m = |hours, least| minutes(hours, least, Hours::Interval, de());
        assert_eq!(m(0.0, 0.0), Ok(0));
        assert_eq!(m(1.5, 0.0), Ok(90));
        assert_eq!(m(168.0, 0.5), Ok(10_080));
        let error = minutes(0.0, 0.5, Hours::Expiry, de()).unwrap_err();
        assert_eq!(
            error,
            "Die Ablaufzeit muss zwischen 0,5 und 168 Stunden liegen."
        );
        assert!(m(1.25, 0.0).unwrap_err().contains("halben Stunden"));
        assert!(m(169.0, 0.0).is_err());
        assert!(m(f64::NAN, 0.0).is_err());
        let english = minutes(0.0, 0.5, Hours::Interval, Language::En.texts()).unwrap_err();
        assert_eq!(
            english,
            "The minimum interval must be between 0.5 and 168 hours."
        );
    }

    #[test]
    fn settings_only_when_paired() {
        assert!(card(paired(), NOW, de()).rules.is_some());
        let ended = ContactView {
            status: Status::Ended,
            ..paired()
        };
        assert!(card(ended, NOW, de()).rules.is_none());
    }

    #[test]
    fn request_button_follows_the_rules() {
        let open = card(paired(), NOW, de());
        assert!(open.can_request);
        assert!(open.next_request.is_none());

        let locked = card(
            ContactView {
                next_request_at: NOW + 6 * HOUR,
                last_response: Some(LastResponse::Manual { at: NOW }),
                ..paired()
            },
            NOW,
            de(),
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
        let text = card(view, NOW, de()).incoming.unwrap();
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
        let card = card(view, NOW, de());
        assert_eq!(card.status, "awaiting_confirmation");
        assert_eq!(card.code.as_deref(), Some("123456"));
        assert_eq!(card.compare_hint, Some(show::compare_hint(de())));
        assert!(!card.can_request);
        assert!(card.next_request.is_none());

        // While waiting for `reveal` there is no code yet.
        let view = ContactView {
            status: Status::AwaitingReveal,
            ..paired()
        };
        let waiting = super::card(view, NOW, de());
        assert_eq!(waiting.status, "awaiting_reveal");
        assert_eq!(waiting.code, None);
        assert_eq!(waiting.compare_hint, None);
        assert!(!waiting.can_request);
        assert!(waiting.rules.is_none());
    }

    #[test]
    fn code_kinds() {
        assert_eq!(kind_name("LZ2-ABC"), Some("invitation"));
        assert_eq!(kind_name("lz1-abc"), Some("old_invitation"));
        assert_eq!(kind_name("  lzd1-abc"), Some("device"));
        assert_eq!(kind_name("LZS1-ABC"), Some("setup"));
        assert_eq!(kind_name("hallo"), None);
        assert_eq!(kind_name(""), None);
    }

    #[test]
    fn default_relay_ignores_empty_values() {
        assert_eq!(default_relay_of(None), None);
        assert_eq!(default_relay_of(Some("")), None);
        assert_eq!(default_relay_of(Some("  ")), None);
        assert_eq!(
            default_relay_of(Some(" https://relay.example.org ")),
            Some("https://relay.example.org")
        );
    }

    #[test]
    fn setup_code_errors_are_german() {
        crate::lang::set_current(Language::De);
        assert!(
            read_setup_code("LZ1-ABC")
                .unwrap_err()
                .contains("Einladung")
        );
        assert!(
            read_setup_code("LZD1-ABC")
                .unwrap_err()
                .contains("Gerätecode")
        );
        assert!(
            read_setup_code("blabla")
                .unwrap_err()
                .contains("kein Einrichtungscode")
        );
        assert!(
            read_setup_code("LZS1-AAAA")
                .unwrap_err()
                .contains("ungültig")
        );
    }

    #[test]
    fn setup_code_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let first = JsonFile::new(dir.path().join("a.json"));
        let token = lz_client::relay::b64(&[7; 32]);
        ctx::init(
            &first,
            Some("https://relay.example.org"),
            Some(&token),
            Language::De,
            &mut Vec::new(),
        )
        .unwrap();
        let view = own_setup_code(&first).unwrap();
        assert!(view.text.starts_with("LZS1-"));

        let (relay, read) = read_setup_code(&view.text).unwrap();
        assert_eq!(relay, "https://relay.example.org");
        assert_eq!(read, token);

        // Without access token there is nothing to show.
        let second = JsonFile::new(dir.path().join("b.json"));
        ctx::init(&second, None, None, Language::De, &mut Vec::new()).unwrap();
        assert!(
            own_setup_code(&second)
                .unwrap_err()
                .contains("Zugangstoken")
        );
    }

    #[test]
    fn progress_hides_codes_and_qr() {
        assert!(is_progress("Das neue Gerät ist eingerichtet."));
        assert!(!is_progress(""));
        assert!(!is_progress("LZD1-ABCDEF"));
        assert!(!is_progress("█▀▀▀█ ▄▄"));
    }
}
