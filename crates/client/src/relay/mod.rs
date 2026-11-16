//! Access to relays, as described in `docs/wire-format.md`, section
//! "Relay-Schnittstelle".
//!
//! [`Relay`] covers every relay at once: each call names the relay by its
//! base address, because contacts may live on different relays. [`Http`]
//! talks to real relays; a fake in memory serves the tests.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use lz_core::wire::{MailboxId, READER_ID_LEN, RelayUrl, Token};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

mod http;

#[cfg(test)]
pub mod fake;

pub use http::Http;

/// Identifier of a device as reader of its mailboxes.
pub type ReaderId = [u8; READER_ID_LEN];

/// What went wrong in a call to a relay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayError {
    /// The mailbox does not exist (anymore). For a mailbox of a contact this
    /// means the contact has removed this side.
    UnknownMailbox,
    /// The relay answered with an error code.
    Refused { status: u16, code: String },
    /// The relay could not be reached or answered something unreadable.
    Transport(String),
}

impl fmt::Display for RelayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownMailbox => f.write_str("das Postfach gibt es nicht mehr"),
            Self::Refused { status, code } => write!(f, "Relay lehnt ab: {code} ({status})"),
            Self::Transport(detail) => write!(f, "Relay nicht erreichbar: {detail}"),
        }
    }
}

impl std::error::Error for RelayError {}

/// A frame the relay sent on a stream connection, or its end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerFrame {
    Subscribed {
        mailbox: MailboxId,
    },
    Message {
        mailbox: MailboxId,
        seq: u64,
        data: Vec<u8>,
    },
    Error {
        mailbox: Option<MailboxId>,
        code: String,
    },
    /// The connection is gone; nothing more arrives on it.
    Closed,
}

/// A frame together with the relay it came from.
#[derive(Clone, Debug)]
pub struct Delivery {
    pub relay: RelayUrl,
    pub frame: ServerFrame,
}

/// An open stream connection to one relay.
///
/// Frames from the relay arrive on the channel given to
/// [`Relay::connect`]. Dropping the connection closes it.
pub trait Connection {
    /// Subscribes a mailbox. The relay answers with `subscribed` or `error`,
    /// then sends the waiting messages.
    fn subscribe(
        &mut self,
        mailbox: &MailboxId,
        read_token: &Token,
        reader: &ReaderId,
    ) -> Result<(), RelayError>;

    /// Acknowledges all messages of the mailbox up to and including `seq`.
    fn ack(&mut self, mailbox: &MailboxId, seq: u64) -> Result<(), RelayError>;
}

/// The calls of the relay interface.
///
/// The futures are not required to be `Send`: every user runs the client on
/// a single-threaded runtime (the desktop app on its own thread).
#[allow(async_fn_in_trait)]
pub trait Relay {
    /// Creates a voucher with which a contact may create one mailbox.
    async fn create_voucher(
        &self,
        relay: &RelayUrl,
        access_token: &Token,
    ) -> Result<Token, RelayError>;

    /// Creates a mailbox, authorised by an access token or a voucher.
    async fn create_mailbox(
        &self,
        relay: &RelayUrl,
        authorisation: &Token,
        read_token: &Token,
        send_token: &Token,
    ) -> Result<MailboxId, RelayError>;

    async fn delete_mailbox(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        read_token: &Token,
    ) -> Result<(), RelayError>;

    /// Registers this device as reader of the mailbox, or changes its push
    /// address. Without one, the relay does not wake the device.
    async fn put_reader(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        read_token: &Token,
        reader: &ReaderId,
        push_url: Option<&str>,
    ) -> Result<(), RelayError>;

    /// Delivers an envelope. Fails with [`RelayError::UnknownMailbox`] if
    /// the mailbox is gone.
    async fn deliver(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        send_token: &Token,
        envelope: &[u8],
    ) -> Result<(), RelayError>;

    /// Opens a stream connection. Frames go to `deliveries`.
    async fn connect(
        &self,
        relay: &RelayUrl,
        deliveries: mpsc::UnboundedSender<Delivery>,
    ) -> Result<Box<dyn Connection>, RelayError>;
}

/// Encodes bytes as `b64url` without padding.
pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decodes `b64url` of exactly `N` bytes.
pub fn from_b64<const N: usize>(text: &str) -> Option<[u8; N]> {
    URL_SAFE_NO_PAD.decode(text.trim()).ok()?.try_into().ok()
}

/// SHA-256 of a token as lowercase hex, as the relay expects it.
pub fn token_hash(token: &Token) -> String {
    Sha256::digest(token)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{b64, from_b64, token_hash};

    #[test]
    fn b64url_round_trip_without_padding() {
        let bytes = [0xFB; 32];
        let text = b64(&bytes);
        assert!(!text.contains('=') && !text.contains('+') && !text.contains('/'));
        assert_eq!(from_b64::<32>(&text), Some(bytes));
        assert_eq!(from_b64::<16>(&text), None);
    }

    #[test]
    fn token_hash_is_hex_sha256() {
        // SHA-256 of 32 zero bytes.
        assert_eq!(
            token_hash(&[0; 32]),
            "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925"
        );
    }
}
