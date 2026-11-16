//! Invitation passed on as QR code or text: `LZ2-` followed by Base32.
//!
//! Version 1 (`LZ1-`) had no commitment and is refused with
//! [`Error::OldInvitation`], so that interfaces can ask for a new one.

use crate::{
    Error, Hash, MAX_RELAY_URL_LEN, MailboxId, PublicKey, Reader, RelayUrl, Token, Writer, base32,
};

/// Fixed start of every invitation code.
pub const PREFIX: &str = "LZ2-";

/// Start of invitations of version 1, recognized only to refuse them.
pub const OLD_PREFIX: &str = "LZ1-";

/// Version byte at the start of an invitation.
pub const VERSION: u8 = 2;

/// Longest invitation in bytes, with voucher and longest relay address.
pub const MAX_LEN: usize = 1 + 1 + 8 + 32 + 32 + 32 + 32 + 32 + 1 + MAX_RELAY_URL_LEN;

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
    /// `SHA-256("lebenszeichen-v2-invite-commit" || n)` over the secret
    /// value `n` that the inviting side reveals after the `hello`.
    pub commitment: Hash,
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
        w.put(&[VERSION, flags])?;
        w.put(&self.expires_at.to_be_bytes())?;
        w.put(&self.mailbox)?;
        w.put(&self.send_token)?;
        w.put(&self.public_key)?;
        w.put(&self.commitment)?;
        if let Some(voucher) = &self.voucher {
            w.put(voucher)?;
        }
        w.text(&self.relay)
    }

    /// Reads the binary form. Unknown flags and trailing bytes are ignored.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(bytes);
        let version = r.u8()?;
        if version != VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        let flags = r.u8()?;
        Ok(Self {
            expires_at: r.u64()?,
            mailbox: r.array()?,
            send_token: r.array()?,
            public_key: r.array()?,
            commitment: r.array()?,
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
        base32::write_code(PREFIX, &bytes[..len], out)
            .expect("MAX_TEXT_LEN covers every invitation")
    }

    /// Reads an invitation code as typed or pasted by a person.
    ///
    /// Lower case, spaces and line breaks are tolerated. An invitation of
    /// version 1 fails with [`Error::OldInvitation`].
    pub fn from_text(text: &str) -> Result<Self, Error> {
        if is_old(text) {
            return Err(Error::OldInvitation);
        }
        let mut bytes = [0; MAX_LEN];
        let len = base32::read_code::<MAX_TEXT_LEN>(text, PREFIX, &mut bytes)?;
        Self::from_bytes(&bytes[..len])
    }
}

/// Whether a text starts like an invitation of version 1, ignoring case and
/// whitespace.
pub(crate) fn is_old(text: &str) -> bool {
    let mut start = text.bytes().filter(|c| !c.is_ascii_whitespace());
    OLD_PREFIX
        .bytes()
        .all(|p| start.next().is_some_and(|c| c.to_ascii_uppercase() == p))
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
            commitment: [5; 32],
            voucher,
            relay: RelayUrl::new("https://relay.example.org").unwrap(),
        }
    }

    #[test]
    fn layout_matches_documentation() {
        let mut buf = [0; MAX_LEN];
        let len = invitation(Some([4; 32])).to_bytes(&mut buf);
        assert_eq!(len, 1 + 1 + 8 + 5 * 32 + 1 + 25);
        assert_eq!(&buf[..2], &[2, 1]);
        assert_eq!(&buf[2..10], &1_794_600_000u64.to_be_bytes());
        assert_eq!(&buf[10..42], &[1; 32]);
        assert_eq!(&buf[42..74], &[2; 32]);
        assert_eq!(&buf[74..106], &[3; 32]);
        assert_eq!(&buf[106..138], &[5; 32]);
        assert_eq!(&buf[138..170], &[4; 32]);
        assert_eq!(buf[170], 25);
        assert_eq!(&buf[171..196], b"https://relay.example.org");

        let len = invitation(None).to_bytes(&mut buf);
        assert_eq!(len, 1 + 1 + 8 + 4 * 32 + 1 + 25);
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[106..138], &[5; 32]);
        assert_eq!(buf[138], 25);
    }

    #[test]
    fn fixed_text_vector() {
        let mut inv = invitation(Some([4; 32]));
        inv.relay = RelayUrl::new("https://r.x").unwrap();
        let mut buf = [0; MAX_TEXT_LEN];
        // Made with Python's base64.b32encode, padding removed.
        let text = inv.to_text(&mut buf);
        assert_eq!(
            text,
            concat!(
                "LZ2-AIAQAAAAABVPO3CAAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQEAQCAIBA",
                "EAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQDAMBQGAYDAMBQGAYDAMBQGAYDAMBQGAYDAMBQ",
                "GAYDAMBQGAYDAMCQKBIFAUCQKBIFAUCQKBIFAUCQKBIFAUCQKBIFAUCQKBIFAUCQKBAEAQCAIBAEAQCA",
                "IBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEBNUHI5DQOM5C6L3SFZ4A",
            )
        );
        assert_eq!(Invitation::from_text(text).unwrap(), inv);
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
        assert_eq!(Invitation::from_text("LZS1-AI"), Err(Error::InvalidCode));
        assert_eq!(Invitation::from_text("LZ2-A!"), Err(Error::InvalidCode));
        assert_eq!(Invitation::from_text("LZ2-AI"), Err(Error::Truncated));
        // Version byte 1 encodes to "AE", 3 to "AM".
        assert_eq!(
            Invitation::from_text("LZ2-AE"),
            Err(Error::UnsupportedVersion(1))
        );
        assert_eq!(
            Invitation::from_text("LZ2-AM"),
            Err(Error::UnsupportedVersion(3))
        );
    }

    #[test]
    fn version_one_is_refused_distinctly() {
        // The start of an invitation of version 1 as the previous release
        // wrote it: no commitment, prefix `LZ1-`.
        let old = "LZ1-AEAAAAAAABVPO3CAAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQEAQCAIB";
        assert_eq!(Invitation::from_text(old), Err(Error::OldInvitation));
        assert_eq!(Invitation::from_text("  lz1-x"), Err(Error::OldInvitation));
        assert_eq!(Invitation::from_text("L Z1-"), Err(Error::OldInvitation));
        assert_ne!(Invitation::from_text("LZ2-AE"), Err(Error::OldInvitation));
    }
}
