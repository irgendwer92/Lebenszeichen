//! Relay server: accepts ciphertext, hands it on and deletes it.
//!
//! The relay knows neither names nor keys nor message types. Configuration
//! comes from environment variables, see `docs/wire-format.md`.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use lz_relay::limits::CLEANUP_INTERVAL_SECS;
use lz_relay::push::HttpWaker;
use lz_relay::store::Store;
use lz_relay::{AppState, Config, SystemClock};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("lz-relay: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let config = Config::from_env()?;
    let store =
        Store::open(&config.db).map_err(|e| format!("Datenbank lässt sich nicht öffnen: {e}"))?;
    let waker = HttpWaker::new(config.push_policy.clone())
        .map_err(|e| format!("HTTP-Client für Wecksignale lässt sich nicht bauen: {e}"))?;
    let state = AppState::new(
        store,
        &config.access_token,
        config.push_policy.clone(),
        Arc::new(SystemClock),
        Arc::new(waker),
    );
    state.spawn_cleanup(Duration::from_secs(CLEANUP_INTERVAL_SECS));

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|e| format!("{} lässt sich nicht öffnen: {e}", config.listen))?;
    eprintln!(
        "lz-relay {} (Protokollversion {}) lauscht auf {}",
        env!("CARGO_PKG_VERSION"),
        lz_wire::PROTOCOL_VERSION,
        config.listen
    );

    tokio::select! {
        result = lz_relay::serve(listener, state) => result.map_err(|e| format!("Serverfehler: {e}")),
        () = shutdown_signal() => {
            eprintln!("lz-relay: beendet");
            Ok(())
        }
    }
}

/// Completes on Ctrl-C or, on Unix, on `SIGTERM` (as sent by `docker stop`).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
