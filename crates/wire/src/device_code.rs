//! Device code shown by a new device: `LZD1-` followed by Base32.

use crate::{
    Error, Hash, MAX_RELAY_URL_LEN, PROTOCOL_VERSION, PublicKey, Reader, ReaderId, RelayUrl,
    Writer, base32,
};

/// Fixed start of every device code.
pub const PREFIX: &str = "LZD1-";

/// Longest device code in bytes, with the longest relay address.
pub const MAX_LEN: usize = 1 + 8 + 32 + 16 + 32 + 32 + 1 + MAX_RELAY_URL_LEN;

/// Longest device code in characters, prefix included.
pub const MAX_TEXT_LEN: usize = PREFIX.len() + base32::encoded_len(MAX_LEN);

/// What an existing device needs to add the new device that shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCode {
    /// The existing device refuses the code after this time.
    pub expires_at: u64,
    /// Public handover key of the new device.
    pub handover_key: PublicKey,
    /// Reader identifier of the new device.
    pub reader: ReaderId,
    /// `SHA-256(fetch token)`.
    pub fetch_hash: Hash,
    /// `SHA-256("lebenszeichen-v1-commit" || n)`.
    pub commitment: Hash,
    /// Relay on which the existing device stores the slots.
    pub relay: RelayUrl,
}

impl DeviceCode {
    /// Writes the binary form and returns its length.
    pub fn to_bytes(&self, out: &mut [u8; MAX_LEN]) -> usize {
        let mut w = Writer::new(out);
        self.write(&mut w)
            .expect("MAX_LEN covers every device code");
        w.position()
    }

    fn write(&self, w: &mut Writer<'_>) -> Result<(), Error> {
        w.put(&[PROTOCOL_VERSION])?;
        w.put(&self.expires_at.to_be_bytes())?;
        w.put(&self.handover_key)?;
        w.put(&self.reader)?;
        w.put(&self.fetch_hash)?;
        w.put(&self.commitment)?;
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
            expires_at: r.u64()?,
            handover_key: r.array()?,
            reader: r.array()?,
            fetch_hash: r.array()?,
            commitment: r.array()?,
            relay: r.text()?,
        })
    }

    /// Writes the device code into `out` and returns it.
    pub fn to_text<'a>(&self, out: &'a mut [u8; MAX_TEXT_LEN]) -> &'a str {
        let mut bytes = [0; MAX_LEN];
        let len = self.to_bytes(&mut bytes);
        base32::write_code(PREFIX, &bytes[..len], out)
            .expect("MAX_TEXT_LEN covers every device code")
    }

    /// Reads a device code as typed or pasted by a person.
    ///
    /// Lower case, spaces and line breaks are tolerated.
    pub fn from_text(text: &str) -> Result<Self, Error> {
        let mut bytes = [0; MAX_LEN];
        let len = base32::read_code::<MAX_TEXT_LEN>(text, PREFIX, &mut bytes)?;
        Self::from_bytes(&bytes[..len])
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceCode, MAX_LEN, MAX_TEXT_LEN, PREFIX};
    use crate::{Error, MAX_RELAY_URL_LEN, RelayUrl};

    fn code() -> DeviceCode {
        DeviceCode {
            expires_at: 1_794_600_600,
            handover_key: [1; 32],
            reader: [2; 16],
            fetch_hash: [3; 32],
            commitment: [4; 32],
            relay: RelayUrl::new("https://relay.example.org").unwrap(),
        }
    }

    #[test]
    fn layout_matches_documentation() {
        let mut buf = [0; MAX_LEN];
        let len = code().to_bytes(&mut buf);
        assert_eq!(len, 1 + 8 + 32 + 16 + 32 + 32 + 1 + 25);
        assert_eq!(buf[0], 1);
        assert_eq!(&buf[1..9], &1_794_600_600u64.to_be_bytes());
        assert_eq!(&buf[9..41], &[1; 32]);
        assert_eq!(&buf[41..57], &[2; 16]);
        assert_eq!(&buf[57..89], &[3; 32]);
        assert_eq!(&buf[89..121], &[4; 32]);
        assert_eq!(buf[121], 25);
        assert_eq!(&buf[122..147], b"https://relay.example.org");
    }

    #[test]
    fn fixed_text_vector() {
        let mut short = code();
        short.relay = RelayUrl::new("https://r.x").unwrap();
        let mut buf = [0; MAX_TEXT_LEN];
        let text = short.to_text(&mut buf);
        // Made with Python's base64.b32encode, padding removed.
        assert_eq!(
            text,
            "LZD1-AEAAAAAANL3W5GABAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEBAEAQCAIBAE\
             AQCAIBAEAQCAIBAGAYDAMBQGAYDAMBQGAYDAMBQGAYDAMBQGAYDAMBQGAYDAMBQGAYEAQCAIBAEAQCAIB\
             AEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQFWQ5DUOBZTULZPOIXHQ"
        );
        assert_eq!(text.len(), PREFIX.len() + (133 * 8usize).div_ceil(5));
        assert_eq!(DeviceCode::from_text(text).unwrap(), short);
    }

    #[test]
    fn longest_code_fits_and_round_trips() {
        let mut longest = code();
        longest.relay = RelayUrl::new(&"r".repeat(MAX_RELAY_URL_LEN)).unwrap();
        let mut buf = [0; MAX_TEXT_LEN];
        let text = longest.to_text(&mut buf);
        assert_eq!(text.len(), MAX_TEXT_LEN);
        assert_eq!(DeviceCode::from_text(text).unwrap(), longest);
    }

    #[test]
    fn tolerates_lower_case_and_whitespace() {
        let mut buf = [0; MAX_TEXT_LEN];
        let text = code().to_text(&mut buf);
        let mut sloppy = [0u8; 2 * MAX_TEXT_LEN];
        let mut len = 0;
        for (i, c) in text.bytes().enumerate() {
            if i > 0 && i % 6 == 0 {
                sloppy[len] = if i % 24 == 0 { b'\n' } else { b' ' };
                len += 1;
            }
            sloppy[len] = c.to_ascii_lowercase();
            len += 1;
        }
        let sloppy = core::str::from_utf8(&sloppy[..len]).unwrap();
        assert_eq!(DeviceCode::from_text(sloppy).unwrap(), code());
    }

    #[test]
    fn rejects_bad_codes() {
        let mut buf = [0; MAX_TEXT_LEN];
        let text = code().to_text(&mut buf);
        // An invitation prefix is not a device code.
        assert_eq!(
            DeviceCode::from_text(&text.replace("LZD1-", "LZ1-")),
            Err(Error::InvalidCode)
        );
        assert_eq!(DeviceCode::from_text("LZD1-A!"), Err(Error::InvalidCode));
        assert_eq!(DeviceCode::from_text("LZD1-AE"), Err(Error::Truncated));
        assert_eq!(
            DeviceCode::from_text("LZD1-AI"),
            Err(Error::UnsupportedVersion(2))
        );
        let mut bytes = [0; MAX_LEN];
        let len = code().to_bytes(&mut bytes);
        assert_eq!(
            DeviceCode::from_bytes(&bytes[..len - 1]),
            Err(Error::Truncated)
        );
        let mut long = [b'r'; 300];
        long[..len].copy_from_slice(&bytes[..len]);
        long[121] = 121;
        assert_eq!(DeviceCode::from_bytes(&long), Err(Error::TooLong));
    }
}
