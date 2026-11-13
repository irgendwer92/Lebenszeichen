//! Base32 after RFC 4648, section 6: upper-case alphabet, no padding.
//!
//! Used for invitation codes because it fits the alphanumeric mode of QR
//! codes and stays readable when typed by hand.

use crate::Error;

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Number of characters needed for `n` bytes.
pub const fn encoded_len(n: usize) -> usize {
    (n * 8).div_ceil(5)
}

/// Writes the Base32 form of `input` to the start of `out` and returns its
/// length.
pub fn encode(input: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let out = out
        .get_mut(..encoded_len(input.len()))
        .ok_or(Error::TooLong)?;
    let mut chars = out.iter_mut();
    let mut acc: u32 = 0;
    let mut bits = 0;
    for &byte in input {
        acc = (acc << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            *chars.next().expect("length computed above") = ALPHABET[(acc >> bits) as usize & 31];
        }
        acc &= (1 << bits) - 1;
    }
    if bits > 0 {
        *chars.next().expect("length computed above") = ALPHABET[(acc << (5 - bits)) as usize & 31];
    }
    Ok(encoded_len(input.len()))
}

/// Decodes upper-case Base32 without padding into the start of `out` and
/// returns the number of bytes.
///
/// Rejects characters outside the alphabet, lengths that no byte sequence
/// produces and non-zero trailing bits, so every byte sequence has exactly
/// one valid code.
pub fn decode(input: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let mut acc: u32 = 0;
    let mut bits = 0;
    let mut n = 0;
    for &c in input {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return Err(Error::InvalidCode),
        };
        acc = (acc << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            *out.get_mut(n).ok_or(Error::TooLong)? = (acc >> bits) as u8;
            n += 1;
            acc &= (1 << bits) - 1;
        }
    }
    if bits >= 5 || acc != 0 {
        return Err(Error::InvalidCode);
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::{decode, encode};
    use crate::Error;

    // Test vectors from RFC 4648, section 10, without padding.
    const VECTORS: [(&str, &str); 7] = [
        ("", ""),
        ("f", "MY"),
        ("fo", "MZXQ"),
        ("foo", "MZXW6"),
        ("foob", "MZXW6YQ"),
        ("fooba", "MZXW6YTB"),
        ("foobar", "MZXW6YTBOI"),
    ];

    #[test]
    fn matches_rfc_vectors() {
        for (plain, code) in VECTORS {
            let mut buf = [0; 16];
            let n = encode(plain.as_bytes(), &mut buf).unwrap();
            assert_eq!(&buf[..n], code.as_bytes());
            let n = decode(code.as_bytes(), &mut buf).unwrap();
            assert_eq!(&buf[..n], plain.as_bytes());
        }
    }

    #[test]
    fn round_trips_all_byte_values() {
        let mut input = [0; 256];
        for (i, b) in input.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut code = [0; 410];
        let n = encode(&input, &mut code).unwrap();
        let mut back = [0; 256];
        assert_eq!(decode(&code[..n], &mut back), Ok(256));
        assert_eq!(back, input);
    }

    #[test]
    fn rejects_non_canonical_codes() {
        let mut buf = [0; 8];
        assert_eq!(decode(b"MZ", &mut buf), Err(Error::InvalidCode));
        assert_eq!(decode(b"MZX", &mut buf), Err(Error::InvalidCode));
        assert_eq!(decode(b"my", &mut buf), Err(Error::InvalidCode));
        assert_eq!(decode(b"M1", &mut buf), Err(Error::InvalidCode));
    }

    #[test]
    fn reports_small_buffers() {
        let mut buf = [0; 3];
        assert_eq!(encode(b"fo", &mut buf), Err(Error::TooLong));
        assert_eq!(decode(b"MZXW6YQ", &mut buf), Err(Error::TooLong));
    }
}
