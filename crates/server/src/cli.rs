//! The command line of `lz-server` and the start of its parts.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{ArgAction, Parser};
use lz_client::ctx::{Clock, relay_url};
use lz_client::i18n::Language;
use lz_client::local::{self, Local, MAX_CONNECTIONS, TokenSource};
use lz_client::relay::Http;
use lz_core::store::JsonFile;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::join::Join;
use crate::server::{Shared, runtime, spawn_listener};

/// Lebenszeichen as a service: listens, sends due reported answers, and
/// serves the local interface and the setup page.
#[derive(Parser)]
#[command(name = "lz-server", version)]
pub struct Args {
    /// State file of the device
    #[arg(
        long,
        env = "LZ_STATE",
        default_value = "/data/device.json",
        value_name = "PATH"
    )]
    pub state: PathBuf,

    /// Socket of the local interface, always with the token
    #[arg(long, value_name = "PATH")]
    pub socket: PathBuf,

    /// The same interface over TCP, with the token (plain HTTP)
    #[arg(long, value_name = "ADDR")]
    pub http: Option<SocketAddr>,

    /// Address of the setup page; needs --page-allow
    #[arg(long, value_name = "ADDR", requires = "page_allow")]
    pub page: Option<SocketAddr>,

    /// Peer allowed to use the page; all others are refused
    #[arg(
        long = "page-allow",
        value_name = "IP",
        requires = "page",
        action = ArgAction::Append,
        num_args = 1..
    )]
    pub page_allow: Vec<IpAddr>,

    /// Relay offered on the page for joining
    #[arg(long, value_name = "URL")]
    pub relay: Option<String>,
}

pub fn main() -> ExitCode {
    lz_client::install_tls_provider();
    let args = Args::parse();
    match run(args, Language::from_env()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lz-server: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Builds the shared state from the arguments.
pub fn shared(args: &Args, lang: Language) -> Result<Arc<Shared>, String> {
    let t = lang.texts();
    let default_relay = match &args.relay {
        Some(relay) => Some(relay_url(t, relay).map_err(|e| e.0)?.to_string()),
        None => None,
    };
    Ok(Arc::new(Shared {
        state: args.state.clone(),
        lang,
        relay: Http::new().map_err(|error| error.text(t))?,
        default_relay,
        socket: args.socket.clone(),
        http: args.http,
        wake: watch::Sender::new(0),
        join: Mutex::new(Join::default()),
    }))
}

/// The local interface with the token from the state file, read per
/// request, so that a renewed token applies at once.
pub fn local(shared: &Shared) -> Local {
    Local {
        store: JsonFile::new(&shared.state),
        clock: Clock::System,
        token: Some(TokenSource::Stored),
    }
}

fn run(args: Args, lang: Language) -> Result<(), String> {
    let shared = shared(&args, lang)?;
    if shared.state.exists() {
        shared.ensure_token().map_err(|e| e.0)?;
    }
    let socket =
        local::bind(&args.socket).map_err(|error| format!("{}: {error}", args.socket.display()))?;
    spawn_listener(Arc::clone(&shared));

    let runtime = runtime().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let http = match args.http {
            Some(address) => {
                let listener = TcpListener::bind(address)
                    .await
                    .map_err(|error| format!("{address}: {error}"))?;
                eprintln!(
                    "{}",
                    shared.text_with("server-http-warning", "address", &address.to_string())
                );
                Some(listener)
            }
            None => None,
        };
        let page = match args.page {
            Some(address) => Some(
                TcpListener::bind(address)
                    .await
                    .map_err(|error| format!("{address}: {error}"))?,
            ),
            None => None,
        };
        let result = tokio::select! {
            result = local::serve(socket, local(&shared)) => result,
            result = optional(http.map(|listener| serve_tcp(listener, local(&shared)))) => result,
            result = optional(page.map(|listener| {
                crate::page::serve(listener, Arc::clone(&shared), args.page_allow.clone())
            })) => result,
            () = shutdown() => Ok(()),
        };
        result.map_err(|error| error.to_string())
    })
}

/// Runs the future if there is one, else waits forever.
async fn optional(future: Option<impl Future<Output = io::Result<()>>>) -> io::Result<()> {
    match future {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}

/// Ends with SIGTERM (`docker stop`) or SIGINT; as process 1 in a
/// container nothing else would end the process.
async fn shutdown() {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut int)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}

/// Serves the local interface on TCP, with the same connection limit as
/// the socket.
pub async fn serve_tcp(listener: TcpListener, local: Local) -> io::Result<()> {
    let local = Arc::new(local);
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            continue;
        };
        let local = Arc::clone(&local);
        tokio::spawn(async move {
            local::serve_connection(stream, local).await;
            drop(permit);
        });
    }
}
