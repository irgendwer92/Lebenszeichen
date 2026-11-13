//! Envelopes: encryption of plaintexts between two contact keys.
//!
//! Byte-compatible with libsodium: the ordinary form is `crypto_box_easy`, the
//! sealed form `crypto_box_seal`. Both put the 16-byte tag in front of the
//! ciphertext.

use crypto_box::aead::AeadInPlace;
use crypto_box::{SalsaBox, SecretKey};
use rand_core::CryptoRngCore;
use sha2::{Digest, Sha256};

use crate::Error;
use crate::wire::{
    ENVELOPE_LEN, Message, NONCE_LEN, PADDED_PLAINTEXT_LEN, PROTOCOL_VERSION, PublicKey,
    SEALED_PLAINTEXT_LEN, TAG_LEN, check_envelope,
};

/// Secret X25519 key that belongs to exactly one contact.
#[derive(Clone)]
pub struct ContactKey(SecretKey);

impl ContactKey {
    pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
        Self(SecretKey::generate(rng))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(SecretKey::from_bytes(bytes))
    }

    /// The secret bytes, for storage on the device only.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    pub fn public_key(&self) -> PublicKey {
        self.0.public_key().to_bytes()
    }
}

impl core::fmt::Debug for ContactKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("ContactKey")
            .field(&self.public_key())
            .finish()
    }
}

const NONCE: core::ops::Range<usize> = 1..1 + NONCE_LEN;
const TAG: core::ops::Range<usize> = NONCE.end..NONCE.end + TAG_LEN;
const CIPHERTEXT: core::ops::RangeFrom<usize> = TAG.end..;

/// Encrypts a message in the ordinary form.
pub fn seal(
    message: &Message,
    sender: &ContactKey,
    recipient: &PublicKey,
    rng: &mut impl CryptoRngCore,
) -> Result<[u8; ENVELOPE_LEN], Error> {
    let mut envelope = [0; ENVELOPE_LEN];
    envelope[0] = PROTOCOL_VERSION;
    rng.fill_bytes(&mut envelope[NONCE]);
    message.encode(&mut envelope[CIPHERTEXT])?;

    let (head, ciphertext) = envelope.split_at_mut(CIPHERTEXT.start);
    let tag = SalsaBox::new(&(*recipient).into(), &sender.0)
        .encrypt_in_place_detached(head[NONCE].into(), b"", ciphertext)
        .map_err(|_| Error::Encrypt)?;
    head[TAG].copy_from_slice(&tag);
    Ok(envelope)
}

/// Decrypts an envelope in the ordinary form.
///
/// Fails with [`Error::Decrypt`] if it was not encrypted between exactly
/// these two keys or was altered on the way.
pub fn open(envelope: &[u8], recipient: &ContactKey, sender: &PublicKey) -> Result<Message, Error> {
    let envelope = check_envelope(envelope)?;
    let mut plaintext = [0; PADDED_PLAINTEXT_LEN];
    plaintext.copy_from_slice(&envelope[CIPHERTEXT]);
    SalsaBox::new(&(*sender).into(), &recipient.0)
        .decrypt_in_place_detached(
            envelope[NONCE].into(),
            b"",
            &mut plaintext,
            envelope[TAG].into(),
        )
        .map_err(|_| Error::Decrypt)?;
    Ok(Message::decode(&plaintext)?)
}

/// Encrypts a message in the sealed form, without a sender key.
///
/// Used only for `hello`, because the recipient does not know the sender yet.
pub fn seal_anonymous(
    message: &Message,
    recipient: &PublicKey,
    rng: &mut impl CryptoRngCore,
) -> Result<[u8; ENVELOPE_LEN], Error> {
    let mut plaintext = [0; SEALED_PLAINTEXT_LEN];
    message.encode(&mut plaintext)?;
    let sealed = crypto_box::PublicKey::from(*recipient)
        .seal(rng, &plaintext)
        .map_err(|_| Error::Encrypt)?;
    let mut envelope = [0; ENVELOPE_LEN];
    envelope[0] = PROTOCOL_VERSION;
    envelope[1..].copy_from_slice(&sealed);
    Ok(envelope)
}

/// Decrypts an envelope in the sealed form.
pub fn open_anonymous(envelope: &[u8], recipient: &ContactKey) -> Result<Message, Error> {
    let envelope = check_envelope(envelope)?;
    let plaintext = recipient
        .0
        .unseal(&envelope[1..])
        .map_err(|_| Error::Decrypt)?;
    Ok(Message::decode(&plaintext)?)
}

/// Six digits both sides show after pairing, derived from both public keys.
///
/// The order of the arguments does not matter.
pub fn verification_code(a: &PublicKey, b: &PublicKey) -> String {
    let (low, high) = if a <= b { (a, b) } else { (b, a) };
    let hash = Sha256::new()
        .chain_update(b"lebenszeichen-v1-code")
        .chain_update(low)
        .chain_update(high)
        .finalize();
    let number = u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]);
    format!("{:06}", number % 1_000_000)
}

#[cfg(test)]
mod tests {
    use rand_core::OsRng;

    use super::{ContactKey, open, open_anonymous, seal, seal_anonymous, verification_code};
    use crate::Error;
    use crate::wire::{Body, ENVELOPE_LEN, Message, PROTOCOL_VERSION};

    fn request() -> Message {
        Message {
            id: [5; 16],
            sent_at: 1_794_600_000,
            body: Body::Request,
        }
    }

    #[test]
    fn ordinary_form_round_trips() {
        let a = ContactKey::generate(&mut OsRng);
        let b = ContactKey::generate(&mut OsRng);
        let envelope = seal(&request(), &a, &b.public_key(), &mut OsRng).unwrap();
        assert_eq!(envelope.len(), ENVELOPE_LEN);
        assert_eq!(envelope[0], PROTOCOL_VERSION);
        assert_eq!(open(&envelope, &b, &a.public_key()).unwrap(), request());
    }

    #[test]
    fn ordinary_form_rejects_wrong_keys_and_tampering() {
        let a = ContactKey::generate(&mut OsRng);
        let b = ContactKey::generate(&mut OsRng);
        let c = ContactKey::generate(&mut OsRng);
        let mut envelope = seal(&request(), &a, &b.public_key(), &mut OsRng).unwrap();
        assert_eq!(open(&envelope, &b, &c.public_key()), Err(Error::Decrypt));
        assert_eq!(open(&envelope, &c, &a.public_key()), Err(Error::Decrypt));
        envelope[200] ^= 1;
        assert_eq!(open(&envelope, &b, &a.public_key()), Err(Error::Decrypt));
    }

    #[test]
    fn sealed_form_round_trips_and_has_same_length() {
        let b = ContactKey::generate(&mut OsRng);
        let envelope = seal_anonymous(&request(), &b.public_key(), &mut OsRng).unwrap();
        assert_eq!(envelope.len(), ENVELOPE_LEN);
        assert_eq!(open_anonymous(&envelope, &b).unwrap(), request());
        let c = ContactKey::generate(&mut OsRng);
        assert_eq!(open_anonymous(&envelope, &c), Err(Error::Decrypt));
    }

    #[test]
    fn forms_do_not_open_each_other() {
        let a = ContactKey::generate(&mut OsRng);
        let b = ContactKey::generate(&mut OsRng);
        let sealed = seal_anonymous(&request(), &b.public_key(), &mut OsRng).unwrap();
        assert_eq!(open(&sealed, &b, &a.public_key()), Err(Error::Decrypt));
        let ordinary = seal(&request(), &a, &b.public_key(), &mut OsRng).unwrap();
        assert_eq!(open_anonymous(&ordinary, &b), Err(Error::Decrypt));
    }

    #[test]
    fn verification_code_is_symmetric_and_six_digits() {
        let a = ContactKey::generate(&mut OsRng).public_key();
        let b = ContactKey::generate(&mut OsRng).public_key();
        let code = verification_code(&a, &b);
        assert_eq!(code, verification_code(&b, &a));
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn verification_code_matches_documented_formula() {
        // SHA-256("lebenszeichen-v1-code" || 32 × 0x01 || 32 × 0x02), first
        // four bytes as big-endian number, modulo one million.
        assert_eq!(verification_code(&[2; 32], &[1; 32]), "259986");
    }

    // Envelopes made with libsodium through PyNaCl 1.6.2: contact keys
    // 32 × 0x0a (sender) and 32 × 0x0b (recipient), the plaintext of
    // `request()`. The ordinary form uses the nonce 0, 1, …, 23.
    const LIBSODIUM_ORDINARY: &[&str] = &[
        "01000102030405060708090a0b0c0d0e0f1011121314151617e2e349be0493cc",
        "40ed57db98bdb08a40d0a682be173c356c77f6a8b88321e17d69dcf2899727ae",
        "53efca9c4cd837ec475be9ad9be60ffc68f527e153eee45a7bdb2ccc67be5bda",
        "27da7a414f60ea6e44750d1c9dd2064cc2bdf37d1774d20627ebdff880bf50bb",
        "fa40ed7b5dd259c3263baae46e11ffd314f865e74f3ce6a4458ac3e8d59a2272",
        "750ba493f32172742b127e9ffa7685dbe996e8b27a15871956c6959165c004e9",
        "bb244211241a5c42a77a6e4c04b4e11c990b50f85c9c38d8610e361ae4a4632e",
        "dbd214b48ed99382d62b54df6282bea96b3d26242a659d56324c121521c170c9",
        "99871e1bfcdb008d32268c83439997cdd50dd3706144d8457137e8ef561733d8",
        "f6721a207ae1f09bbb",
    ];
    const LIBSODIUM_SEALED: &[&str] = &[
        "01786e4e502848eb349086bfe587de9d90c716d3c06553f2d328b1688131977e",
        "0ca8047b938373cef21a9d29d3c990c3edcce1c795e31d2dd311d44dc99e9d2a",
        "64bbaeb94b87e75c5c83b4761f9594c8391896d8b1eb7d5f86d163ff71b8f969",
        "289995943509dd0c642c355607d28c2c739f137b8cf4dbf861b8717a83ae9fa7",
        "a0b21ba245cc25f008cb2bc42d6abfe84ac9ac615113f06d2452474885271491",
        "363ed2b45b18a29c7a95b9ec294461d2299504319864513028f2d8093958b2c9",
        "4232dcec98ac2ef4c7324bb9f47722f5e8dadfe51193b4d27d483d4f4cedcf68",
        "55e9dfa13aee9b126aa160cf76b101012f1b3923ff34b53d1d136b436110c58c",
        "14bcdf81f7f55f9bbf711c40f8100f9bf2501fc59ba2f2fdd24ef7c3f291057f",
        "4b2cb8564af3e22e85",
    ];

    fn unhex(parts: &[&str]) -> Vec<u8> {
        let hex = parts.concat();
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Produces 0, 1, 2, … so the nonce is predictable.
    struct CountingRng(u8);

    impl rand_core::RngCore for CountingRng {
        fn next_u32(&mut self) -> u32 {
            rand_core::impls::next_u32_via_fill(self)
        }
        fn next_u64(&mut self) -> u64 {
            rand_core::impls::next_u64_via_fill(self)
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for b in dest {
                *b = self.0;
                self.0 = self.0.wrapping_add(1);
            }
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    impl rand_core::CryptoRng for CountingRng {}

    #[test]
    fn ordinary_form_is_byte_identical_to_libsodium() {
        let a = ContactKey::from_bytes([0x0a; 32]);
        let b = ContactKey::from_bytes([0x0b; 32]);
        let expected = unhex(LIBSODIUM_ORDINARY);
        let envelope = seal(&request(), &a, &b.public_key(), &mut CountingRng(0)).unwrap();
        assert_eq!(envelope.as_slice(), expected.as_slice());
        assert_eq!(open(&expected, &b, &a.public_key()).unwrap(), request());
    }

    #[test]
    fn sealed_form_opens_libsodium_output() {
        let b = ContactKey::from_bytes([0x0b; 32]);
        let envelope = unhex(LIBSODIUM_SEALED);
        assert_eq!(open_anonymous(&envelope, &b).unwrap(), request());
    }
}
