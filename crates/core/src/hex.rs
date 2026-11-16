//! Byte arrays as lower-case hex strings in stored JSON.
//!
//! Use with `#[serde(with = "crate::hex")]`.

use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

pub fn serialize<S: Serializer, const N: usize>(
    bytes: &[u8; N],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&encode(bytes))
}

pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
    deserializer: D,
) -> Result<[u8; N], D::Error> {
    let text = <&str>::deserialize(deserializer)?;
    decode(text).ok_or_else(|| D::Error::custom(format!("expected {} hex digits", 2 * N)))
}

/// For `Option<[u8; N]>`, stored as hex or `null`.
pub mod option {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S: Serializer, const N: usize>(
        bytes: &Option<[u8; N]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(bytes) => super::serialize(bytes, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        deserializer: D,
    ) -> Result<Option<[u8; N]>, D::Error> {
        Option::<&str>::deserialize(deserializer)?
            .map(|text| {
                super::decode(text)
                    .ok_or_else(|| D::Error::custom(format!("expected {} hex digits", 2 * N)))
            })
            .transpose()
    }
}

/// For `Vec<u8>` of any length.
pub mod bytes {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = <&str>::deserialize(deserializer)?;
        super::decode_vec(text).ok_or_else(|| D::Error::custom("expected hex digits"))
    }
}

pub fn decode_vec(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

pub fn encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn decode<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != 2 * N || !text.is_ascii() {
        return None;
    }
    let mut out = [0; N];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{decode, encode};

    #[test]
    fn round_trips() {
        let bytes = [0x00, 0x0f, 0xa5, 0xff];
        assert_eq!(encode(&bytes), "000fa5ff");
        assert_eq!(decode::<4>("000fa5ff"), Some(bytes));
        assert_eq!(decode::<4>("000FA5FF"), Some(bytes));
    }

    #[test]
    fn rejects_wrong_length_and_digits() {
        assert_eq!(decode::<2>("abc"), None);
        assert_eq!(decode::<2>("abcdef"), None);
        assert_eq!(decode::<2>("zz00"), None);
        assert_eq!(decode::<2>("ä0"), None);
    }
}
