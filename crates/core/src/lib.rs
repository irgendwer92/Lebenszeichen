//! Client core: protocol, encryption, request rules and storage.
//!
//! The core has no dependency on Tauri or any user interface. The desktop
//! app, the Android layer and the command-line client all call into it.
//!
//! The protocol part takes time and randomness as arguments and has no
//! dependency on storage or the operating system, so it can later move to
//! devices without one.

pub use lz_wire as wire;

pub mod contact;
pub mod crypto;
pub mod group;
pub mod handover;
mod hex;
pub mod pairing;
pub mod rules;
pub mod store;

/// Everything that can go wrong in the protocol part of the core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The bytes do not follow the wire format.
    Wire(wire::Error),
    /// Encryption failed. Does not happen with valid keys.
    Encrypt,
    /// Wrong keys, or the envelope was altered on the way.
    Decrypt,
    /// The message or invitation lies outside its time window.
    Expired,
    /// A valid message or call that does not fit the current state.
    Unexpected,
    /// The contact has ended.
    Ended,
    /// The rules of the other side allow the next request only at this time.
    NotYet(u64),
    /// A revealed value, challenge or handover does not match when adding a
    /// device. Show an alarm and stop.
    Mismatch,
    /// The handover is not readable.
    Unreadable,
    /// The handover has a version this device does not know; the older
    /// device has to be updated first.
    UnknownVersion(u32),
}

impl From<wire::Error> for Error {
    fn from(error: wire::Error) -> Self {
        Self::Wire(error)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wire(error) => write!(f, "wire format: {error}"),
            Self::Encrypt => f.write_str("encryption failed"),
            Self::Decrypt => f.write_str("decryption failed"),
            Self::Expired => f.write_str("outside the time window"),
            Self::Unexpected => f.write_str("does not fit the current state"),
            Self::Ended => f.write_str("contact has ended"),
            Self::NotYet(at) => write!(f, "next request possible at {at}"),
            Self::Mismatch => f.write_str("does not match what was agreed"),
            Self::Unreadable => f.write_str("not readable"),
            Self::UnknownVersion(v) => write!(f, "unknown version {v}"),
        }
    }
}

impl std::error::Error for Error {}

/// Local defaults that are not part of the protocol.
///
/// The defaults of the request rules are [`wire::Rules::DEFAULT`].
pub mod defaults {
    /// Minutes to wait for a manual answer before a reported state is sent.
    pub const REPORTED_ANSWER_DELAY_MINUTES: u32 = 15;
}

// Checked at compile time: the weekly limit must be the stricter rule,
// otherwise the minimum interval alone would already enforce it.
const _: () = {
    let rules = wire::Rules::DEFAULT;
    assert!(rules.weekly_limit as u32 <= 7 * 24 * 60 / rules.min_interval_minutes);
};
