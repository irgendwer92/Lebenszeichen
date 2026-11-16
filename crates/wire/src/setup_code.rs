//! Setup code: `LZS1-` followed by Base32. Gives a new device the relay
//! address and the access token of its relay, so that nothing has to be
//! typed.
//!
//! The code carries the access token itself and therefore does not expire:
//! whoever sees it can create mailboxes on the relay until the operator
//! changes the token. It is meant to be shown directly, never sent.

use crate::{Error, MAX_RELAY_URL_LEN, PROTOCOL_VERSION, Reader, RelayUrl, Token, Writer, base32};

/// Fixed start of every setup code.
pub const PREFIX: &str = "LZS1-";

/// Longest setup code in bytes, with the longest relay address.
pub const MAX_LEN: usize = 1 + 32 + 1 + MAX_RELAY_URL_LEN;

/// Longest setup code in characters, prefix included.
pub const MAX_TEXT_LEN: usize = PREFIX.len() + base32::encoded_len(MAX_LEN);

/// Relay account for a new device.
#[derive(Clone, PartialEq, Eq)]
pub struct SetupCode {
    pub access_token: Token,
    pub relay: RelayUrl,
}

impl core::fmt::Debug for SetupCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SetupCode")
            .field("access_token", &"..")
            .field("relay", &self.relay)
            .finish()
    }
}

impl SetupCode {
    /// Writes the binary form and returns its length.
    pub fn to_bytes(&self, out: &mut [u8; MAX_LEN]) -> usize {
        let mut w = Writer::new(out);
        self.write(&mut w).expect("MAX_LEN covers every setup code");
        w.position()
    }

    fn write(&self, w: &mut Writer<'_>) -> Result<(), Error> {
        w.put(&[PROTOCOL_VERSION])?;
        w.put(&self.access_token)?;
        w.text(&self.relay)
    }

    /// Reads the binary form. Trailing bytes are ignored.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(bytes);
        let version = r.u8()?;
        if version != PROTOCOL_VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        Ok(Self {
            access_token: r.array()?,
            relay: r.text()?,
        })
    }

    /// Writes the setup code into `out` and returns it.
    pub fn to_text<'a>(&self, out: &'a mut [u8; MAX_TEXT_LEN]) -> &'a str {
        let mut bytes = [0; MAX_LEN];
        let len = self.to_bytes(&mut bytes);
        base32::write_code(PREFIX, &bytes[..len], out)
            .expect("MAX_TEXT_LEN covers every setup code")
    }

    /// Reads a setup code as typed or pasted by a person.
    ///
    /// Lower case, spaces and line breaks are tolerated.
    pub fn from_text(text: &str) -> Result<Self, Error> {
        let mut bytes = [0; MAX_LEN];
        let len = base32::read_code::<MAX_TEXT_LEN>(text, PREFIX, &mut bytes)?;
        Self::from_bytes(&bytes[..len])
    }
}

/// Which kind of code a scanned or pasted text is, by its prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeKind {
    /// `LZ2-`: an invitation.
    Invitation,
    /// `LZ1-`: an invitation of version 1, recognized only to refuse it.
    OldInvitation,
    /// `LZD1-`: the device code of a new device.
    Device,
    /// `LZS1-`: a setup code.
    Setup,
}

impl CodeKind {
    /// Recognizes the kind of a code, ignoring case and leading whitespace.
    /// `None` for anything else.
    pub fn of(text: &str) -> Option<Self> {
        let text = text.trim_start();
        let starts = |prefix: &str| {
            text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix)
        };
        if starts(crate::device_code::PREFIX) {
            Some(Self::Device)
        } else if starts(PREFIX) {
            Some(Self::Setup)
        } else if starts(crate::invitation::PREFIX) {
            Some(Self::Invitation)
        } else if starts(crate::invitation::OLD_PREFIX) {
            Some(Self::OldInvitation)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CodeKind, MAX_LEN, MAX_TEXT_LEN, SetupCode};
    use crate::{Error, MAX_RELAY_URL_LEN, RelayUrl};

    fn code() -> SetupCode {
        SetupCode {
            access_token: [7; 32],
            relay: RelayUrl::new("https://r.x").unwrap(),
        }
    }

    #[test]
    fn layout_matches_documentation() {
        let mut buf = [0; MAX_LEN];
        let len = code().to_bytes(&mut buf);
        assert_eq!(len, 1 + 32 + 1 + 11);
        assert_eq!(buf[0], 1);
        assert_eq!(&buf[1..33], &[7; 32]);
        assert_eq!(buf[33], 11);
        assert_eq!(&buf[34..45], b"https://r.x");
    }

    #[test]
    fn fixed_text_vector() {
        let mut buf = [0; MAX_TEXT_LEN];
        let text = code().to_text(&mut buf);
        // Made with Python's base64.b32encode, padding removed.
        assert_eq!(
            text,
            "LZS1-AEDQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOC3IOR2HA4Z2F4XXELTY"
        );
        assert_eq!(SetupCode::from_text(text).unwrap(), code());
        assert_eq!(
            SetupCode::from_text(&text.to_lowercase().replace("aedq", "aedq \n")).unwrap(),
            code()
        );
    }

    #[test]
    fn longest_code_fits() {
        let mut longest = code();
        longest.relay = RelayUrl::new(&"r".repeat(MAX_RELAY_URL_LEN)).unwrap();
        let mut buf = [0; MAX_TEXT_LEN];
        let text = longest.to_text(&mut buf);
        assert_eq!(text.len(), MAX_TEXT_LEN);
        assert_eq!(SetupCode::from_text(text).unwrap(), longest);
    }

    #[test]
    fn rejects_bad_codes() {
        let mut buf = [0; MAX_TEXT_LEN];
        let text = code().to_text(&mut buf);
        assert_eq!(
            SetupCode::from_text(&text.replace("LZS1-", "LZD1-")),
            Err(Error::InvalidCode)
        );
        assert_eq!(
            SetupCode::from_text("LZS1-AI"),
            Err(Error::UnsupportedVersion(2))
        );
        assert_eq!(SetupCode::from_text("LZS1-AE"), Err(Error::Truncated));
    }

    #[test]
    fn debug_hides_the_token() {
        struct Buf([u8; 128], usize);
        impl core::fmt::Write for Buf {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                let end = self.1 + s.len();
                self.0
                    .get_mut(self.1..end)
                    .ok_or(core::fmt::Error)?
                    .copy_from_slice(s.as_bytes());
                self.1 = end;
                Ok(())
            }
        }
        let mut buf = Buf([0; 128], 0);
        core::fmt::write(&mut buf, format_args!("{:?}", code())).unwrap();
        let shown = core::str::from_utf8(&buf.0[..buf.1]).unwrap();
        assert!(!shown.contains("7, 7"), "{shown}");
        assert!(shown.contains("https://r.x"));
    }

    #[test]
    fn recognizes_the_kind_of_a_code() {
        assert_eq!(CodeKind::of("LZ2-AIAA"), Some(CodeKind::Invitation));
        assert_eq!(CodeKind::of(" lz1-aeaa"), Some(CodeKind::OldInvitation));
        assert_eq!(CodeKind::of("  lzd1-aeaa"), Some(CodeKind::Device));
        assert_eq!(CodeKind::of("LZS1-AEAA"), Some(CodeKind::Setup));
        assert_eq!(CodeKind::of("https://example.org"), None);
        assert_eq!(CodeKind::of("LZ"), None);
    }
}
