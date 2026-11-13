//! Short UTF-8 texts with a fixed upper bound, stored without an allocator.

use crate::{Error, MAX_REASON_LEN, MAX_RELAY_URL_LEN};

/// UTF-8 text of at most `N` bytes, with `N` at most 255.
///
/// On the wire such a text is a length byte followed by the bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoundedStr<const N: usize> {
    len: u8,
    bytes: [u8; N],
}

/// Base address of a relay, for example `https://relay.example.org`.
pub type RelayUrl = BoundedStr<MAX_RELAY_URL_LEN>;

/// Reason of a reported response, chosen by the person asked.
pub type Reason = BoundedStr<MAX_REASON_LEN>;

impl<const N: usize> BoundedStr<N> {
    /// Copies the text, or fails with [`Error::TooLong`] above `N` bytes.
    pub fn new(text: &str) -> Result<Self, Error> {
        const { assert!(N <= u8::MAX as usize) };
        let bytes = text.as_bytes();
        if bytes.len() > N {
            return Err(Error::TooLong);
        }
        let mut out = Self {
            len: 0,
            bytes: [0; N],
        };
        out.bytes[..bytes.len()].copy_from_slice(bytes);
        out.len = bytes.len() as u8;
        Ok(out)
    }

    /// Checks bytes read from the wire.
    pub fn from_utf8(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > N {
            return Err(Error::TooLong);
        }
        Self::new(core::str::from_utf8(bytes).map_err(|_| Error::InvalidUtf8)?)
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len.into()]).expect("checked on construction")
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn len_u8(&self) -> u8 {
        self.len
    }
}

impl<const N: usize> core::fmt::Debug for BoundedStr<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self.as_str(), f)
    }
}

impl<const N: usize> core::fmt::Display for BoundedStr<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::BoundedStr;
    use crate::Error;

    #[test]
    fn keeps_text_up_to_limit() {
        let text = BoundedStr::<6>::new("schläft").unwrap_err();
        assert_eq!(text, Error::TooLong);
        assert_eq!(BoundedStr::<8>::new("schläft").unwrap().as_str(), "schläft");
    }

    #[test]
    fn rejects_invalid_utf8() {
        assert_eq!(BoundedStr::<8>::from_utf8(&[0xC3]), Err(Error::InvalidUtf8));
    }
}
