//! Invitation passed on as QR code or text: `LZ1-` followed by Base32.

use crate::{
    Error, MAX_RELAY_URL_LEN, MailboxId, PROTOCOL_VERSION, PublicKey, Reader, RelayUrl, Token,
    Writer, base32,
};

/// Fixed start of every invitation code.
pub const PREFIX: &str = "LZ1-";

/// Longest invitation in bytes, with voucher and longest relay address.
pub const MAX_LEN: usize = 1 + 1 + 8 + 32 + 32 + 32 + 32 + 1 + MAX_RELAY_URL_LEN;

/// Longest invitation code in characters, prefix included.
pub const MAX_TEXT_LEN: usize = PREFIX.len() + base32::encoded_len(MAX_LEN);

const FLAG_VOUCHER: u8 = 1;

/// Everything the invitee needs to accept the invitation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invitation {
    /// The inviting side accepts a `hello` only before this time.
    pub expires_at: u64,
    /// Mailbox of the inviting side, into which the invitee sends `hello`.
    pub mailbox: MailboxId,
    pub send_token: Token,
    /// Contact key of the inviting side.
    pub public_key: PublicKey,
    /// Allows the invitee to create one mailbox on the relay of the inviting
    /// side, if it has no relay of its own.
    pub voucher: Option<Token>,
    pub relay: RelayUrl,
}

impl Invitation {
    /// Writes the binary form and returns its length.
    pub fn to_bytes(&self, out: &mut [u8; MAX_LEN]) -> usize {
        let mut w = Writer::new(out);
        self.write(&mut w).expect("MAX_LEN covers every invitation");
        w.position()
    }

    fn write(&self, w: &mut Writer<'_>) -> Result<(), Error> {
        let flags = if self.voucher.is_some() {
            FLAG_VOUCHER
        } else {
            0
        };
        w.put(&[PROTOCOL_VERSION, flags])?;
        w.put(&self.expires_at.to_be_bytes())?;
        w.put(&self.mailbox)?;
        w.put(&self.send_token)?;
        w.put(&self.public_key)?;
        if let Some(voucher) = &self.voucher {
            w.put(voucher)?;
        }
        w.text(&self.relay)
    }

    /// Reads the binary form. Unknown flags and trailing bytes are ignored.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(bytes);
        let version = r.u8()?;
        if version != PROTOCOL_VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        let flags = r.u8()?;
        Ok(Self {
            expires_at: r.u64()?,
            mailbox: r.array()?,
            send_token: r.array()?,
            public_key: r.array()?,
            voucher: if flags & FLAG_VOUCHER != 0 {
                Some(r.array()?)
            } else {
                None
            },
            relay: r.text()?,
        })
    }

    /// Writes the invitation code into `out` and returns it.
    pub fn to_text<'a>(&self, out: &'a mut [u8; MAX_TEXT_LEN]) -> &'a str {
        let mut bytes = [0; MAX_LEN];
        let len = self.to_bytes(&mut bytes);
        out[..PREFIX.len()].copy_from_slice(PREFIX.as_bytes());
        let code_len = base32::encode(&bytes[..len], &mut out[PREFIX.len()..])
            .expect("MAX_TEXT_LEN covers every invitation");
        core::str::from_utf8(&out[..PREFIX.len() + code_len]).expect("Base32 is ASCII")
    }

    /// Reads an invitation code as typed or pasted by a person.
    ///
    /// Lower case, spaces and line breaks are tolerated.
    pub fn from_text(text: &str) -> Result<Self, Error> {
        let mut clean = [0; MAX_TEXT_LEN];
        let mut len = 0;
        for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
            *clean.get_mut(len).ok_or(Error::TooLong)? = c.to_ascii_uppercase();
            len += 1;
        }
        let code = clean[..len]
            .strip_prefix(PREFIX.as_bytes())
            .ok_or(Error::InvalidCode)?;
        let mut bytes = [0; MAX_LEN];
        let len = base32::decode(code, &mut bytes)?;
        Self::from_bytes(&bytes[..len])
    }
}

#[cfg(test)]
mod tests {
    use super::{Invitation, MAX_LEN, MAX_TEXT_LEN, PREFIX};
    use crate::{Error, MAX_RELAY_URL_LEN, RelayUrl};

    fn invitation(voucher: Option<[u8; 32]>) -> Invitation {
        Invitation {
            expires_at: 1_794_600_000,
            mailbox: [1; 32],
            send_token: [2; 32],
            public_key: [3; 32],
            voucher,
            relay: RelayUrl::new("https://relay.example.org").unwrap(),
        }
    }

    #[test]
    fn layout_matches_documentation() {
        let mut buf = [0; MAX_LEN];
        let len = invitation(Some([4; 32])).to_bytes(&mut buf);
        assert_eq!(len, 1 + 1 + 8 + 4 * 32 + 1 + 25);
        assert_eq!(&buf[..2], &[1, 1]);
        assert_eq!(&buf[2..10], &1_794_600_000u64.to_be_bytes());
        assert_eq!(&buf[10..42], &[1; 32]);
        assert_eq!(&buf[42..74], &[2; 32]);
        assert_eq!(&buf[74..106], &[3; 32]);
        assert_eq!(&buf[106..138], &[4; 32]);
        assert_eq!(buf[138], 25);
        assert_eq!(&buf[139..164], b"https://relay.example.org");

        let len = invitation(None).to_bytes(&mut buf);
        assert_eq!(len, 1 + 1 + 8 + 3 * 32 + 1 + 25);
        assert_eq!(buf[1], 0);
    }

    #[test]
    fn text_round_trips_with_and_without_voucher() {
        for inv in [invitation(None), invitation(Some([4; 32]))] {
            let mut buf = [0; MAX_TEXT_LEN];
            let text = inv.to_text(&mut buf);
            assert!(text.starts_with(PREFIX));
            assert!(
                text[PREFIX.len()..]
                    .bytes()
                    .all(|c| c.is_ascii_uppercase() || (b'2'..=b'7').contains(&c))
            );
            assert_eq!(Invitation::from_text(text).unwrap(), inv);
        }
    }

    #[test]
    fn longest_invitation_fits() {
        let mut inv = invitation(Some([4; 32]));
        inv.relay = RelayUrl::new(&"r".repeat(MAX_RELAY_URL_LEN)).unwrap();
        let mut buf = [0; MAX_TEXT_LEN];
        let text = inv.to_text(&mut buf);
        assert_eq!(text.len(), MAX_TEXT_LEN);
        assert_eq!(Invitation::from_text(text).unwrap(), inv);
    }

    #[test]
    fn tolerates_lower_case_and_whitespace() {
        let inv = invitation(None);
        let mut buf = [0; MAX_TEXT_LEN];
        let text = inv.to_text(&mut buf);
        let mut sloppy = [0u8; 2 * MAX_TEXT_LEN];
        let mut len = 0;
        for (i, c) in text.bytes().enumerate() {
            if i > 0 && i % 8 == 0 {
                sloppy[len] = if i % 32 == 0 { b'\n' } else { b' ' };
                len += 1;
            }
            sloppy[len] = c.to_ascii_lowercase();
            len += 1;
        }
        let sloppy = core::str::from_utf8(&sloppy[..len]).unwrap();
        assert_eq!(Invitation::from_text(sloppy).unwrap(), inv);
    }

    #[test]
    fn rejects_bad_codes() {
        assert_eq!(Invitation::from_text("LZ2-AE"), Err(Error::InvalidCode));
        assert_eq!(Invitation::from_text("LZ1-A!"), Err(Error::InvalidCode));
        assert_eq!(Invitation::from_text("LZ1-AE"), Err(Error::Truncated));
        // Version byte 2 encodes to "AI".
        assert_eq!(
            Invitation::from_text("LZ1-AI"),
            Err(Error::UnsupportedVersion(2))
        );
    }
}
