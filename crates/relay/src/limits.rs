//! Fixed limits of the relay, from `docs/wire-format.md` and `docs/protocol.md`.

pub use lz_wire::{READER_ID_LEN, TOKEN_LEN};

/// How long a voucher stays valid.
pub const VOUCHER_TTL_SECS: i64 = 10 * 60;

/// A message is deleted at the latest this long after arrival.
pub const MESSAGE_TTL_SECS: i64 = 24 * 60 * 60;

/// A mailbox that was not fetched for this many days is deleted.
pub const MAILBOX_IDLE_DAYS: i64 = 90;

/// At most this many messages wait in a mailbox.
pub const MAX_WAITING_MESSAGES: usize = 8;

/// At most this many messages are accepted per mailbox within
/// [`RATE_WINDOW_SECS`].
pub const MAX_DELIVERIES_PER_WINDOW: usize = 20;

/// Window for [`MAX_DELIVERIES_PER_WINDOW`].
pub const RATE_WINDOW_SECS: i64 = 60 * 60;

/// At most this many readers per mailbox; each one may cause a wake signal.
pub const MAX_READERS_PER_MAILBOX: usize = 16;

/// Longest accepted push URL in bytes.
pub const MAX_PUSH_URL_LEN: usize = 1024;

/// Largest JSON request body.
pub const MAX_JSON_BODY: usize = 4 * 1024;

/// Largest WebSocket frame from a client.
pub const MAX_FRAME_LEN: usize = 4 * 1024;

/// At most this many mailboxes per WebSocket connection.
pub const MAX_SUBSCRIPTIONS_PER_CONNECTION: usize = 256;

/// Time limit for one wake signal, including connection setup.
pub const WAKE_TIMEOUT_SECS: u64 = 5;

/// Interval between two cleanup runs.
pub const CLEANUP_INTERVAL_SECS: u64 = 5 * 60;
