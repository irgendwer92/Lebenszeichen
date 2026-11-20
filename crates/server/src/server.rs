//! What the parts of `lz-server` share: the configuration, contexts on the
//! state file, the token, and the background listener.

use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lz_client::ctx::{Clock, Ctx, Result};
use lz_client::i18n::{Arg, Language, Texts};
use lz_client::relay::Http;
use lz_client::show::no_hints;
use lz_core::store::JsonFile;
use tokio::sync::watch;

use crate::join::Join;

/// How long a fetch waits for further messages; `listen` hardly uses it.
const SYNC_IDLE: Duration = Duration::from_millis(1500);

/// Pause before listening starts again after it stopped, or before the
/// next look for a state file that the page may be creating.
pub const LISTEN_RETRY: Duration = Duration::from_secs(5);

/// The configuration and the state of the process, shared by the page, the
/// device flow and the listener.
pub struct Shared {
    /// The state file.
    pub state: PathBuf,
    pub lang: Language,
    /// Shared by all contexts; cheap to clone.
    pub relay: Http,
    /// Relay offered on the page for joining.
    pub default_relay: Option<String>,
    /// Socket of the local interface, shown on the page.
    pub socket: PathBuf,
    /// Address of the TCP port of the local interface, if it is on.
    pub http: Option<SocketAddr>,
    /// Counts up whenever listening should start again at once, for
    /// example after joining.
    pub wake: watch::Sender<u64>,
    /// Adding this device, if begun.
    pub join: Mutex<Join>,
}

impl Shared {
    pub fn texts(&self) -> Texts {
        self.lang.texts()
    }

    /// A text without arguments.
    pub fn text(&self, id: &str) -> String {
        self.texts().text(id, &[])
    }

    /// A text with one argument.
    pub fn text_with(&self, id: &str, name: &str, value: &str) -> String {
        self.texts().text(id, &[(name, Arg::Text(value))])
    }

    /// A context on the state file that writes to `out`.
    pub fn ctx(&self, out: Box<dyn Write>) -> Ctx<Http> {
        Ctx {
            store: JsonFile::new(&self.state),
            relay: self.relay.clone(),
            clock: Clock::System,
            out,
            verbose: false,
            idle: SYNC_IDLE,
            // Commands are not typed here; the page has buttons.
            hints: no_hints,
            lang: self.lang,
            on_notice: None,
        }
    }

    /// A context whose output goes nowhere.
    pub fn quiet_ctx(&self) -> Ctx<Http> {
        self.ctx(Box::new(io::sink()))
    }

    /// Creates the token of the local interface if the state file has
    /// none, and says where to find it. The token itself stays out of the
    /// log, which `docker logs` and support requests pass on (security
    /// review of M9).
    pub fn ensure_token(&self) -> Result<()> {
        let mut ctx = self.quiet_ctx();
        if ctx.report_token()?.is_none() {
            ctx.enable_report_token()?;
            let path = self.state.display().to_string();
            println!("{}", self.text_with("server-token-created", "path", &path));
        }
        Ok(())
    }

    /// Lets the listener start again at once.
    pub fn wake_listener(&self) {
        self.wake.send_modify(|n| *n = n.wrapping_add(1));
    }
}

/// A single-threaded runtime, like the client expects: its futures are not
/// `Send`.
pub fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

/// Listens on a thread of its own while a state file exists, like
/// `lz listen`: notices go to standard output. Without a state file it
/// looks again every few seconds, or at once when woken (after joining).
/// After an error it starts again after a pause.
pub fn spawn_listener(shared: Arc<Shared>) {
    let mut woken = shared.wake.subscribe();
    std::thread::spawn(move || {
        let runtime = match runtime() {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("lz-server: {error}");
                std::process::exit(1);
            }
        };
        runtime.block_on(async move {
            loop {
                woken.mark_unchanged();
                if shared.state.exists() {
                    let mut ctx = shared.ctx(Box::new(io::stdout()));
                    if let Err(error) = ctx.listen().await {
                        eprintln!(
                            "{}",
                            shared.text_with("server-listen-stopped", "error", &error.0)
                        );
                    }
                }
                tokio::select! {
                    () = tokio::time::sleep(LISTEN_RETRY) => {}
                    _ = woken.changed() => {}
                }
            }
        });
    });
}
