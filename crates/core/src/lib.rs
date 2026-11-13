//! Client core: protocol, encryption, request rules and storage.
//!
//! The core has no dependency on Tauri or any user interface. The desktop
//! app, the Android layer and the command-line client all call into it.
//!
//! The protocol part takes time and randomness as arguments and has no
//! dependency on storage or the operating system, so it can later move to
//! devices without one.

pub use lz_wire as wire;

pub mod crypto;
pub mod pairing;

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
    /// A valid message that does not fit the current state.
    Unexpected,
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
            Self::Unexpected => f.write_str("unexpected message"),
        }
    }
}

impl std::error::Error for Error {}

/// Default limits for incoming requests, as fixed in the concept.
///
/// The person being asked can change them per contact.
pub mod defaults {
    /// Hours until an unanswered request expires.
    pub const REQUEST_TTL_HOURS: u32 = 24;

    /// Hours that must pass after an answer before the next request.
    pub const MIN_INTERVAL_HOURS: u32 = 6;

    /// Accepted requests per contact within any seven days.
    pub const WEEKLY_LIMIT: u32 = 7;

    /// Minutes to wait for a manual answer before a reported state is sent.
    pub const REPORTED_ANSWER_DELAY_MINUTES: u32 = 15;
}

// Checked at compile time: the weekly limit must be the stricter rule,
// otherwise the minimum interval alone would already enforce it.
const _: () = assert!(defaults::WEEKLY_LIMIT <= 7 * 24 / defaults::MIN_INTERVAL_HOURS);

/// Placeholder for the Android walking skeleton of milestone M1.
///
/// The Kotlin layer calls [`prototype::marker`] through JNI after a wake
/// signal, while no WebView is running. The returned line ends up in the
/// local event log and in the body of the test answer, so a received answer
/// proves that the core ran in the woken process. The module is removed when
/// the real protocol arrives in M2 and M4.
pub mod prototype {
    /// Fixed prefix of every marker line.
    pub const MARKER_PREFIX: &str = "lebenszeichen-m1";

    /// Longest accepted length of an event name or request identifier.
    pub const MAX_FIELD_LEN: usize = 64;

    /// Builds the marker line for an event of a request.
    ///
    /// Both inputs come from outside the core. Everything except ASCII
    /// letters, digits, `-` and `_` is dropped and the rest is cut to
    /// [`MAX_FIELD_LEN`], so the line always stays a single short line.
    #[must_use]
    pub fn marker(event: &str, request_id: &str) -> String {
        format!(
            "{MARKER_PREFIX} core=rust wire={} event={} request={}",
            crate::wire::PROTOCOL_VERSION,
            clean(event),
            clean(request_id),
        )
    }

    fn clean(field: &str) -> String {
        let cleaned: String = field
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .take(MAX_FIELD_LEN)
            .collect();
        if cleaned.is_empty() {
            "none".to_owned()
        } else {
            cleaned
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{MAX_FIELD_LEN, marker};

        #[test]
        fn marker_names_core_version_event_and_request() {
            assert_eq!(
                marker("dismiss", "r-123_a"),
                "lebenszeichen-m1 core=rust wire=1 event=dismiss request=r-123_a"
            );
        }

        #[test]
        fn marker_drops_unsafe_characters() {
            assert_eq!(
                marker("tap\nx y", "../ä?"),
                "lebenszeichen-m1 core=rust wire=1 event=tapxy request=none"
            );
        }

        #[test]
        fn marker_limits_field_length() {
            let long = "a".repeat(MAX_FIELD_LEN + 10);
            let line = marker(&long, "r");
            assert!(line.contains(&format!("event={} ", "a".repeat(MAX_FIELD_LEN))));
        }
    }
}
