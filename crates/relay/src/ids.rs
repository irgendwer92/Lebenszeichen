//! Identifiers, tokens and their encodings.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::limits::{READER_ID_LEN, TOKEN_LEN};

/// A mailbox identifier.
pub type MailboxId = [u8; lz_wire::MAILBOX_ID_LEN];

/// A reader identifier, chosen by the client once per device.
pub type ReaderId = [u8; READER_ID_LEN];

/// A bearer token or voucher.
pub type Token = [u8; TOKEN_LEN];

/// A `SHA-256` hash.
pub type Hash = [u8; 32];

/// Encodes bytes as `b64url` without padding.
pub fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decodes `b64url` without padding into exactly `N` bytes.
pub fn decode<const N: usize>(text: &str) -> Option<[u8; N]> {
    // Longer input cannot decode to N bytes; reject it before allocating.
    if text.len() > N.div_ceil(3) * 4 {
        return None;
    }
    URL_SAFE_NO_PAD.decode(text).ok()?.try_into().ok()
}

/// Decodes 64 hex digits into a hash.
pub fn decode_hex_hash(text: &str) -> Option<Hash> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let digit = |c: u8| (c as char).to_digit(16);
    for (byte, &[high, low]) in out.iter_mut().zip(bytes.as_chunks::<2>().0) {
        *byte = (digit(high)? * 16 + digit(low)?) as u8;
    }
    Some(out)
}

/// `SHA-256` of a byte string.
pub fn sha256(bytes: &[u8]) -> Hash {
    Sha256::digest(bytes).into()
}

/// Checks a presented token against a stored hash in constant time.
pub fn token_matches(token: &Token, stored_hash: &Hash) -> bool {
    sha256(token).ct_eq(stored_hash).into()
}

/// Fresh random bytes from a cryptographically secure generator seeded by
/// the operating system.
pub fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64url_round_trip_and_strictness() {
        let bytes = [0xfbu8; 32];
        let text = encode(&bytes);
        assert_eq!(text.len(), 43);
        assert_eq!(decode::<32>(&text), Some(bytes));
        assert_eq!(decode::<32>(&format!("{text}=")), None);
        assert_eq!(decode::<16>(&text), None);
        assert_eq!(decode::<32>("not base64!"), None);
    }

    #[test]
    fn hex_hash() {
        let hash = sha256(b"x");
        let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(decode_hex_hash(&hex), Some(hash));
        assert_eq!(decode_hex_hash(&hex[1..]), None);
        assert_eq!(decode_hex_hash(&hex.to_uppercase()), Some(hash));
        assert_eq!(decode_hex_hash(&"zz".repeat(32)), None);
    }
}
