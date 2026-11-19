//! Fixed limits of the relay, from `docs/wire-format.md` and `docs/protocol.md`.

pub use lz_wire::{READER_ID_LEN, TOKEN_LEN};

/// How long a voucher stays valid.
pub const VOUCHER_TTL_SECS: i64 = 10 * 60;

/// A message is deleted at the latest this long after arrival.
pub const MESSAGE_TTL_SECS: i64 = 24 * 60 * 60;

/// A mailbox that was not fetched for this many days is deleted.
pub const MAILBOX_IDLE_DAYS: i64 = 90;

/// At most this many messages wait in a mailbox.
pub const MAX_WAITING_MESSAGES: usize = 32;

/// A mailbox created with a voucher may have at most this many derived
/// mailboxes at the same time.
pub const MAX_DERIVED_MAILBOXES: usize = 10;

/// A mailbox created with a voucher may derive at most this many mailboxes in
/// its lifetime; deleting a derived mailbox does not give this back.
pub const MAX_DERIVED_MAILBOXES_EVER: usize = 30;

/// A handover slot (and a reveal reservation) is deleted at the latest this
/// long after the `PUT` that created it.
pub const HANDOVER_TTL_SECS: i64 = 10 * 60;

/// Largest handover body.
pub const MAX_HANDOVER_BYTES: usize = 64 * 1024;

/// Exact length of the body of a reveal slot.
pub const REVEAL_BYTES: usize = 32;

/// At most this many handover slots on the relay, reserved reveal slots
/// included.
pub const MAX_HANDOVERS: usize = 64;

/// At most this many waiting handover slots authorized with the same token.
pub const MAX_HANDOVERS_PER_TOKEN: usize = 2;

/// At most this many waiting handover slots authorized with the read tokens of
/// one voucher family: a voucher mailbox and the mailboxes derived from it.
/// Reserved reveal slots do not count here.
pub const MAX_HANDOVERS_PER_FAMILY: usize = 2;

/// At most this many handover slots, reserved reveal slots included, for all
/// voucher families together, so that the holder of the access token always
/// has the rest of [`MAX_HANDOVERS`] for adding devices.
pub const MAX_GUEST_HANDOVERS: usize = MAX_HANDOVERS / 2;

/// At most this many messages are accepted per mailbox within
/// [`RATE_WINDOW_SECS`].
pub const MAX_DELIVERIES_PER_WINDOW: usize = 60;

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
