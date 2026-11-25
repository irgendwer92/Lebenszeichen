//! Adding a device: device code, challenge, revealed value, handover.
//!
//! ```text
//! N (new device)                               E (existing device)
//! NewDevice::create          ──device code──▶  Adding::start
//!                            ◀─challenge slot─  c || t
//! NewDevice::take_challenge  ──reveal slot───▶  Adding::check_reveal
//! both show the device check code; the person confirms on E
//!                            ◀─handover slot──  Adding::seal
//! NewDevice::open_handover
//! both show the handover check code; the person confirms on N
//! NewDevice::install, then Device::announce_device
//! ```
//!
//! Nothing here talks to a relay. Creating the group, registering readers
//! and storing or fetching slots is up to the caller.

use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::crypto::{self, GroupKey, HandoverKey, sha256, six_digits};
use crate::store::{Device, FORMAT_VERSION, Group, KnownDevice, RelayAccount, StoredContact};
use crate::wire::{
    DEVICE_CODE_TTL_SECS, DeviceCode, Hash, MAX_HANDOVER_LEN, PublicKey, ReaderId, RelayUrl, Token,
};

/// Version of the handover content this device writes and reads.
pub const HANDOVER_VERSION: u32 = 1;

/// `SHA-256(token)`, as the relay stores tokens.
pub fn token_hash(token: &Token) -> Hash {
    sha256(&[token])
}

/// `SHA-256("lebenszeichen-v1-commit" || n)`.
pub fn commitment(n: &[u8; 32]) -> Hash {
    sha256(&[b"lebenszeichen-v1-commit", n])
}

/// Slot of the challenge from E.
pub fn challenge_slot(handover_key: &PublicKey) -> Hash {
    sha256(&[b"lebenszeichen-v1-challenge", handover_key])
}

/// Slot the relay reserves for the reply to a slot stored with a reply
/// hash.
pub fn reveal_slot(slot: &Hash) -> Hash {
    sha256(&[b"lebenszeichen-v1-reveal", slot])
}

/// Slot of the sealed handover.
pub fn handover_slot(handover_key: &PublicKey) -> Hash {
    sha256(&[b"lebenszeichen-v1-handover", handover_key])
}

/// Six digits both devices show once E has checked `n`.
pub fn device_check_code(handover_key: &PublicKey, n: &[u8; 32], challenge: &[u8; 32]) -> String {
    six_digits(&sha256(&[
        b"lebenszeichen-v1-device",
        handover_key,
        n,
        challenge,
    ]))
}

/// Six digits both devices show once N has opened the handover.
pub fn handover_check_code(group_key: &GroupKey, handover_key: &PublicKey) -> String {
    six_digits(&sha256(&[
        b"lebenszeichen-v1-handover-check",
        &group_key.to_bytes(),
        handover_key,
    ]))
}

/// Content of the challenge slot: challenge `c` and reply token `t`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    #[serde(with = "crate::hex")]
    pub challenge: [u8; 32],
    #[serde(with = "crate::hex")]
    pub reply_token: Token,
}

impl Challenge {
    pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
        let mut challenge = [0; 32];
        let mut reply_token = [0; 32];
        rng.fill_bytes(&mut challenge);
        rng.fill_bytes(&mut reply_token);
        Self {
            challenge,
            reply_token,
        }
    }

    /// `c || t`, as stored in the challenge slot.
    pub fn to_bytes(&self) -> [u8; 64] {
        let mut out = [0; 64];
        out[..32].copy_from_slice(&self.challenge);
        out[32..].copy_from_slice(&self.reply_token);
        out
    }

    /// Reads the challenge slot; anything but exactly 64 bytes fails.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let bytes: &[u8; 64] = bytes.try_into().map_err(|_| Error::Unreadable)?;
        let (challenge, reply_token) = bytes.split_at(32);
        Ok(Self {
            challenge: challenge.try_into().expect("32 bytes"),
            reply_token: reply_token.try_into().expect("32 bytes"),
        })
    }
}

/// The new device while it is being added.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewDevice {
    pub key: HandoverKey,
    #[serde(with = "crate::hex")]
    pub fetch_token: Token,
    /// The secret value `n` behind the commitment.
    #[serde(with = "crate::hex")]
    pub secret: [u8; 32],
    #[serde(with = "crate::hex")]
    pub reader: ReaderId,
    pub expires_at: u64,
    pub relay: RelayUrl,
    /// The challenge once fetched. A second one is never accepted.
    #[serde(with = "crate::hex::option")]
    pub challenge: Option<[u8; 32]>,
}

impl NewDevice {
    /// Starts adding with fresh keys; `relay` is where E stores the slots.
    pub fn create(relay: RelayUrl, now: u64, rng: &mut impl CryptoRngCore) -> Self {
        let mut fetch_token = [0; 32];
        let mut secret = [0; 32];
        let mut reader = [0; 16];
        rng.fill_bytes(&mut fetch_token);
        rng.fill_bytes(&mut secret);
        rng.fill_bytes(&mut reader);
        Self {
            key: HandoverKey::generate(rng),
            fetch_token,
            secret,
            reader,
            expires_at: now + DEVICE_CODE_TTL_SECS,
            relay,
            challenge: None,
        }
    }

    /// The code to show as QR code and text.
    pub fn code(&self) -> DeviceCode {
        DeviceCode {
            expires_at: self.expires_at,
            handover_key: self.key.public_key(),
            reader: self.reader,
            fetch_hash: token_hash(&self.fetch_token),
            commitment: commitment(&self.secret),
            relay: self.relay,
        }
    }

    pub fn challenge_slot(&self) -> Hash {
        challenge_slot(&self.key.public_key())
    }

    pub fn reveal_slot(&self) -> Hash {
        reveal_slot(&self.challenge_slot())
    }

    pub fn handover_slot(&self) -> Hash {
        handover_slot(&self.key.public_key())
    }

    /// Reads the fetched challenge slot. Returns the reply token, with which
    /// the caller stores [`Self::secret`] in [`Self::reveal_slot`].
    ///
    /// Fails with [`Error::Unexpected`] if a challenge was taken before; the
    /// caller then starts over with a new [`NewDevice`].
    pub fn take_challenge(&mut self, slot: &[u8], now: u64) -> Result<Token, Error> {
        if self.challenge.is_some() {
            return Err(Error::Unexpected);
        }
        if now > self.expires_at {
            return Err(Error::Expired);
        }
        let challenge = Challenge::from_bytes(slot)?;
        self.challenge = Some(challenge.challenge);
        Ok(challenge.reply_token)
    }

    /// The device check code, once the challenge is known.
    pub fn check_code(&self) -> Option<String> {
        let challenge = self.challenge.as_ref()?;
        Some(device_check_code(
            &self.key.public_key(),
            &self.secret,
            challenge,
        ))
    }

    /// Opens the fetched handover slot.
    ///
    /// Fails with [`Error::Mismatch`] if it does not carry the challenge
    /// this device took: then it shows an alarm and discards it.
    pub fn open_handover(&self, sealed: &[u8]) -> Result<Handover, Error> {
        let challenge = self.challenge.ok_or(Error::Unexpected)?;
        let handover = Handover::from_json(&self.key.open(sealed)?)?;
        if handover.challenge != challenge {
            return Err(Error::Mismatch);
        }
        Ok(handover)
    }

    /// The handover check code for an opened handover.
    pub fn handover_check_code(&self, handover: &Handover) -> String {
        handover_check_code(&handover.group.key, &self.key.public_key())
    }

    /// After the person confirmed: the state of this device. The caller
    /// then registers its push address and sends
    /// [`Device::announce_device`].
    pub fn install(&self, handover: Handover, name: String) -> Device {
        let mut device = Device::from_handover(handover, self.reader);
        device.name = Some(name);
        device
    }
}

/// The existing device while it adds a new one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Adding {
    #[serde(with = "device_code")]
    pub code: DeviceCode,
    pub challenge: Challenge,
    /// `n`, once revealed and checked.
    #[serde(with = "crate::hex::option")]
    pub revealed: Option<[u8; 32]>,
}

impl Adding {
    /// Starts adding the device that shows `code`.
    ///
    /// Refuses expired codes. The caller also refuses relays on which this
    /// device has neither its relay account nor a mailbox, see
    /// [`Device::knows_relay`].
    pub fn start(code: DeviceCode, now: u64, rng: &mut impl CryptoRngCore) -> Result<Self, Error> {
        if now > code.expires_at {
            return Err(Error::Expired);
        }
        Ok(Self {
            code,
            challenge: Challenge::generate(rng),
            revealed: None,
        })
    }

    pub fn challenge_slot(&self) -> Hash {
        challenge_slot(&self.code.handover_key)
    }

    /// Hash of the reply token, stored with the challenge slot.
    pub fn reply_hash(&self) -> Hash {
        token_hash(&self.challenge.reply_token)
    }

    pub fn reveal_slot(&self) -> Hash {
        reveal_slot(&self.challenge_slot())
    }

    pub fn handover_slot(&self) -> Hash {
        handover_slot(&self.code.handover_key)
    }

    /// Checks the fetched reveal slot against the commitment and returns the
    /// device check code. [`Error::Mismatch`] means an alarm.
    pub fn check_reveal(&mut self, revealed: &[u8]) -> Result<String, Error> {
        let n: [u8; 32] = revealed.try_into().map_err(|_| Error::Mismatch)?;
        if commitment(&n) != self.code.commitment {
            return Err(Error::Mismatch);
        }
        self.revealed = Some(n);
        Ok(device_check_code(
            &self.code.handover_key,
            &n,
            &self.challenge.challenge,
        ))
    }

    /// After the person confirmed the device check code, and after the new
    /// reader was registered on all mailboxes: the sealed handover for the
    /// handover slot, and the handover check code.
    pub fn seal(
        &self,
        device: &Device,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(Vec<u8>, String), Error> {
        if self.revealed.is_none() {
            return Err(Error::Unexpected);
        }
        let handover = device.handover(self.challenge.challenge)?;
        let sealed = crypto::seal_bytes(&handover.to_json(), &self.code.handover_key, rng)?;
        if sealed.len() > MAX_HANDOVER_LEN {
            return Err(Error::Wire(crate::wire::Error::TooLong));
        }
        let code = handover_check_code(&handover.group.key, &self.code.handover_key);
        Ok((sealed, code))
    }
}

/// [`DeviceCode`] in stored JSON, as its text form.
mod device_code {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    use crate::wire::DeviceCode;
    use crate::wire::device_code::MAX_TEXT_LEN;

    pub fn serialize<S: Serializer>(code: &DeviceCode, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(code.to_text(&mut [0; MAX_TEXT_LEN]))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<DeviceCode, D::Error> {
        DeviceCode::from_text(<&str>::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Content of the handover: the state of E without what is specific to it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Handover {
    pub version: u32,
    #[serde(with = "crate::hex")]
    pub challenge: [u8; 32],
    /// `null` if E has no relay account.
    pub relay: Option<RelayAccount>,
    pub group: Group,
    /// All devices of the person except the new one, E included.
    pub devices: Vec<KnownDevice>,
    /// In the form of the state file; open invitations and pairings that
    /// are not complete stay on E.
    pub contacts: Vec<StoredContact>,
}

impl Handover {
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serializable")
    }

    /// Reads the content; refuses versions other than [`HANDOVER_VERSION`].
    pub fn from_json(bytes: &[u8]) -> Result<Self, Error> {
        #[derive(Deserialize)]
        struct Version {
            version: u32,
        }
        let Version { version } = serde_json::from_slice(bytes).map_err(|_| Error::Unreadable)?;
        if version != HANDOVER_VERSION {
            return Err(Error::UnknownVersion(version));
        }
        serde_json::from_slice(bytes).map_err(|_| Error::Unreadable)
    }
}

impl Device {
    /// The handover for a new device. Needs the group, which the caller
    /// creates before if there is none yet.
    ///
    /// Own tokens of this device are never handed over (M10): each
    /// `read_token` goes as it is, and the caller has set it to the
    /// enrolment token it registered for the new device where the relay
    /// has `reader_tokens`.
    pub fn handover(&self, challenge: [u8; 32]) -> Result<Handover, Error> {
        let mut group = self.group.clone().ok_or(Error::Unexpected)?;
        group.own_token = None;
        group.new_token = None;
        let mut devices = self.devices.clone();
        devices.push(KnownDevice {
            reader: self.reader_id,
            name: self.name.clone().unwrap_or_default(),
        });
        Ok(Handover {
            version: HANDOVER_VERSION,
            challenge,
            relay: self.relay.clone(),
            group,
            devices,
            // Only the device that paired holds the secret value of a
            // pairing; the others learn of the contact once it is complete.
            contacts: self
                .contacts
                .iter()
                .filter(|c| c.state.is_paired())
                .map(|c| StoredContact {
                    own_token: None,
                    new_token: None,
                    ..c.clone()
                })
                .collect(),
        })
    }

    /// The state of a new device from a handover, with its own reader
    /// identifier.
    pub fn from_handover(handover: Handover, reader_id: ReaderId) -> Self {
        Self {
            format: FORMAT_VERSION,
            reader_id,
            relay: handover.relay,
            contacts: handover.contacts,
            invitations: Vec::new(),
            push_url: None,
            push_pending: false,
            name: None,
            group: Some(handover.group),
            devices: handover
                .devices
                .into_iter()
                .filter(|d| d.reader != reader_id)
                .collect(),
            outbox: Vec::new(),
            unsent: Vec::new(),
            transfers: Vec::new(),
            parked: Vec::new(),
            reports: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use rand_core::OsRng;

    use super::{
        Adding, Challenge, Handover, NewDevice, challenge_slot, commitment, device_check_code,
        handover_check_code, handover_slot, reveal_slot,
    };
    use crate::Error;
    use crate::contact::Event;
    use crate::crypto::{self, GroupKey};
    use crate::group::tests::{T, device_with_group, pair};
    use crate::hex;
    use crate::wire::{DEVICE_CODE_TTL_SECS, DeviceCode, RelayUrl, device_code};

    #[test]
    fn derivations_match_documented_formulas() {
        // Computed with Python's hashlib from the formulas in wire-format.md.
        let (n, pk, c) = ([1; 32], [2; 32], [4; 32]);
        let hex = |h: [u8; 32]| hex::encode(&h);
        assert_eq!(
            hex(commitment(&n)),
            "ce5b56070ca69781c9669ba8ae64a9823b3177239c631f9482340e715442aaa2"
        );
        let slot = challenge_slot(&pk);
        assert_eq!(
            hex(slot),
            "3a0fda1cfd69ecbe888ee887b80066538029bed49f275e3fba5ee78934464746"
        );
        assert_eq!(
            hex(reveal_slot(&slot)),
            "c4ce7e7ff83e261fcf9b6eb7d717eb4d712a89ab3bd5d4e9dd13cb7b26be69b1"
        );
        assert_eq!(
            hex(handover_slot(&pk)),
            "2ccf7d331fa885ec7daeebfa1789a369d2727222a0351392828d6b9bfb46a86c"
        );
        assert_eq!(device_check_code(&pk, &n, &c), "294996");
        assert_eq!(
            handover_check_code(&GroupKey::from_bytes([5; 32]), &pk),
            "336234"
        );
    }

    /// E with a group and one contact, and the other person.
    fn existing() -> (crate::store::Device, crate::contact::ContactState) {
        let mut e = device_with_group();
        let (stored, peer) = pair(1, "Anna");
        e.contacts.push(stored);
        (e, peer)
    }

    #[test]
    fn full_adding_of_a_device() {
        let (e, mut peer) = existing();
        let relay = RelayUrl::new("https://relay.example").unwrap();
        let mut n = NewDevice::create(relay, T, &mut OsRng);
        let mut text = [0; device_code::MAX_TEXT_LEN];
        let code = DeviceCode::from_text(n.code().to_text(&mut text)).unwrap();
        assert!(e.knows_relay(&code.relay));

        let mut adding = Adding::start(code, T + 10, &mut OsRng).unwrap();
        assert_eq!(adding.challenge_slot(), n.challenge_slot());
        assert_eq!(adding.reveal_slot(), n.reveal_slot());
        assert_eq!(adding.handover_slot(), n.handover_slot());

        let reply = n
            .take_challenge(&adding.challenge.to_bytes(), T + 20)
            .unwrap();
        assert_eq!(super::token_hash(&reply), adding.reply_hash());
        let e_code = adding.check_reveal(&n.secret).unwrap();
        assert_eq!(Some(e_code), n.check_code());

        let (sealed, e_check) = adding.seal(&e, &mut OsRng).unwrap();
        let handover = n.open_handover(&sealed).unwrap();
        assert_eq!(n.handover_check_code(&handover), e_check);

        let new = n.install(handover, "Phone".into());
        assert_eq!(new.reader_id, n.reader);
        assert_eq!(new.name.as_deref(), Some("Phone"));
        assert_eq!(new.devices.len(), 1);
        assert_eq!(new.devices[0].reader, e.reader_id);
        assert_eq!(new.devices[0].name, "Laptop");
        assert_eq!(
            new.group.as_ref().unwrap().key,
            e.group.as_ref().unwrap().key
        );
        assert!(new.invitations.is_empty());

        // The new device reads the contact like the existing one.
        let (id, request) = peer.request(T + 60, &mut OsRng).unwrap();
        let mut state = new.contacts[0].state.clone();
        assert_eq!(state.receive(&request, T + 60).unwrap(), Event::Request(id));
    }

    #[test]
    fn own_tokens_are_not_handed_over() {
        let (mut e, _) = existing();
        let group = e.group.as_mut().unwrap();
        group.own_token = Some([7; 32]);
        group.new_token = Some([8; 32]);
        e.contacts[0].own_token = Some([9; 32]);
        e.contacts[0].new_token = Some([10; 32]);
        let json = e.handover([3; 32]).unwrap().to_json();
        let text = String::from_utf8(json).unwrap();
        for secret in [7u8, 8, 9, 10] {
            assert!(!text.contains(&hex::encode(&[secret; 32])), "{text}");
        }
        assert!(!text.contains("own_token") && !text.contains("new_token"));
    }

    #[test]
    fn handover_json_has_the_documented_shape() {
        let (e, _) = existing();
        let json: serde_json::Value =
            serde_json::from_slice(&e.handover([3; 32]).unwrap().to_json()).unwrap();
        let keys = |v: &serde_json::Value| -> Vec<String> {
            let mut k: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        assert_eq!(
            keys(&json),
            [
                "challenge",
                "contacts",
                "devices",
                "group",
                "relay",
                "version"
            ]
        );
        assert_eq!(json["version"], 1);
        assert_eq!(json["challenge"], hex::encode(&[3; 32]));
        assert_eq!(keys(&json["relay"]), ["access_token", "url"]);
        assert_eq!(
            keys(&json["group"]),
            [
                "key",
                "mailbox",
                "read_token",
                "relay",
                "seen",
                "send_token"
            ]
        );
        assert_eq!(keys(&json["devices"][0]), ["name", "reader"]);
        // Contacts in their stored form.
        let stored = serde_json::to_value(&e.contacts).unwrap();
        assert_eq!(json["contacts"], stored);
    }

    #[test]
    fn pairings_in_progress_are_not_handed_over() {
        let (mut e, _) = existing();
        let address = |fill| crate::pairing::Address {
            relay: RelayUrl::new("https://relay.example").unwrap(),
            mailbox: [fill; 32],
            send_token: [fill; 32],
        };
        let (pending, invitation) =
            crate::pairing::PendingInvitation::create(address(20), None, T, &mut OsRng);
        let (_, hello) = crate::pairing::accept(&invitation, address(21), T, &mut OsRng).unwrap();
        let mut waiting = e.contacts[0].clone();
        waiting.name = "Bert".into();
        waiting.state =
            crate::contact::ContactState::new(pending.receive_hello(&hello, T).unwrap());
        assert!(waiting.state.contact.secret.is_some());
        e.contacts.push(waiting);
        let handover = e.handover([3; 32]).unwrap();
        assert_eq!(handover.contacts.len(), 1);
        assert_eq!(handover.contacts[0].name, "Anna");
        assert!(
            !String::from_utf8(handover.to_json())
                .unwrap()
                .contains("secret")
        );
    }

    #[test]
    fn wrong_reveal_is_an_alarm() {
        let n = NewDevice::create(RelayUrl::new("https://r.example").unwrap(), T, &mut OsRng);
        let mut adding = Adding::start(n.code(), T, &mut OsRng).unwrap();
        let (e, _) = existing();
        assert_eq!(adding.seal(&e, &mut OsRng).unwrap_err(), Error::Unexpected);
        assert_eq!(adding.check_reveal(&[0; 32]), Err(Error::Mismatch));
        assert_eq!(adding.check_reveal(&n.secret[..31]), Err(Error::Mismatch));
        assert!(adding.check_reveal(&n.secret).is_ok());
    }

    #[test]
    fn expired_code_and_second_challenge_are_refused() {
        let mut n = NewDevice::create(RelayUrl::new("https://r.example").unwrap(), T, &mut OsRng);
        let late = T + DEVICE_CODE_TTL_SECS + 1;
        assert_eq!(
            Adding::start(n.code(), late, &mut OsRng).unwrap_err(),
            Error::Expired
        );
        let challenge = Challenge::generate(&mut OsRng);
        assert_eq!(
            n.clone().take_challenge(&challenge.to_bytes(), late),
            Err(Error::Expired)
        );
        assert_eq!(
            n.take_challenge(&challenge.to_bytes()[..63], T),
            Err(Error::Unreadable)
        );
        n.take_challenge(&challenge.to_bytes(), T).unwrap();
        let other = Challenge::generate(&mut OsRng);
        assert_eq!(
            n.take_challenge(&other.to_bytes(), T),
            Err(Error::Unexpected)
        );
    }

    #[test]
    fn planted_or_foreign_handover_is_refused() {
        let (e, _) = existing();
        let mut n = NewDevice::create(RelayUrl::new("https://r.example").unwrap(), T, &mut OsRng);
        assert_eq!(n.open_handover(&[]).unwrap_err(), Error::Unexpected);
        let challenge = Challenge::generate(&mut OsRng);
        n.take_challenge(&challenge.to_bytes(), T).unwrap();
        let public = n.key.public_key();

        // Someone who knows only the device code seals a handover with
        // another challenge.
        let planted = e.handover([0; 32]).unwrap().to_json();
        let sealed = crypto::seal_bytes(&planted, &public, &mut OsRng).unwrap();
        assert_eq!(n.open_handover(&sealed).unwrap_err(), Error::Mismatch);

        let mut future = e.handover(challenge.challenge).unwrap();
        future.version = 2;
        let sealed = crypto::seal_bytes(&future.to_json(), &public, &mut OsRng).unwrap();
        assert_eq!(
            n.open_handover(&sealed).unwrap_err(),
            Error::UnknownVersion(2)
        );
        let sealed = crypto::seal_bytes(b"{\"version\":1}", &public, &mut OsRng).unwrap();
        assert_eq!(n.open_handover(&sealed).unwrap_err(), Error::Unreadable);
        let sealed = crypto::seal_bytes(&planted, &[9; 32], &mut OsRng).unwrap();
        assert_eq!(n.open_handover(&sealed).unwrap_err(), Error::Decrypt);
        assert_eq!(Handover::from_json(b"[]").unwrap_err(), Error::Unreadable);
    }

    #[test]
    fn handover_needs_a_group() {
        let mut e = device_with_group();
        e.group = None;
        assert_eq!(e.handover([0; 32]).unwrap_err(), Error::Unexpected);
    }

    #[test]
    fn adding_state_survives_storage() {
        let n = NewDevice::create(RelayUrl::new("https://r.example").unwrap(), T, &mut OsRng);
        let stored: NewDevice = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(stored.code(), n.code());
        let adding = Adding::start(n.code(), T, &mut OsRng).unwrap();
        let stored: Adding =
            serde_json::from_str(&serde_json::to_string(&adding).unwrap()).unwrap();
        assert_eq!(stored.code, adding.code);
        assert_eq!(stored.challenge, adding.challenge);
    }
}
