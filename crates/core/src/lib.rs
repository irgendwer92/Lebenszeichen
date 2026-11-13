//! Client core: protocol, encryption, request rules and storage.
//!
//! The core has no dependency on Tauri or any user interface. The desktop
//! app, the Android layer and the command-line client all call into it.
//!
//! Nothing is implemented yet; the protocol follows in milestone M2.

pub use lz_wire as wire;

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
