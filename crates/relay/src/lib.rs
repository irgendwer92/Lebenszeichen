//! Relay server: accepts ciphertext, hands it on and deletes it.
//!
//! The relay knows neither names nor keys nor message types. The interface is
//! specified in `docs/drahtformat.md`, section "Relay-Schnittstelle".
//!
//! [`app`] builds the router from an [`AppState`]; `main.rs` only reads the
//! configuration and starts the server, so tests can run the relay in-process.

mod api;
mod config;
mod error;
pub mod hub;
pub mod ids;
pub mod limits;
pub mod push;
mod rate;
pub mod store;
mod stream;

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::routing::{get, post, put};

pub use config::Config;
pub use error::ApiError;

use hub::{Event, Hub};
use ids::{Hash, Token};
use push::{PushPolicy, Waker};
use rate::RateLimiter;
use store::Store;

/// Source of the current time in Unix seconds. Tests use [`ManualClock`].
pub trait Clock: Send + Sync + 'static {
    /// Current time in Unix seconds.
    fn now(&self) -> i64;
}

/// The system clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}

/// A clock that only moves when told to.
#[derive(Debug)]
pub struct ManualClock(AtomicI64);

impl ManualClock {
    /// A clock standing at `now`.
    pub fn new(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }

    /// Moves the clock forward by `secs`.
    pub fn advance(&self, secs: i64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Shared state of the relay.
#[derive(Clone)]
pub struct AppState(Arc<Inner>);

struct Inner {
    store: Store,
    hub: Hub,
    limiter: Mutex<RateLimiter>,
    clock: Arc<dyn Clock>,
    waker: Arc<dyn Waker>,
    push_policy: PushPolicy,
    access_token_hash: Hash,
}

impl AppState {
    /// Assembles the state. Only the hash of `access_token` is kept.
    pub fn new(
        store: Store,
        access_token: &Token,
        push_policy: PushPolicy,
        clock: Arc<dyn Clock>,
        waker: Arc<dyn Waker>,
    ) -> Self {
        Self(Arc::new(Inner {
            store,
            hub: Hub::default(),
            limiter: Mutex::new(RateLimiter::default()),
            clock,
            waker,
            push_policy,
            access_token_hash: ids::sha256(access_token),
        }))
    }

    /// The underlying store, for tests and maintenance. Its methods block.
    pub fn store(&self) -> &Store {
        &self.0.store
    }

    fn now(&self) -> i64 {
        self.0.clock.now()
    }

    fn limiter(&self) -> std::sync::MutexGuard<'_, RateLimiter> {
        self.0.limiter.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs a blocking store operation on the blocking thread pool.
    async fn db<T, F>(&self, f: F) -> Result<T, ApiError>
    where
        T: Send + 'static,
        F: FnOnce(&Store) -> rusqlite::Result<T> + Send + 'static,
    {
        let state = self.clone();
        match tokio::task::spawn_blocking(move || f(&state.0.store)).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(e)) => {
                // SQLite errors carry no identifiers worth hiding, but keep it short.
                eprintln!("lz-relay: Datenbankfehler: {e}");
                Err(ApiError::Internal)
            }
            Err(_) => Err(ApiError::Internal),
        }
    }

    /// Deletes expired messages and vouchers and idle mailboxes, and tells
    /// subscribed connections about deleted mailboxes.
    pub async fn cleanup(&self) -> Result<store::Cleanup, ApiError> {
        let now = self.now();
        self.limiter().prune(now);
        let removed = self.db(move |s| s.cleanup(now)).await?;
        for mailbox in &removed.mailboxes {
            self.0.hub.notify(mailbox, Event::MailboxDeleted);
        }
        Ok(removed)
    }

    /// Runs [`AppState::cleanup`] now and then every `interval`.
    pub fn spawn_cleanup(&self, interval: Duration) -> tokio::task::JoinHandle<()> {
        let state = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                let _ = state.cleanup().await;
            }
        })
    }
}

/// Builds the HTTP and WebSocket routes of the relay.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(api::health))
        .route("/v1/vouchers", post(api::create_voucher))
        .route("/v1/mailboxes", post(api::create_mailbox))
        .route(
            "/v1/mailboxes/{mailbox}",
            axum::routing::delete(api::delete_mailbox),
        )
        .route(
            "/v1/mailboxes/{mailbox}/readers/{reader}",
            put(api::put_reader).delete(api::delete_reader),
        )
        .route("/v1/mailboxes/{mailbox}/messages", post(api::post_message))
        .route("/v1/stream", get(stream::stream))
        .with_state(state)
}

/// Serves the relay on `listener`.
pub async fn serve(listener: tokio::net::TcpListener, state: AppState) -> std::io::Result<()> {
    axum::serve(listener, app(state)).await
}
