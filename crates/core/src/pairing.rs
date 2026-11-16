//! Pairing of two people: invitation, `hello`, `reveal`, verification code,
//! `hello_ok`.
//!
//! Nothing here talks to a relay or a disk. Creating mailboxes, delivering
//! envelopes and storing the results is up to the caller.
//!
//! ```text
//! B (invites)                                   A (accepts)
//! create mailbox, PendingInvitation::create
//!   (secret n, commitment in the code)  ──code──▶  Invitation::from_text
//!                                                create own mailbox
//!                                                accept, store, then send
//!                                     ◀─hello──  (AwaitingReveal, no code)
//! PendingInvitation::receive_hello
//! store contact with n, Contact::reveal
//!   (AwaitingConfirmation)            ─reveal──▶ Contact::receive_reveal
//!                                                n matches the commitment
//!                                                (AwaitingHelloOk)
//! both compare the verification code
//! Contact::confirm, n deleted         ─hello_ok▶ Contact::receive_hello_ok
//! ```
//!
//! The accepting side shows no code and never pairs before a `reveal` that
//! matches the commitment of the invitation; a `hello_ok` that comes first is
//! kept until then.

use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::crypto::{self, ContactKey};
use crate::wire::{
    Body, ENVELOPE_LEN, Hash, Hello, INVITATION_TTL_SECS, Invitation, MAX_CLOCK_AHEAD_SECS,
    MAX_MESSAGE_AGE_SECS, MailboxId, Message, PublicKey, RelayUrl, Token,
};

/// Shortest time between two sendings of the same `hello` while waiting for
/// `reveal`. The time doubles with every repetition.
pub const HELLO_REPEAT_SECS: u64 = 5 * 60;

/// A mailbox on a relay and the token to deliver into it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    pub relay: RelayUrl,
    #[serde(with = "crate::hex")]
    pub mailbox: MailboxId,
    #[serde(with = "crate::hex")]
    pub send_token: Token,
}

/// Where a contact stands in the pairing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingState {
    /// Accepted an invitation and sent `hello`; waiting for `reveal`. No
    /// code can be shown yet.
    AwaitingReveal,
    /// Accepted an invitation and received a valid `reveal`; the code is
    /// shown, waiting for `hello_ok`.
    AwaitingHelloOk,
    /// Received `hello` and sent `reveal`; waiting for the local person to
    /// compare the code.
    AwaitingConfirmation,
    /// Both sides are paired.
    Paired,
}

/// The cryptographic side of a contact. Names never leave the device and are
/// not part of this.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Contact {
    pub key: ContactKey,
    #[serde(with = "crate::hex")]
    pub peer_key: PublicKey,
    /// Mailbox of the other side, into which this side delivers.
    pub peer: Address,
    /// Own mailbox, from which this side reads messages of this contact.
    #[serde(with = "crate::hex")]
    pub own_mailbox: MailboxId,
    pub state: PairingState,
    /// The inviting side's secret value `n`, needed for the code. The
    /// inviting side keeps it from the invitation until the person confirms
    /// or rejects; the accepting side from a valid `reveal` until paired.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::hex::option"
    )]
    pub secret: Option<[u8; 32]>,
    /// What the accepting side keeps until a valid `reveal` arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_reveal: Option<RevealWait>,
}

/// State of the accepting side between `hello` and `reveal`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevealWait {
    /// Commitment from the invitation.
    #[serde(with = "crate::hex")]
    pub commitment: Hash,
    /// The `hello` as sent, to send it again unchanged if `reveal` got lost.
    #[serde(with = "crate::hex")]
    pub hello: [u8; ENVELOPE_LEN],
    /// Sending time of `hello`. Without `reveal` the pairing fails
    /// [`MAX_MESSAGE_AGE_SECS`] after it.
    pub sent_at: u64,
    /// When `hello` was last sent, the first time or again.
    pub resent_at: u64,
    /// `hello_ok` arrived before `reveal`; applied once `reveal` is valid.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hello_ok: bool,
}

/// What a `reveal` meant for the accepting side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Revealed {
    /// The first valid `reveal`: the code to show, and whether a `hello_ok`
    /// kept back has completed the pairing with it.
    Code { code: String, paired: bool },
    /// The same value again, for example after `hello` was repeated.
    Again,
}

/// An invitation handed out and not yet accepted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingInvitation {
    pub key: ContactKey,
    pub own: Address,
    pub expires_at: u64,
    /// The secret value `n` behind the commitment in the invitation.
    /// `None` for an invitation of version 1, which is no longer accepted
    /// and gets deleted.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::hex::option"
    )]
    pub secret: Option<[u8; 32]>,
}

impl PendingInvitation {
    /// Starts an invitation for a freshly created mailbox.
    ///
    /// `own.send_token` goes to the invitee; the read token stays with the
    /// caller. A voucher lets the invitee create its mailbox on this relay.
    pub fn create(
        own: Address,
        voucher: Option<Token>,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> (Self, Invitation) {
        let key = ContactKey::generate(rng);
        let mut secret = [0; 32];
        rng.fill_bytes(&mut secret);
        let expires_at = now + INVITATION_TTL_SECS;
        let invitation = Invitation {
            expires_at,
            mailbox: own.mailbox,
            send_token: own.send_token,
            public_key: key.public_key(),
            commitment: crypto::invite_commitment(&secret),
            voucher,
            relay: own.relay,
        };
        let pending = Self {
            key,
            own,
            expires_at,
            secret: Some(secret),
        };
        (pending, invitation)
    }

    /// An invitation of version 1, made before the commitment existed. It
    /// cannot lead to a pairing anymore; the caller deletes it and its
    /// mailbox.
    pub fn is_outdated(&self) -> bool {
        self.secret.is_none()
    }

    /// After this time no valid `hello` can arrive anymore; the invitation and
    /// its mailbox can be dropped.
    pub fn keep_until(&self) -> u64 {
        self.expires_at.saturating_add(MAX_MESSAGE_AGE_SECS)
    }

    /// Reads an envelope that arrived in the mailbox of this invitation.
    ///
    /// Returns the new contact, waiting for confirmation and holding the
    /// secret value. The caller must store it in place of the pending
    /// invitation, so that only one `hello` is accepted, and in the same
    /// step make [`Contact::reveal`], which it delivers afterwards. Anything
    /// else, including a `hello` sent after the expiry, fails, uses nothing
    /// up and should be dropped silently.
    ///
    /// What counts is when `hello` was sent, not when it is read, so the
    /// inviting side may fetch it until [`Self::keep_until`].
    pub fn receive_hello(&self, envelope: &[u8], now: u64) -> Result<Contact, Error> {
        let secret = self.secret.ok_or(Error::Unexpected)?;
        let message = crypto::open_anonymous(envelope, &self.key)?;
        check_time(&message, now)?;
        if message.sent_at > self.expires_at {
            return Err(Error::Expired);
        }
        let Body::Hello(hello) = message.body else {
            return Err(Error::Unexpected);
        };
        Ok(Contact {
            key: self.key.clone(),
            peer_key: hello.public_key,
            peer: Address {
                relay: hello.relay,
                mailbox: hello.mailbox,
                send_token: hello.send_token,
            },
            own_mailbox: self.own.mailbox,
            state: PairingState::AwaitingConfirmation,
            secret: Some(secret),
            awaiting_reveal: None,
        })
    }
}

/// Accepts an invitation; `own` is the mailbox just created for this contact.
///
/// Returns the new contact and the `hello` to deliver into the mailbox of the
/// inviting side. The contact key is new on every call. The caller stores the
/// contact before it delivers `hello`.
pub fn accept(
    invitation: &Invitation,
    own: Address,
    now: u64,
    rng: &mut impl CryptoRngCore,
) -> Result<(Contact, [u8; ENVELOPE_LEN]), Error> {
    if now > invitation.expires_at {
        return Err(Error::Expired);
    }
    let key = ContactKey::generate(rng);
    let hello = new_message(
        Body::Hello(Hello {
            public_key: key.public_key(),
            mailbox: own.mailbox,
            send_token: own.send_token,
            relay: own.relay,
        }),
        now,
        rng,
    );
    let envelope = crypto::seal_anonymous(&hello, &invitation.public_key, rng)?;
    let contact = Contact {
        key,
        peer_key: invitation.public_key,
        peer: Address {
            relay: invitation.relay,
            mailbox: invitation.mailbox,
            send_token: invitation.send_token,
        },
        own_mailbox: own.mailbox,
        state: PairingState::AwaitingReveal,
        secret: None,
        awaiting_reveal: Some(RevealWait {
            commitment: invitation.commitment,
            hello: envelope,
            sent_at: now,
            resent_at: now,
            hello_ok: false,
        }),
    };
    Ok((contact, envelope))
}

impl Contact {
    /// Six digits for both people to compare, once both public keys and the
    /// secret value are known: on the inviting side until it confirms, on
    /// the accepting side between a valid `reveal` and `hello_ok`.
    pub fn verification_code(&self) -> Option<String> {
        let secret = self.secret.as_ref()?;
        let own = self.key.public_key();
        match self.state {
            PairingState::AwaitingConfirmation => {
                Some(crypto::verification_code(&own, &self.peer_key, secret))
            }
            PairingState::AwaitingHelloOk => {
                Some(crypto::verification_code(&self.peer_key, &own, secret))
            }
            PairingState::AwaitingReveal | PairingState::Paired => None,
        }
    }

    /// `reveal` for the accepting side, while the inviting side waits for
    /// the person to confirm. Made again for every `hello` with the key of
    /// this contact.
    pub fn reveal(
        &self,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<[u8; ENVELOPE_LEN], Error> {
        match (self.state, self.secret) {
            (PairingState::AwaitingConfirmation, Some(secret)) => {
                self.seal(Body::Reveal(secret), now, rng)
            }
            _ => Err(Error::Unexpected),
        }
    }

    /// The inviting person confirmed the code; returns `hello_ok` to send.
    /// The secret value is no longer needed and is deleted.
    pub fn confirm(
        &mut self,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<[u8; ENVELOPE_LEN], Error> {
        if self.state != PairingState::AwaitingConfirmation || self.secret.is_none() {
            return Err(Error::Unexpected);
        }
        let envelope = self.seal(Body::HelloOk, now, rng)?;
        self.state = PairingState::Paired;
        self.forget_pairing();
        Ok(envelope)
    }

    /// Handles `reveal` on the accepting side.
    ///
    /// Fails with [`Error::Mismatch`] if the value does not match the
    /// commitment of the invitation, or differs from the value accepted
    /// before: show an alarm, end the contact and delete the own mailbox.
    pub fn receive_reveal(&mut self, secret: &[u8; 32]) -> Result<Revealed, Error> {
        match self.state {
            PairingState::AwaitingReveal => {
                let wait = self.awaiting_reveal.as_ref().ok_or(Error::Unexpected)?;
                if crypto::invite_commitment(secret) != wait.commitment {
                    return Err(Error::Mismatch);
                }
                let hello_ok = wait.hello_ok;
                self.awaiting_reveal = None;
                self.secret = Some(*secret);
                self.state = PairingState::AwaitingHelloOk;
                let code = self.verification_code().ok_or(Error::Unexpected)?;
                if hello_ok {
                    self.state = PairingState::Paired;
                    self.forget_pairing();
                }
                Ok(Revealed::Code {
                    code,
                    paired: hello_ok,
                })
            }
            PairingState::AwaitingHelloOk if self.secret.as_ref() == Some(secret) => {
                Ok(Revealed::Again)
            }
            PairingState::AwaitingHelloOk => Err(Error::Mismatch),
            PairingState::AwaitingConfirmation | PairingState::Paired => Err(Error::Unexpected),
        }
    }

    /// Handles `hello_ok` on the accepting side. Returns `true` if the
    /// pairing is complete, `false` if it is kept until a valid `reveal`.
    pub fn receive_hello_ok(&mut self, message: &Message) -> Result<bool, Error> {
        if message.body != Body::HelloOk {
            return Err(Error::Unexpected);
        }
        match (self.state, self.awaiting_reveal.as_mut()) {
            (PairingState::AwaitingReveal, Some(wait)) => {
                wait.hello_ok = true;
                Ok(false)
            }
            (PairingState::AwaitingHelloOk, _) if self.secret.is_some() => {
                self.state = PairingState::Paired;
                self.forget_pairing();
                Ok(true)
            }
            _ => Err(Error::Unexpected),
        }
    }

    /// The `hello` to send again while waiting for `reveal`, if it is time:
    /// [`HELLO_REPEAT_SECS`] after the first sending, then after twice the
    /// time since it, and so on. Records the new sending.
    pub fn hello_to_repeat(&mut self, now: u64) -> Option<[u8; ENVELOPE_LEN]> {
        if self.state != PairingState::AwaitingReveal {
            return None;
        }
        let wait = self.awaiting_reveal.as_mut()?;
        let gap = HELLO_REPEAT_SECS.max(wait.resent_at.saturating_sub(wait.sent_at));
        if now < wait.resent_at.saturating_add(gap) {
            return None;
        }
        wait.resent_at = now;
        Some(wait.hello)
    }

    /// Whether this pairing can no longer complete: no valid `reveal`
    /// within [`MAX_MESSAGE_AGE_SECS`] after `hello`, or a pairing begun
    /// with an invitation of version 1, for which there is no code.
    pub fn pairing_failed(&self, now: u64) -> bool {
        match self.state {
            PairingState::AwaitingReveal => self
                .awaiting_reveal
                .as_ref()
                .is_none_or(|wait| wait.sent_at.saturating_add(MAX_MESSAGE_AGE_SECS) < now),
            PairingState::AwaitingHelloOk | PairingState::AwaitingConfirmation => {
                self.secret.is_none()
            }
            PairingState::Paired => false,
        }
    }

    /// Deletes what only the pairing needs: the secret value and the state
    /// of waiting for `reveal`.
    pub fn forget_pairing(&mut self) {
        self.secret = None;
        self.awaiting_reveal = None;
    }

    /// Encrypts a new message to this contact.
    pub fn seal(
        &self,
        body: Body,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<[u8; ENVELOPE_LEN], Error> {
        self.seal_message(&new_message(body, now, rng), rng)
    }

    /// Encrypts a prepared message to this contact.
    pub fn seal_message(
        &self,
        message: &Message,
        rng: &mut impl CryptoRngCore,
    ) -> Result<[u8; ENVELOPE_LEN], Error> {
        crypto::seal(message, &self.key, &self.peer_key, rng)
    }

    /// Decrypts an envelope from this contact and checks its time.
    ///
    /// Repeated identifiers are not detected here; that needs the stored
    /// history of the contact.
    pub fn open(&self, envelope: &[u8], now: u64) -> Result<Message, Error> {
        let message = crypto::open(envelope, &self.key, &self.peer_key)?;
        check_time(&message, now)?;
        Ok(message)
    }
}

/// A message with a fresh random identifier, sent at `now`.
pub(crate) fn new_message(body: Body, now: u64, rng: &mut impl CryptoRngCore) -> Message {
    let mut id = [0; 16];
    rng.fill_bytes(&mut id);
    Message {
        id,
        sent_at: now,
        body,
    }
}

/// Rejects messages outside the window against replay.
pub(crate) fn check_time(message: &Message, now: u64) -> Result<(), Error> {
    let too_old = message.sent_at.saturating_add(MAX_MESSAGE_AGE_SECS) < now;
    let too_new = message.sent_at > now.saturating_add(MAX_CLOCK_AHEAD_SECS);
    if too_old || too_new {
        return Err(Error::Expired);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use rand_core::{CryptoRng, OsRng, RngCore};

    use super::{Address, HELLO_REPEAT_SECS, PairingState, PendingInvitation, Revealed, accept};
    use crate::Error;
    use crate::contact::{ContactState, Event};
    use crate::crypto;
    use crate::wire::{
        Body, INVITATION_TTL_SECS, Invitation, MAX_CLOCK_AHEAD_SECS, MAX_MESSAGE_AGE_SECS, RelayUrl,
    };

    const NOW: u64 = 1_794_600_000;

    fn address(relay: &str, fill: u8) -> Address {
        Address {
            relay: RelayUrl::new(relay).unwrap(),
            mailbox: [fill; 32],
            send_token: [fill + 1; 32],
        }
    }

    /// Deterministic randomness for tests whose outcome must not depend on
    /// chance: SHA-256 over a seed and a counter.
    pub(crate) struct TestRng {
        seed: u8,
        counter: u64,
    }

    impl TestRng {
        pub(crate) fn new(seed: u8) -> Self {
            Self { seed, counter: 0 }
        }
    }

    impl RngCore for TestRng {
        fn next_u32(&mut self) -> u32 {
            let mut b = [0; 4];
            self.fill_bytes(&mut b);
            u32::from_be_bytes(b)
        }

        fn next_u64(&mut self) -> u64 {
            let mut b = [0; 8];
            self.fill_bytes(&mut b);
            u64::from_be_bytes(b)
        }

        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for chunk in dest.chunks_mut(32) {
                self.counter += 1;
                let block = crypto::sha256(&[&[self.seed], &self.counter.to_be_bytes()]);
                chunk.copy_from_slice(&block[..chunk.len()]);
            }
        }

        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    impl CryptoRng for TestRng {}

    #[test]
    fn full_pairing_through_text_code() {
        let (pending, invitation) =
            PendingInvitation::create(address("https://b.example", 1), None, NOW, &mut OsRng);
        let secret = pending.secret.unwrap();
        assert_eq!(invitation.commitment, crypto::invite_commitment(&secret));
        let mut buf = [0; crate::wire::invitation::MAX_TEXT_LEN];
        let code = invitation.to_text(&mut buf);
        assert!(code.starts_with("LZ2-"));
        let invitation = Invitation::from_text(code).unwrap();

        let (mut a, hello) = accept(
            &invitation,
            address("https://a.example", 3),
            NOW + 5,
            &mut OsRng,
        )
        .unwrap();
        assert_eq!(a.state, PairingState::AwaitingReveal);
        assert_eq!(a.peer, address("https://b.example", 1));
        // No code before `reveal`.
        assert_eq!(a.verification_code(), None);

        let mut b = pending.receive_hello(&hello, NOW + 10).unwrap();
        assert_eq!(b.state, PairingState::AwaitingConfirmation);
        assert_eq!(b.peer, address("https://a.example", 3));
        assert_eq!(b.secret, Some(secret));
        let b_code = b.verification_code().unwrap();
        assert_eq!(
            b_code,
            crypto::verification_code(&b.key.public_key(), &a.key.public_key(), &secret)
        );

        let reveal = b.reveal(NOW + 10, &mut OsRng).unwrap();
        let message = a.open(&reveal, NOW + 11).unwrap();
        assert_eq!(message.body, Body::Reveal(secret));
        assert_eq!(
            a.receive_reveal(&secret).unwrap(),
            Revealed::Code {
                code: b_code.clone(),
                paired: false
            }
        );
        assert_eq!(a.state, PairingState::AwaitingHelloOk);
        assert_eq!(a.verification_code(), Some(b_code));
        assert!(a.awaiting_reveal.is_none());

        let hello_ok = b.confirm(NOW + 20, &mut OsRng).unwrap();
        assert_eq!(b.state, PairingState::Paired);
        assert_eq!(b.secret, None, "n is deleted on confirming");
        assert_eq!(b.verification_code(), None);
        let message = a.open(&hello_ok, NOW + 21).unwrap();
        assert!(a.receive_hello_ok(&message).unwrap());
        assert_eq!(a.state, PairingState::Paired);
        assert_eq!(a.secret, None);

        // Both directions work afterwards.
        let request = a.seal(Body::Request, NOW + 30, &mut OsRng).unwrap();
        assert_eq!(b.open(&request, NOW + 31).unwrap().body, Body::Request);
        let end = b.seal(Body::End, NOW + 40, &mut OsRng).unwrap();
        assert_eq!(a.open(&end, NOW + 41).unwrap().body, Body::End);
    }

    #[test]
    fn accepting_side_uses_a_fresh_key_each_time() {
        let (_, invitation) =
            PendingInvitation::create(address("https://b.example", 1), None, NOW, &mut OsRng);
        let own = address("https://a.example", 3);
        let (first, _) = accept(&invitation, own.clone(), NOW, &mut OsRng).unwrap();
        let (second, _) = accept(&invitation, own, NOW, &mut OsRng).unwrap();
        assert_ne!(first.key.public_key(), second.key.public_key());
        let wait = first.awaiting_reveal.unwrap();
        assert_eq!(wait.commitment, invitation.commitment);
        assert_eq!((wait.sent_at, wait.resent_at), (NOW, NOW));
    }

    #[test]
    fn reveal_must_match_the_commitment() {
        let (pending, invitation) =
            PendingInvitation::create(address("https://b.example", 1), None, NOW, &mut OsRng);
        let (mut a, hello) = accept(
            &invitation,
            address("https://a.example", 3),
            NOW,
            &mut OsRng,
        )
        .unwrap();
        pending.receive_hello(&hello, NOW).unwrap();
        assert_eq!(a.receive_reveal(&[9; 32]), Err(Error::Mismatch));
        assert_eq!(a.state, PairingState::AwaitingReveal);
        assert_eq!(a.verification_code(), None);

        // After a valid one, the same value is harmless, another one is not.
        let secret = pending.secret.unwrap();
        assert!(matches!(
            a.receive_reveal(&secret),
            Ok(Revealed::Code { .. })
        ));
        assert_eq!(a.receive_reveal(&secret), Ok(Revealed::Again));
        assert_eq!(a.receive_reveal(&[9; 32]), Err(Error::Mismatch));
    }

    #[test]
    fn hello_ok_before_reveal_is_kept_back() {
        let (pending, invitation) =
            PendingInvitation::create(address("https://b.example", 1), None, NOW, &mut OsRng);
        let (mut a, hello) = accept(
            &invitation,
            address("https://a.example", 3),
            NOW,
            &mut OsRng,
        )
        .unwrap();
        let mut b = pending.receive_hello(&hello, NOW).unwrap();
        let code = b.verification_code().unwrap();
        let hello_ok = b.confirm(NOW, &mut OsRng).unwrap();
        let message = a.open(&hello_ok, NOW).unwrap();
        assert!(!a.receive_hello_ok(&message).unwrap());
        assert_eq!(a.state, PairingState::AwaitingReveal);

        assert_eq!(
            a.receive_reveal(&pending.secret.unwrap()).unwrap(),
            Revealed::Code { code, paired: true }
        );
        assert_eq!(a.state, PairingState::Paired);
        assert_eq!(a.secret, None);
    }

    #[test]
    fn hello_is_repeated_with_growing_gaps_and_fails_after_a_day() {
        let (_, invitation) =
            PendingInvitation::create(address("https://b.example", 1), None, NOW, &mut OsRng);
        let (mut a, hello) = accept(
            &invitation,
            address("https://a.example", 3),
            NOW,
            &mut OsRng,
        )
        .unwrap();
        assert_eq!(a.hello_to_repeat(NOW + HELLO_REPEAT_SECS - 1), None);
        assert_eq!(a.hello_to_repeat(NOW + HELLO_REPEAT_SECS), Some(hello));
        // The next one only after twice as long in all.
        assert_eq!(a.hello_to_repeat(NOW + 2 * HELLO_REPEAT_SECS - 1), None);
        assert_eq!(a.hello_to_repeat(NOW + 2 * HELLO_REPEAT_SECS), Some(hello));
        assert_eq!(a.hello_to_repeat(NOW + 4 * HELLO_REPEAT_SECS - 1), None);
        assert_eq!(a.hello_to_repeat(NOW + 4 * HELLO_REPEAT_SECS), Some(hello));

        assert!(!a.pairing_failed(NOW + MAX_MESSAGE_AGE_SECS));
        assert!(a.pairing_failed(NOW + MAX_MESSAGE_AGE_SECS + 1));
    }

    #[test]
    fn expired_invitation_is_refused_on_both_sides() {
        let (pending, invitation) =
            PendingInvitation::create(address("https://b.example", 1), None, NOW, &mut OsRng);
        let late = NOW + INVITATION_TTL_SECS + 1;
        let own = address("https://a.example", 3);
        assert_eq!(
            accept(&invitation, own.clone(), late, &mut OsRng).unwrap_err(),
            Error::Expired
        );
        // A modified client could still send a late `hello`.
        let mut late_invitation = invitation.clone();
        late_invitation.expires_at = late;
        let (_, hello) = accept(&late_invitation, own, late, &mut OsRng).unwrap();
        assert_eq!(
            pending.receive_hello(&hello, late).unwrap_err(),
            Error::Expired
        );
    }

    #[test]
    fn hello_sent_in_time_may_be_read_later() {
        let (pending, invitation) =
            PendingInvitation::create(address("https://b.example", 1), None, NOW, &mut OsRng);
        let sent = NOW + INVITATION_TTL_SECS;
        let (_, hello) = accept(
            &invitation,
            address("https://a.example", 3),
            sent,
            &mut OsRng,
        )
        .unwrap();
        assert!(pending.receive_hello(&hello, NOW + 5 * 3600).is_ok());
        assert_eq!(pending.keep_until(), sent + MAX_MESSAGE_AGE_SECS);
        assert_eq!(
            pending
                .receive_hello(&hello, pending.keep_until() + 1)
                .unwrap_err(),
            Error::Expired
        );
    }

    #[test]
    fn invitation_of_version_one_accepts_nothing() {
        let (mut pending, invitation) =
            PendingInvitation::create(address("https://b.example", 1), None, NOW, &mut OsRng);
        let (_, hello) = accept(
            &invitation,
            address("https://a.example", 3),
            NOW,
            &mut OsRng,
        )
        .unwrap();
        pending.secret = None;
        assert!(pending.is_outdated());
        assert_eq!(
            pending.receive_hello(&hello, NOW).unwrap_err(),
            Error::Unexpected
        );
    }

    #[test]
    fn hello_for_another_invitation_is_refused() {
        let (pending, _) =
            PendingInvitation::create(address("https://b.example", 1), None, NOW, &mut OsRng);
        let (_, other) =
            PendingInvitation::create(address("https://b.example", 5), None, NOW, &mut OsRng);
        let (_, hello) = accept(&other, address("https://a.example", 3), NOW, &mut OsRng).unwrap();
        assert_eq!(
            pending.receive_hello(&hello, NOW).unwrap_err(),
            Error::Decrypt
        );
    }

    #[test]
    fn confirm_reveal_and_hello_ok_only_in_their_state() {
        let (pending, invitation) =
            PendingInvitation::create(address("https://b.example", 1), None, NOW, &mut OsRng);
        let (mut a, hello) = accept(
            &invitation,
            address("https://a.example", 3),
            NOW,
            &mut OsRng,
        )
        .unwrap();
        let mut b = pending.receive_hello(&hello, NOW).unwrap();
        assert_eq!(a.confirm(NOW, &mut OsRng).unwrap_err(), Error::Unexpected);
        assert_eq!(a.reveal(NOW, &mut OsRng).unwrap_err(), Error::Unexpected);
        assert_eq!(
            b.receive_reveal(&pending.secret.unwrap()),
            Err(Error::Unexpected)
        );

        let request = b.seal(Body::Request, NOW, &mut OsRng).unwrap();
        let message = a.open(&request, NOW).unwrap();
        assert_eq!(a.receive_hello_ok(&message), Err(Error::Unexpected));

        b.confirm(NOW, &mut OsRng).unwrap();
        assert_eq!(b.confirm(NOW, &mut OsRng).unwrap_err(), Error::Unexpected);
        assert_eq!(b.reveal(NOW, &mut OsRng).unwrap_err(), Error::Unexpected);
    }

    #[test]
    fn messages_outside_the_time_window_are_refused() {
        let (pending, invitation) =
            PendingInvitation::create(address("https://b.example", 1), None, NOW, &mut OsRng);
        let (a, hello) = accept(
            &invitation,
            address("https://a.example", 3),
            NOW,
            &mut OsRng,
        )
        .unwrap();
        let b = pending.receive_hello(&hello, NOW).unwrap();
        let request = a.seal(Body::Request, NOW, &mut OsRng).unwrap();
        assert!(b.open(&request, NOW + MAX_MESSAGE_AGE_SECS).is_ok());
        assert_eq!(
            b.open(&request, NOW + MAX_MESSAGE_AGE_SECS + 1),
            Err(Error::Expired)
        );
        assert!(b.open(&request, NOW - MAX_CLOCK_AHEAD_SECS).is_ok());
        assert_eq!(
            b.open(&request, NOW - MAX_CLOCK_AHEAD_SECS - 1),
            Err(Error::Expired)
        );
    }

    /// Mallory swaps B's invitation to A for her own and accepts B's
    /// invitation herself. In version 1 she could search her keys until both
    /// codes matched. Now each code depends on a secret value that is fixed
    /// before Mallory could search and revealed only after her key is fixed.
    #[test]
    fn swapped_invitation_cannot_be_matched_by_searching() {
        let rng = &mut TestRng::new(1);
        let (b_pending, b_invitation) =
            PendingInvitation::create(address("https://b.example", 1), None, NOW, rng);

        // Towards B, Mallory has to send her `hello` before she learns n_B.
        // She can pick her key freely, here the best of many, but without
        // n_B every choice is a guess.
        let (mallory_to_b, hello) =
            accept(&b_invitation, address("https://m.example", 5), NOW, rng).unwrap();
        let mut b = ContactState::new(b_pending.receive_hello(&hello, NOW).unwrap());
        let b_code = b.contact.verification_code().unwrap();
        // Only now n_B reaches her.
        let reveal = b.contact.reveal(NOW, rng).unwrap();
        let n_b = match mallory_to_b.open(&reveal, NOW).unwrap().body {
            Body::Reveal(n) => n,
            other => panic!("{other:?}"),
        };
        // A second `hello` with a better key gets no `reveal` and a warning.
        let (_, better) = accept(&b_invitation, address("https://m.example", 7), NOW, rng).unwrap();
        assert_eq!(b.receive(&better, NOW).unwrap(), Event::SecondAcceptance);

        // Towards A, her invitation commits to n_M before A's fresh key
        // exists, and A's key is new for this invitation.
        let (m_pending, m_invitation) =
            PendingInvitation::create(address("https://m.example", 9), None, NOW, rng);
        let (a, hello) = accept(&m_invitation, address("https://a.example", 3), NOW, rng).unwrap();
        let mut a = ContactState::new(a);
        let mallory_to_a = m_pending.receive_hello(&hello, NOW).unwrap();
        let a_key = a.contact.key.public_key();
        let m_key = mallory_to_a.key.public_key();
        // Knowing A's key and n_B now, she searches for a value that would
        // give A the code B shows. Any value but n_M is refused by A.
        let searched = (0u32..20_000)
            .map(|i| crypto::sha256(&[&n_b, &i.to_be_bytes()]))
            .find(|n| crypto::verification_code(&m_key, &a_key, n) == b_code)
            .unwrap_or(n_b);
        let forged = mallory_to_a.seal(Body::Reveal(searched), NOW, rng).unwrap();
        assert_eq!(
            a.clone().receive(&forged, NOW).unwrap(),
            Event::RevealMismatch
        );
        // With the committed value, the codes differ.
        let reveal = mallory_to_a.reveal(NOW, rng).unwrap();
        let Event::Revealed { code: a_code, .. } = a.receive(&reveal, NOW).unwrap() else {
            panic!("reveal not accepted");
        };
        assert_ne!(a_code, b_code);
        assert!(!a.is_paired());
    }
}
