//! Wire format shared by clients and the relay.
//!
//! This crate is `no_std` on purpose: the wire format must stay usable on
//! microcontrollers and must not depend on an operating system. It also needs
//! no allocator; every value has a fixed upper bound and lives on the stack.
//!
//! The binding description is `docs/wire-format.md`. This crate covers the
//! byte layout only. Encryption, keys and the request rules live in `lz-core`.

#![no_std]

pub mod base32;
pub mod device_code;
pub mod invitation;
pub mod message;
pub mod record;
pub mod satellite;
pub mod setup_code;
mod text;

pub use device_code::DeviceCode;
pub use invitation::Invitation;
pub use message::{
    Body, ContactPart, DeviceAnnounce, Hello, Message, PartData, Requested, Responded, Response,
    ResponseKind, Rules, RulesSet, StateShared,
};
pub use record::{ContactRecord, Stamp};
pub use satellite::{
    ContactFlags, ContactSettings, ContactSettingsFor, Part as SatellitePart, Preset,
    Settings as SatelliteSettings, SettingsFor,
};
pub use setup_code::{CodeKind, SetupCode};
pub use text::{BoundedStr, ContactName, DeviceName, Reason, RelayUrl};

/// Version byte at the start of every envelope, device code and setup code.
/// Invitations have their own, [`invitation::VERSION`].
pub const PROTOCOL_VERSION: u8 = 1;

/// Every plaintext of an ordinary envelope is padded to this length before
/// encryption, so the relay cannot tell message types apart by size.
pub const PADDED_PLAINTEXT_LEN: usize = 256;

/// Plaintext length of a sealed envelope, used only for `hello`.
///
/// Shorter than [`PADDED_PLAINTEXT_LEN`] so that both forms of the envelope
/// have the same length on the wire.
pub const SEALED_PLAINTEXT_LEN: usize = 248;

/// Length of the random nonce in front of the ciphertext.
pub const NONCE_LEN: usize = 24;

/// Length of the authentication tag added by `crypto_box`.
pub const TAG_LEN: usize = 16;

/// Length of an X25519 public key.
pub const PUBLIC_KEY_LEN: usize = 32;

/// Length of a complete envelope: version byte, nonce, ciphertext with tag.
pub const ENVELOPE_LEN: usize = 1 + NONCE_LEN + PADDED_PLAINTEXT_LEN + TAG_LEN;

/// Largest request body the relay accepts for a single message.
pub const MAX_MESSAGE_BYTES: usize = 512;

/// Length of a mailbox identifier.
pub const MAILBOX_ID_LEN: usize = 32;

/// Length of read tokens, send tokens and vouchers.
pub const TOKEN_LEN: usize = 32;

/// Length of a message identifier.
pub const MESSAGE_ID_LEN: usize = 16;

/// Length of a reader identifier.
pub const READER_ID_LEN: usize = 16;

/// Longest relay address in bytes of UTF-8.
pub const MAX_RELAY_URL_LEN: usize = 120;

/// Longest reason of a reported response in bytes of UTF-8.
pub const MAX_REASON_LEN: usize = 64;

/// Longest name of a device in `device`, in bytes of UTF-8.
pub const MAX_DEVICE_NAME_LEN: usize = 32;

/// Longest local name of a contact in a contact record, in bytes of UTF-8.
pub const MAX_CONTACT_NAME_LEN: usize = 64;

/// Longest data of one `contact_part`.
pub const MAX_PART_LEN: usize = 178;

/// Longest handover the relay stores, sealed.
pub const MAX_HANDOVER_LEN: usize = 64 * 1024;

/// Lifetime of a device code; handover slots live as long on the relay.
pub const DEVICE_CODE_TTL_SECS: u64 = 10 * 60;

/// How long a device keeps sync messages it could not deliver or apply yet,
/// and incomplete contact transfers.
pub const SYNC_KEEP_SECS: u64 = 24 * 60 * 60;

/// A message is accepted only if it was sent at most this long ago.
pub const MAX_MESSAGE_AGE_SECS: u64 = 24 * 60 * 60 + 10 * 60;

/// A message is accepted only if its time lies at most this far ahead.
pub const MAX_CLOCK_AHEAD_SECS: u64 = 10 * 60;

/// How long a receiver remembers accepted message identifiers per contact.
pub const REPLAY_MEMORY_SECS: u64 = 25 * 60 * 60;

/// Default lifetime of an invitation.
pub const INVITATION_TTL_SECS: u64 = 10 * 60;

/// Public key of a contact.
pub type PublicKey = [u8; PUBLIC_KEY_LEN];

/// Identifier of a mailbox on a relay.
pub type MailboxId = [u8; MAILBOX_ID_LEN];

/// Read token, send token or voucher.
pub type Token = [u8; TOKEN_LEN];

/// Identifier of a message; for a request also the identifier of the request.
pub type MessageId = [u8; MESSAGE_ID_LEN];

/// Identifier of a device as a reader of mailboxes.
pub type ReaderId = [u8; READER_ID_LEN];

/// Identifier of one transfer of a contact in `contact_part`.
pub type TransferId = [u8; 16];

/// SHA-256 value, for example of a token or a handover slot.
pub type Hash = [u8; 32];

// Checked at compile time: an envelope must fit into one relay message, and
// the sealed form must have exactly the same length as the ordinary one.
// The largest `contact_part` must fill the body of the ordinary form exactly.
const _: () = assert!(ENVELOPE_LEN <= MAX_MESSAGE_BYTES);
const _: () = assert!(32 + 16 + 3 + MAX_PART_LEN == PADDED_PLAINTEXT_LEN - message::HEADER_LEN);
const _: () = assert!(ENVELOPE_LEN == 1 + PUBLIC_KEY_LEN + SEALED_PLAINTEXT_LEN + TAG_LEN);

/// Everything that can go wrong while reading or writing the wire format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The input ends before a required field.
    Truncated,
    /// A value or the output buffer exceeds its fixed limit.
    TooLong,
    /// A text field is not valid UTF-8.
    InvalidUtf8,
    /// A field holds a value the format does not define.
    InvalidValue,
    /// An invitation or device code is not valid Base32 or lacks its prefix.
    InvalidCode,
    /// The version byte is not the one this format expects.
    UnsupportedVersion(u8),
    /// An invitation of version 1 (`LZ1-`), which is no longer accepted;
    /// the person should ask for a new one.
    OldInvitation,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => f.write_str("input ends early"),
            Self::TooLong => f.write_str("value too long"),
            Self::InvalidUtf8 => f.write_str("text is not valid UTF-8"),
            Self::InvalidValue => f.write_str("undefined field value"),
            Self::InvalidCode => f.write_str("not a valid invitation code"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported protocol version {v}"),
            Self::OldInvitation => f.write_str("invitation of version 1"),
        }
    }
}

impl core::error::Error for Error {}

/// Checks length and version byte of an envelope received from the relay.
///
/// Says nothing about the content; whether it decrypts is up to the client.
pub fn check_envelope(bytes: &[u8]) -> Result<&[u8; ENVELOPE_LEN], Error> {
    let envelope: &[u8; ENVELOPE_LEN] = match bytes.try_into() {
        Ok(envelope) => envelope,
        Err(_) if bytes.len() < ENVELOPE_LEN => return Err(Error::Truncated),
        Err(_) => return Err(Error::TooLong),
    };
    match envelope[0] {
        PROTOCOL_VERSION => Ok(envelope),
        other => Err(Error::UnsupportedVersion(other)),
    }
}

/// Sequential reader over a byte slice.
pub(crate) struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let (head, rest) = self.rest.split_at_checked(n).ok_or(Error::Truncated)?;
        self.rest = rest;
        Ok(head)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.array::<1>()?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, Error> {
        self.array().map(u16::from_be_bytes)
    }

    pub(crate) fn u32(&mut self) -> Result<u32, Error> {
        self.array().map(u32::from_be_bytes)
    }

    pub(crate) fn u64(&mut self) -> Result<u64, Error> {
        self.array().map(u64::from_be_bytes)
    }

    /// Reads a length byte followed by that many bytes of UTF-8.
    pub(crate) fn text<const N: usize>(&mut self) -> Result<BoundedStr<N>, Error> {
        let len = self.u8()?;
        BoundedStr::from_utf8(self.take(len.into())?)
    }
}

/// Sequential writer into a byte slice.
pub(crate) struct Writer<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> Writer<'a> {
    pub(crate) fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    pub(crate) fn put(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let end = self.pos.checked_add(bytes.len()).ok_or(Error::TooLong)?;
        self.buf
            .get_mut(self.pos..end)
            .ok_or(Error::TooLong)?
            .copy_from_slice(bytes);
        self.pos = end;
        Ok(())
    }

    /// Writes a length byte followed by the text.
    pub(crate) fn text<const N: usize>(&mut self, text: &BoundedStr<N>) -> Result<(), Error> {
        self.put(&[text.len_u8()])?;
        self.put(text.as_str().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::{ENVELOPE_LEN, Error, PROTOCOL_VERSION, check_envelope};

    #[test]
    fn envelope_is_297_bytes() {
        assert_eq!(ENVELOPE_LEN, 297);
    }

    #[test]
    fn check_envelope_accepts_version_one() {
        let mut bytes = [0xAB; ENVELOPE_LEN];
        bytes[0] = PROTOCOL_VERSION;
        assert!(check_envelope(&bytes).is_ok());
    }

    #[test]
    fn check_envelope_rejects_wrong_length_and_version() {
        let mut bytes = [0; ENVELOPE_LEN + 1];
        bytes[0] = PROTOCOL_VERSION;
        assert_eq!(check_envelope(&bytes[..10]), Err(Error::Truncated));
        assert_eq!(check_envelope(&bytes), Err(Error::TooLong));
        bytes[0] = 2;
        assert_eq!(
            check_envelope(&bytes[..ENVELOPE_LEN]),
            Err(Error::UnsupportedVersion(2))
        );
    }
}
