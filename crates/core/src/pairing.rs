//! Pairing of two people: invitation, `hello`, verification code, `hello_ok`.
//!
//! Nothing here talks to a relay or a disk. Creating mailboxes, delivering
//! envelopes and storing the results is up to the caller.
//!
//! ```text
//! B (invites)                                   A (accepts)
//! create mailbox, Invitation::create  ──code──▶  Invitation::from_text
//!                                                create own mailbox
//!                                     ◀─hello──  accept
//! PendingInvitation::receive_hello
//! both compare the verification code
//! Contact::confirm                    ─hello_ok▶ Contact::receive_hello_ok
//! ```

use rand_core::CryptoRngCore;

use crate::Error;
use crate::crypto::{self, ContactKey};
use crate::wire::{
    Body, ENVELOPE_LEN, Hello, INVITATION_TTL_SECS, Invitation, MAX_CLOCK_AHEAD_SECS,
    MAX_MESSAGE_AGE_SECS, MailboxId, Message, PublicKey, RelayUrl, Token,
};

/// A mailbox on a relay and the token to deliver into it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    pub relay: RelayUrl,
    pub mailbox: MailboxId,
    pub send_token: Token,
}

/// Where a contact stands in the pairing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairingState {
    /// Accepted an invitation and sent `hello`; waiting for `hello_ok`.
    AwaitingHelloOk,
    /// Received `hello`; waiting for the local person to compare the code.
    AwaitingConfirmation,
    /// Both sides are paired.
    Paired,
}

/// The cryptographic side of a contact. Names never leave the device and are
/// not part of this.
#[derive(Clone, Debug)]
pub struct Contact {
    pub key: ContactKey,
    pub peer_key: PublicKey,
    /// Mailbox of the other side, into which this side delivers.
    pub peer: Address,
    /// Own mailbox, from which this side reads messages of this contact.
    pub own_mailbox: MailboxId,
    pub state: PairingState,
}

/// An invitation handed out and not yet accepted.
#[derive(Clone, Debug)]
pub struct PendingInvitation {
    pub key: ContactKey,
    pub own: Address,
    pub expires_at: u64,
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
        let expires_at = now + INVITATION_TTL_SECS;
        let invitation = Invitation {
            expires_at,
            mailbox: own.mailbox,
            send_token: own.send_token,
            public_key: key.public_key(),
            voucher,
            relay: own.relay,
        };
        let pending = Self {
            key,
            own,
            expires_at,
        };
        (pending, invitation)
    }

    /// Reads an envelope that arrived in the mailbox of this invitation.
    ///
    /// Returns the new contact, waiting for confirmation. The caller must
    /// then drop the pending invitation, so that only one `hello` is accepted.
    /// Anything else, including a late `hello`, fails and should be dropped
    /// silently.
    pub fn receive_hello(&self, envelope: &[u8], now: u64) -> Result<Contact, Error> {
        if now > self.expires_at {
            return Err(Error::Expired);
        }
        let message = crypto::open_anonymous(envelope, &self.key)?;
        check_time(&message, now)?;
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
        })
    }
}

/// Accepts an invitation; `own` is the mailbox just created for this contact.
///
/// Returns the new contact and the `hello` to deliver into the mailbox of the
/// inviting side.
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
        state: PairingState::AwaitingHelloOk,
    };
    Ok((contact, envelope))
}

impl Contact {
    /// Six digits for both people to compare.
    pub fn verification_code(&self) -> String {
        crypto::verification_code(&self.key.public_key(), &self.peer_key)
    }

    /// The inviting person confirmed the code; returns `hello_ok` to send.
    pub fn confirm(
        &mut self,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<[u8; ENVELOPE_LEN], Error> {
        if self.state != PairingState::AwaitingConfirmation {
            return Err(Error::Unexpected);
        }
        let envelope = self.seal(Body::HelloOk, now, rng)?;
        self.state = PairingState::Paired;
        Ok(envelope)
    }

    /// Handles a decrypted message while waiting for `hello_ok`.
    pub fn receive_hello_ok(&mut self, message: &Message) -> Result<(), Error> {
        if self.state != PairingState::AwaitingHelloOk || message.body != Body::HelloOk {
            return Err(Error::Unexpected);
        }
        self.state = PairingState::Paired;
        Ok(())
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
fn check_time(message: &Message, now: u64) -> Result<(), Error> {
    let too_old = message.sent_at.saturating_add(MAX_MESSAGE_AGE_SECS) < now;
    let too_new = message.sent_at > now.saturating_add(MAX_CLOCK_AHEAD_SECS);
    if too_old || too_new {
        return Err(Error::Expired);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rand_core::OsRng;

    use super::{Address, PairingState, PendingInvitation, accept};
    use crate::Error;
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

    #[test]
    fn full_pairing_through_text_code() {
        let (pending, invitation) =
            PendingInvitation::create(address("https://b.example", 1), None, NOW, &mut OsRng);
        let mut buf = [0; crate::wire::invitation::MAX_TEXT_LEN];
        let code = invitation.to_text(&mut buf);
        let invitation = Invitation::from_text(code).unwrap();

        let (mut a, hello) = accept(
            &invitation,
            address("https://a.example", 3),
            NOW + 5,
            &mut OsRng,
        )
        .unwrap();
        assert_eq!(a.state, PairingState::AwaitingHelloOk);
        assert_eq!(a.peer, address("https://b.example", 1));

        let mut b = pending.receive_hello(&hello, NOW + 10).unwrap();
        assert_eq!(b.state, PairingState::AwaitingConfirmation);
        assert_eq!(b.peer, address("https://a.example", 3));
        assert_eq!(b.verification_code(), a.verification_code());

        let hello_ok = b.confirm(NOW + 20, &mut OsRng).unwrap();
        assert_eq!(b.state, PairingState::Paired);
        let message = a.open(&hello_ok, NOW + 21).unwrap();
        a.receive_hello_ok(&message).unwrap();
        assert_eq!(a.state, PairingState::Paired);

        // Both directions work afterwards.
        let request = a.seal(Body::Request, NOW + 30, &mut OsRng).unwrap();
        assert_eq!(b.open(&request, NOW + 31).unwrap().body, Body::Request);
        let end = b.seal(Body::End, NOW + 40, &mut OsRng).unwrap();
        assert_eq!(a.open(&end, NOW + 41).unwrap().body, Body::End);
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
        let (_, hello) = accept(&invitation, own, NOW, &mut OsRng).unwrap();
        assert_eq!(
            pending.receive_hello(&hello, late).unwrap_err(),
            Error::Expired
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
    fn confirm_and_hello_ok_only_in_their_state() {
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

        let request = b.seal(Body::Request, NOW, &mut OsRng).unwrap();
        let message = a.open(&request, NOW).unwrap();
        assert_eq!(a.receive_hello_ok(&message), Err(Error::Unexpected));

        b.confirm(NOW, &mut OsRng).unwrap();
        assert_eq!(b.confirm(NOW, &mut OsRng).unwrap_err(), Error::Unexpected);
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
}
