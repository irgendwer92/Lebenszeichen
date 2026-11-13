//! Wire format shared by clients and the relay.
//!
//! This crate is `no_std` on purpose: the wire format must stay usable on
//! microcontrollers and must not depend on an operating system.
//!
//! Only the constants fixed by the protocol draft live here so far. The
//! envelope itself follows in milestone M2.

#![no_std]

/// Version byte at the start of every envelope.
pub const PROTOCOL_VERSION: u8 = 1;

/// Every plaintext is padded to this length before encryption, so the relay
/// cannot tell message types apart by size.
pub const PADDED_PLAINTEXT_LEN: usize = 256;

/// Length of the random nonce in front of the ciphertext.
pub const NONCE_LEN: usize = 24;

/// Length of the authentication tag added by `crypto_box`.
pub const TAG_LEN: usize = 16;

/// Length of a complete envelope: version byte, nonce, ciphertext with tag.
pub const ENVELOPE_LEN: usize = 1 + NONCE_LEN + PADDED_PLAINTEXT_LEN + TAG_LEN;

/// Largest request body the relay accepts for a single message.
pub const MAX_MESSAGE_BYTES: usize = 512;

/// Length of a mailbox identifier.
pub const MAILBOX_ID_LEN: usize = 32;

// Checked at compile time: an envelope must fit into one relay message.
const _: () = assert!(ENVELOPE_LEN <= MAX_MESSAGE_BYTES);
