//! Everything that happens with a contact after pairing: rules, requests,
//! responses, ending the contact.
//!
//! [`ContactState`] holds the whole state of one contact and is the unit the
//! caller stores. Like the rest of the protocol part it takes time and
//! randomness as arguments and does no I/O.

use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::crypto;
use crate::pairing::{Contact, PairingState, Revealed, check_time, new_message};
use crate::rules::Ledger;
use crate::wire::{
    Body, ENVELOPE_LEN, Message, MessageId, REPLAY_MEMORY_SECS, Reason, Response, ResponseKind,
    Rules, Stamp,
};

/// State of one contact as stored on the device.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContactState {
    pub contact: Contact,
    /// How often the other side may ask; enforced here.
    pub own_rules: Rules,
    /// How often this side may ask, as last announced by the other side.
    pub peer_rules: Rules,
    /// Requests from the other side.
    pub incoming: Ledger,
    /// Requests to the other side.
    pub outgoing: Ledger,
    /// Recently received messages, to drop repetitions.
    pub seen: Vec<Seen>,
    /// One side has ended the contact. Nothing is sent or accepted anymore.
    pub ended: bool,
    /// Time and identifier of the last applied own rules; zero for none.
    #[serde(default, skip_serializing_if = "is_unset", with = "stamp")]
    pub own_rules_stamp: Stamp,
    /// Time and identifier of the last applied rules of the other side.
    #[serde(default, skip_serializing_if = "is_unset", with = "stamp")]
    pub peer_rules_stamp: Stamp,
}

pub(crate) fn is_unset(stamp: &Stamp) -> bool {
    *stamp == Stamp::default()
}

/// [`Stamp`] in stored JSON: `{"at": …, "id": "<hex>"}`.
pub(crate) mod stamp {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::wire::{MessageId, Stamp};

    #[derive(Serialize, Deserialize)]
    struct Stored {
        at: u64,
        #[serde(with = "crate::hex")]
        id: MessageId,
    }

    pub fn serialize<S: Serializer>(stamp: &Stamp, serializer: S) -> Result<S::Ok, S::Error> {
        Stored {
            at: stamp.at,
            id: stamp.id,
        }
        .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Stamp, D::Error> {
        Stored::deserialize(deserializer).map(|s| Stamp { at: s.at, id: s.id })
    }
}

/// A stamp for new own rules, newer than `last`: now with a random
/// identifier, one second later if that would not be newer.
pub(crate) fn next_stamp(last: Stamp, now: u64, rng: &mut impl CryptoRngCore) -> Stamp {
    let mut id = [0; 16];
    rng.fill_bytes(&mut id);
    let mut stamp = Stamp {
        at: now.max(last.at),
        id,
    };
    if stamp <= last {
        stamp.at = last.at.saturating_add(1);
    }
    stamp
}

/// A received message, remembered for [`REPLAY_MEMORY_SECS`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    #[serde(with = "crate::hex")]
    pub id: MessageId,
    /// Sending time of the message.
    pub at: u64,
}

/// Remembers a received message in `seen`; returns `true` if it was seen
/// before. Forgets entries older than [`REPLAY_MEMORY_SECS`].
pub(crate) fn remember(seen: &mut Vec<Seen>, id: &MessageId, sent_at: u64, now: u64) -> bool {
    seen.retain(|seen| now < seen.at.saturating_add(REPLAY_MEMORY_SECS));
    if seen.iter().any(|seen| &seen.id == id) {
        return true;
    }
    seen.push(Seen {
        id: *id,
        at: sent_at,
    });
    false
}

/// What a received envelope meant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// `hello_ok` arrived. The caller should now send [`ContactState::announce_rules`].
    Paired,
    /// A valid `reveal` arrived on the accepting side: the code can be shown
    /// now. If `paired`, a `hello_ok` kept back has completed the pairing as
    /// well; the caller then acts as for [`Event::Paired`].
    Revealed { code: String, paired: bool },
    /// A `reveal` that does not match the commitment of the invitation:
    /// someone interferes. The caller shows an alarm, sends
    /// [`ContactState::end`] and deletes the own mailbox.
    RevealMismatch,
    /// `hello_ok` arrived before `reveal` and is kept until a valid one.
    HelloOkKept,
    /// The `hello` this contact was made from arrived again, while the
    /// person has not confirmed yet: the caller sends
    /// [`Contact::reveal`] again, in case the first one got lost.
    HelloAgain,
    /// The other side changed how often it may be asked.
    Rules(Rules),
    /// A request was accepted and should be shown.
    Request(MessageId),
    /// A response to an own request. A manual one replaces a reported one.
    Response {
        request: MessageId,
        kind: ResponseKind,
        /// Sending time of the response.
        at: u64,
    },
    /// The other side ended the contact.
    Ended,
    /// The other side rejected the pairing before it was complete, usually
    /// because the verification codes did not match. The contact has ended.
    Rejected,
    /// Another `hello` with a different key arrived for the invitation this
    /// contact came from. Someone else knows the invitation code; whoever
    /// confirmed or confirms must be sure the verification code matched.
    SecondAcceptance,
    /// The message was dropped silently.
    Ignored(Ignored),
}

/// Why a valid message was dropped. Only for diagnostics; the other side
/// never learns about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ignored {
    /// The identifier was seen before.
    Duplicate,
    /// Pairing is not complete, or the message type belongs to pairing.
    NotExpected,
    /// The request breaks the own rules.
    RuleViolation,
    /// The request had already expired when it arrived.
    Expired,
    /// Response to an unknown or expired request.
    UnknownRequest,
    /// Second response of the same kind, or a reported one after a manual one.
    AlreadyAnswered,
    /// A message type this version does not know.
    UnknownType,
    /// Rules not newer than the ones applied last.
    Outdated,
    /// The contact has ended.
    Ended,
}

impl ContactState {
    /// Wraps a contact fresh from pairing, with default rules on both sides.
    pub fn new(contact: Contact) -> Self {
        Self {
            contact,
            own_rules: Rules::DEFAULT,
            peer_rules: Rules::DEFAULT,
            incoming: Ledger::default(),
            outgoing: Ledger::default(),
            seen: Vec::new(),
            ended: false,
            own_rules_stamp: Stamp::default(),
            peer_rules_stamp: Stamp::default(),
        }
    }

    pub fn is_paired(&self) -> bool {
        self.contact.state == PairingState::Paired
    }

    /// The inviting person confirmed the code. Returns `hello_ok` and the own
    /// rules, to be delivered in this order.
    pub fn confirm(
        &mut self,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<[[u8; ENVELOPE_LEN]; 2], Error> {
        if self.ended {
            return Err(Error::Ended);
        }
        let hello_ok = self.contact.confirm(now, rng)?;
        Ok([hello_ok, self.announce_rules(now, rng)?])
    }

    /// The own rules as a message, sent after pairing and after each change.
    ///
    /// Its time and identifier become [`Self::own_rules_stamp`] and are
    /// always newer than the previous one, so that the contact applies it
    /// even if sent in the same second. A `rules_set` to the own devices
    /// reuses them, see [`crate::store::Device::sync_rules_set`].
    pub fn announce_rules(
        &mut self,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<[u8; ENVELOPE_LEN], Error> {
        let stamp = next_stamp(self.own_rules_stamp, now, rng);
        let message = Message {
            id: stamp.id,
            sent_at: stamp.at,
            body: Body::Rules(self.own_rules),
        };
        let envelope = self.contact.seal_message(&message, rng)?;
        self.own_rules_stamp = stamp;
        Ok(envelope)
    }

    /// Changes the own rules and returns the message announcing them.
    pub fn set_rules(
        &mut self,
        rules: Rules,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<[u8; ENVELOPE_LEN], Error> {
        self.check_active()?;
        self.own_rules = rules;
        self.announce_rules(now, rng)
    }

    /// Earliest time this side may ask again; at most `now` means right away.
    pub fn next_request_at(&self, now: u64) -> u64 {
        self.outgoing.next_allowed(&self.peer_rules, now)
    }

    /// Asks for a sign of life.
    ///
    /// Fails with [`Error::NotYet`] while the rules of the other side forbid
    /// it; an unchanged client never sends such a request.
    pub fn request(
        &mut self,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(MessageId, [u8; ENVELOPE_LEN]), Error> {
        self.check_active()?;
        self.outgoing.prune(&self.peer_rules, now);
        let next = self.next_request_at(now);
        if next > now {
            return Err(Error::NotYet(next));
        }
        let message = new_message(Body::Request, now, rng);
        let envelope = self.contact.seal_message(&message, rng)?;
        self.outgoing.record(message.id, now);
        Ok((message.id, envelope))
    }

    /// The open request from the other side, if any.
    pub fn open_request(&self, now: u64) -> Option<MessageId> {
        self.incoming.open(&self.own_rules, now).map(|e| e.id)
    }

    /// Someone reacted to the open request on this device.
    pub fn respond(
        &mut self,
        request: &MessageId,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<[u8; ENVELOPE_LEN], Error> {
        self.check_active()?;
        if self.open_request(now).as_ref() != Some(request) {
            return Err(Error::Unexpected);
        }
        let envelope = self.contact.seal(
            Body::Response(Response {
                request: *request,
                kind: ResponseKind::Manual,
            }),
            now,
            rng,
        )?;
        self.incoming.answer(request, now);
        Ok(envelope)
    }

    /// Sends a reported state for the open request.
    ///
    /// Whether and when to do this (waiting time, permission per contact) is
    /// decided by the caller. At most one reported response per request; the
    /// request stays open for a manual response.
    pub fn report(
        &mut self,
        request: &MessageId,
        reason: Reason,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<[u8; ENVELOPE_LEN], Error> {
        self.check_active()?;
        let entry = self
            .incoming
            .current_mut(request, &self.own_rules, now)
            .filter(|e| e.answered_at.is_none() && e.reported.is_none())
            .ok_or(Error::Unexpected)?;
        let envelope = self.contact.seal(
            Body::Response(Response {
                request: *request,
                kind: ResponseKind::Reported(reason),
            }),
            now,
            rng,
        )?;
        entry.reported = Some(reason);
        Ok(envelope)
    }

    /// Ends the contact and returns `end` for the other side.
    ///
    /// Also works before pairing is complete; that is how a pairing with a
    /// wrong verification code is rejected visibly. The caller then deletes
    /// the own mailbox of this contact.
    pub fn end(
        &mut self,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<[u8; ENVELOPE_LEN], Error> {
        if self.ended {
            return Err(Error::Ended);
        }
        let envelope = self.contact.seal(Body::End, now, rng)?;
        self.set_ended();
        Ok(envelope)
    }

    /// Handles an envelope from the own mailbox of this contact.
    ///
    /// Errors mean the envelope is not a valid message from this contact.
    /// Both errors and [`Event::Ignored`] are dropped silently by the caller.
    pub fn receive(&mut self, envelope: &[u8], now: u64) -> Result<Event, Error> {
        if self.ended {
            return Ok(Event::Ignored(Ignored::Ended));
        }
        let message = match self.contact.open(envelope, now) {
            Ok(message) => message,
            Err(Error::Decrypt) => return self.receive_sealed(envelope, now),
            Err(error) => return Err(error),
        };
        if self.remember(&message.id, message.sent_at, now) {
            return Ok(Event::Ignored(Ignored::Duplicate));
        }

        if !self.is_paired() {
            return Ok(match &message.body {
                Body::End => {
                    self.set_ended();
                    Event::Rejected
                }
                Body::HelloOk => match self.contact.receive_hello_ok(&message) {
                    Ok(true) => Event::Paired,
                    Ok(false) => Event::HelloOkKept,
                    Err(_) => Event::Ignored(Ignored::NotExpected),
                },
                Body::Reveal(secret) => match self.contact.receive_reveal(secret) {
                    Ok(Revealed::Code { code, paired }) => Event::Revealed { code, paired },
                    Ok(Revealed::Again) => Event::Ignored(Ignored::Duplicate),
                    Err(Error::Mismatch) => Event::RevealMismatch,
                    Err(_) => Event::Ignored(Ignored::NotExpected),
                },
                _ => Event::Ignored(Ignored::NotExpected),
            });
        }

        // Times from the other side are capped at the own clock.
        let at = message.sent_at.min(now);
        Ok(match message.body {
            Body::Hello(_) | Body::HelloOk | Body::Reveal(_) => {
                Event::Ignored(Ignored::NotExpected)
            }
            Body::Rules(rules) => {
                // Two devices of the contact may have sent rules; the newest
                // wins, judged by the uncapped sending time.
                let stamp = Stamp {
                    at: message.sent_at,
                    id: message.id,
                };
                if stamp <= self.peer_rules_stamp {
                    return Ok(Event::Ignored(Ignored::Outdated));
                }
                self.peer_rules = rules;
                self.peer_rules_stamp = stamp;
                Event::Rules(rules)
            }
            Body::Request => self.accept_request(message.id, at, now),
            Body::Response(response) => self.accept_response(response, at),
            Body::End => {
                self.set_ended();
                Event::Ended
            }
            // Messages between own devices never come from a contact.
            Body::Device(_)
            | Body::DeviceRemoved(_)
            | Body::Requested(_)
            | Body::Responded(_)
            | Body::RulesSet(_)
            | Body::ContactPart(_)
            | Body::ContactRemoved(_)
            | Body::StateShared(_)
            | Body::StateCleared => Event::Ignored(Ignored::NotExpected),
            Body::Unknown(_) => Event::Ignored(Ignored::UnknownType),
        })
    }

    /// Applies own rules set on another device, if they are newer than the
    /// last applied ones. Returns whether they were applied.
    pub fn apply_own_rules(&mut self, rules: Rules, stamp: Stamp) -> bool {
        if stamp <= self.own_rules_stamp {
            return false;
        }
        self.own_rules = rules;
        self.own_rules_stamp = stamp;
        true
    }

    /// Records a request another own device sent to this contact, so that
    /// its response is accepted here too.
    pub fn apply_requested(&mut self, request: MessageId, at: u64, now: u64) {
        self.outgoing.prune(&self.peer_rules, now);
        if !self.outgoing.entries.iter().any(|e| e.id == request) {
            self.outgoing.record(request, at);
        }
    }

    /// Records a response another own device sent at `at`, as if this device
    /// had sent it.
    ///
    /// If the request has not arrived here yet, it is recorded with the time
    /// of the response, so that it is not shown as open when it arrives; see
    /// [`Self::receive`].
    pub fn apply_responded(&mut self, request: &MessageId, kind: &ResponseKind, at: u64) {
        if !self.incoming.entries.iter().any(|e| &e.id == request) {
            self.incoming.record(*request, at);
        }
        match kind {
            ResponseKind::Manual => {
                self.incoming.answer(request, at);
            }
            ResponseKind::Reported(reason) => {
                if let Some(entry) = self
                    .incoming
                    .entries
                    .iter_mut()
                    .find(|e| &e.id == request && e.answered_at.is_none())
                {
                    entry.reported.get_or_insert(*reason);
                }
            }
        }
    }

    /// Handles an envelope that is not in the ordinary form for this contact.
    ///
    /// The only sealed message that makes sense here is a further `hello` for
    /// the invitation this contact came from: with the key of this contact,
    /// it asks for `reveal` again; with another key, someone else accepted
    /// the invitation too, and gets nothing.
    fn receive_sealed(&mut self, envelope: &[u8], now: u64) -> Result<Event, Error> {
        let message = crypto::open_anonymous(envelope, &self.contact.key)?;
        check_time(&message, now)?;
        let Body::Hello(hello) = &message.body else {
            return Err(Error::Decrypt);
        };
        let seen = self.remember(&message.id, message.sent_at, now);
        if hello.public_key == self.contact.peer_key {
            // The `hello` this contact was made from. A repetition is the
            // purpose here, so it is not dropped as a duplicate.
            let waiting = self.contact.state == PairingState::AwaitingConfirmation
                && self.contact.secret.is_some();
            return Ok(if waiting {
                Event::HelloAgain
            } else {
                Event::Ignored(Ignored::Duplicate)
            });
        }
        if seen {
            return Ok(Event::Ignored(Ignored::Duplicate));
        }
        Ok(Event::SecondAcceptance)
    }

    /// Marks the contact as ended and deletes what only the pairing needed.
    fn set_ended(&mut self) {
        self.ended = true;
        self.contact.forget_pairing();
    }

    /// Whether a pairing that is not complete can no longer complete; see
    /// [`Contact::pairing_failed`]. The caller then shows that the pairing
    /// failed, sends [`Self::end`] and deletes the own mailbox.
    pub fn pairing_failed(&self, now: u64) -> bool {
        !self.ended && self.contact.pairing_failed(now)
    }

    /// Remembers a received message; returns `true` if it was seen before.
    fn remember(&mut self, id: &MessageId, sent_at: u64, now: u64) -> bool {
        remember(&mut self.seen, id, sent_at, now)
    }

    fn accept_request(&mut self, id: MessageId, at: u64, now: u64) -> Event {
        let rules = self.own_rules;
        if let Some(pos) = self.incoming.entries.iter().position(|e| e.id == id) {
            // Another own device answered it before it arrived here; it was
            // accepted there and now gets its real time.
            let mut entry = self.incoming.entries.remove(pos);
            entry.at = at;
            let open = entry.answered_at.is_none();
            self.incoming.insert(entry);
            let still_open = self.incoming.open(&rules, now).is_some_and(|e| e.id == id);
            return if open && still_open {
                Event::Request(id)
            } else {
                Event::Ignored(Ignored::AlreadyAnswered)
            };
        }
        self.incoming.prune(&rules, now);
        if self.incoming.next_allowed(&rules, now) > now {
            return Event::Ignored(Ignored::RuleViolation);
        }
        if at.saturating_add(u64::from(rules.expiry_minutes) * 60) <= now {
            return Event::Ignored(Ignored::Expired);
        }
        self.incoming.record(id, at);
        Event::Request(id)
    }

    fn accept_response(&mut self, response: Response, at: u64) -> Event {
        // A response counts if it was sent while the request was current,
        // even if it arrives later.
        let Some(entry) = self
            .outgoing
            .current_mut(&response.request, &self.peer_rules, at)
        else {
            return Event::Ignored(Ignored::UnknownRequest);
        };
        if entry.answered_at.is_some() {
            return Event::Ignored(Ignored::AlreadyAnswered);
        }
        match &response.kind {
            ResponseKind::Manual => {
                self.outgoing.answer(&response.request, at);
            }
            ResponseKind::Reported(reason) => {
                if entry.reported.is_some() {
                    return Event::Ignored(Ignored::AlreadyAnswered);
                }
                entry.reported = Some(*reason);
            }
        }
        Event::Response {
            request: response.request,
            kind: response.kind,
            at,
        }
    }

    fn check_active(&self) -> Result<(), Error> {
        if self.ended {
            Err(Error::Ended)
        } else if !self.is_paired() {
            Err(Error::Unexpected)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use rand_core::OsRng;

    use super::{ContactState, Event, Ignored};
    use crate::Error;
    use crate::pairing::{Address, PendingInvitation, accept};
    use crate::rules::WEEK_SECS;
    use crate::wire::{Body, ENVELOPE_LEN, Reason, RelayUrl, ResponseKind, Rules, Stamp};

    const T: u64 = 1_794_600_000;
    const HOUR: u64 = 3600;

    fn address(fill: u8) -> Address {
        Address {
            relay: RelayUrl::new("https://relay.example").unwrap(),
            mailbox: [fill; 32],
            send_token: [fill + 1; 32],
        }
    }

    /// Pairs A (asks) and B (is asked) completely, rules exchanged.
    fn paired() -> (ContactState, ContactState) {
        let (pending, invitation) = PendingInvitation::create(address(1), None, T, &mut OsRng);
        let (a, hello) = accept(&invitation, address(3), T, &mut OsRng).unwrap();
        let mut a = ContactState::new(a);
        let mut b = ContactState::new(pending.receive_hello(&hello, T).unwrap());
        let reveal = b.contact.reveal(T, &mut OsRng).unwrap();
        assert!(matches!(
            a.receive(&reveal, T).unwrap(),
            Event::Revealed { paired: false, .. }
        ));
        let [hello_ok, rules] = b.confirm(T, &mut OsRng).unwrap();
        assert_eq!(a.receive(&hello_ok, T).unwrap(), Event::Paired);
        assert_eq!(a.receive(&rules, T).unwrap(), Event::Rules(Rules::DEFAULT));
        let rules = a.announce_rules(T, &mut OsRng).unwrap();
        assert_eq!(b.receive(&rules, T).unwrap(), Event::Rules(Rules::DEFAULT));
        (a, b)
    }

    /// A request as a modified client would send it, ignoring the rules.
    fn forced_request(a: &ContactState, now: u64) -> [u8; crate::wire::ENVELOPE_LEN] {
        a.contact.seal(Body::Request, now, &mut OsRng).unwrap()
    }

    #[test]
    fn request_and_manual_response() {
        let (mut a, mut b) = paired();
        let (id, request) = a.request(T + HOUR, &mut OsRng).unwrap();
        assert_eq!(b.receive(&request, T + HOUR).unwrap(), Event::Request(id));
        assert_eq!(b.open_request(T + HOUR), Some(id));

        let response = b.respond(&id, T + 2 * HOUR, &mut OsRng).unwrap();
        assert_eq!(
            a.receive(&response, T + 2 * HOUR).unwrap(),
            Event::Response {
                request: id,
                kind: ResponseKind::Manual,
                at: T + 2 * HOUR,
            }
        );
        assert_eq!(b.open_request(T + 2 * HOUR), None);
        assert_eq!(a.next_request_at(T + 2 * HOUR), T + 8 * HOUR);
    }

    #[test]
    fn minimum_interval_is_enforced_on_both_sides() {
        let (mut a, mut b) = paired();
        let (id, request) = a.request(T, &mut OsRng).unwrap();
        b.receive(&request, T).unwrap();
        let response = b.respond(&id, T, &mut OsRng).unwrap();
        a.receive(&response, T).unwrap();

        assert_eq!(
            a.request(T + HOUR, &mut OsRng).unwrap_err(),
            Error::NotYet(T + 6 * HOUR)
        );
        let forced = forced_request(&a, T + HOUR);
        assert_eq!(
            b.receive(&forced, T + HOUR).unwrap(),
            Event::Ignored(Ignored::RuleViolation)
        );
        let (_, request) = a.request(T + 6 * HOUR, &mut OsRng).unwrap();
        assert!(matches!(
            b.receive(&request, T + 6 * HOUR).unwrap(),
            Event::Request(_)
        ));
    }

    #[test]
    fn only_one_open_request() {
        let (mut a, mut b) = paired();
        let (_, request) = a.request(T, &mut OsRng).unwrap();
        b.receive(&request, T).unwrap();
        assert_eq!(
            a.request(T + HOUR, &mut OsRng).unwrap_err(),
            Error::NotYet(T + 24 * HOUR)
        );
        let forced = forced_request(&a, T + HOUR);
        assert_eq!(
            b.receive(&forced, T + HOUR).unwrap(),
            Event::Ignored(Ignored::RuleViolation)
        );
        // After expiry, the next request is allowed and the old one counts.
        let (_, request) = a.request(T + 24 * HOUR, &mut OsRng).unwrap();
        assert!(matches!(
            b.receive(&request, T + 24 * HOUR).unwrap(),
            Event::Request(_)
        ));
        assert_eq!(b.incoming.entries.len(), 2);
    }

    #[test]
    fn weekly_limit_with_changed_rules() {
        let (mut a, mut b) = paired();
        let rules = Rules {
            min_interval_minutes: 0,
            expiry_minutes: 60,
            weekly_limit: 2,
        };
        let announce = b.set_rules(rules, T, &mut OsRng).unwrap();
        assert_eq!(a.receive(&announce, T).unwrap(), Event::Rules(rules));

        for at in [T, T + HOUR] {
            let (id, request) = a.request(at, &mut OsRng).unwrap();
            assert_eq!(b.receive(&request, at).unwrap(), Event::Request(id));
            let response = b.respond(&id, at, &mut OsRng).unwrap();
            a.receive(&response, at).unwrap();
        }
        assert_eq!(
            a.request(T + 2 * HOUR, &mut OsRng).unwrap_err(),
            Error::NotYet(T + WEEK_SECS)
        );
        let forced = forced_request(&a, T + 2 * HOUR);
        assert_eq!(
            b.receive(&forced, T + 2 * HOUR).unwrap(),
            Event::Ignored(Ignored::RuleViolation)
        );
        assert!(a.request(T + WEEK_SECS, &mut OsRng).is_ok());
    }

    #[test]
    fn reported_response_keeps_request_open_until_manual() {
        let (mut a, mut b) = paired();
        let (id, request) = a.request(T, &mut OsRng).unwrap();
        b.receive(&request, T).unwrap();
        let reason = Reason::new("schläft").unwrap();

        let reported = b.report(&id, reason, T + 1000, &mut OsRng).unwrap();
        assert_eq!(
            b.report(&id, reason, T + 1001, &mut OsRng).unwrap_err(),
            Error::Unexpected
        );
        assert_eq!(
            a.receive(&reported, T + 1000).unwrap(),
            Event::Response {
                request: id,
                kind: ResponseKind::Reported(reason),
                at: T + 1000,
            }
        );
        assert_eq!(b.open_request(T + 1000), Some(id));
        assert_eq!(a.next_request_at(T + 1000), T + 24 * HOUR);

        let manual = b.respond(&id, T + 2000, &mut OsRng).unwrap();
        assert!(matches!(
            a.receive(&manual, T + 2000).unwrap(),
            Event::Response {
                kind: ResponseKind::Manual,
                ..
            }
        ));
        assert_eq!(
            b.report(&id, reason, T + 2001, &mut OsRng).unwrap_err(),
            Error::Unexpected
        );
    }

    #[test]
    fn late_response_and_response_to_unknown_request_are_dropped() {
        let (mut a, mut b) = paired();
        let (id, request) = a.request(T, &mut OsRng).unwrap();
        b.receive(&request, T).unwrap();
        assert_eq!(
            b.respond(&id, T + 24 * HOUR, &mut OsRng).unwrap_err(),
            Error::Unexpected
        );
        assert_eq!(
            b.respond(&[0; 16], T, &mut OsRng).unwrap_err(),
            Error::Unexpected
        );

        // A response sent before expiry still counts when it arrives later.
        let response = b.respond(&id, T + 23 * HOUR, &mut OsRng).unwrap();
        assert!(matches!(
            a.receive(&response, T + 25 * HOUR).unwrap(),
            Event::Response { .. }
        ));
        // A second manual response is dropped.
        let again = b
            .contact
            .seal(
                Body::Response(crate::wire::Response {
                    request: id,
                    kind: ResponseKind::Manual,
                }),
                T + 23 * HOUR,
                &mut OsRng,
            )
            .unwrap();
        assert_eq!(
            a.receive(&again, T + 23 * HOUR).unwrap(),
            Event::Ignored(Ignored::AlreadyAnswered)
        );
    }

    #[test]
    fn duplicate_envelope_is_dropped() {
        let (mut a, mut b) = paired();
        let (id, request) = a.request(T, &mut OsRng).unwrap();
        assert_eq!(b.receive(&request, T).unwrap(), Event::Request(id));
        assert_eq!(
            b.receive(&request, T + 1).unwrap(),
            Event::Ignored(Ignored::Duplicate)
        );
    }

    #[test]
    fn delayed_request_counts_from_sending_time() {
        let (mut a, mut b) = paired();
        let (id, request) = a.request(T, &mut OsRng).unwrap();
        assert_eq!(
            b.receive(&request, T + 5 * HOUR).unwrap(),
            Event::Request(id)
        );
        assert_eq!(b.incoming.entries[0].at, T);
        assert_eq!(b.open_request(T + 24 * HOUR), None);

        // Arriving after its expiry, a request is not shown at all.
        let late = forced_request(&a, T + 30 * HOUR);
        assert_eq!(
            b.receive(&late, T + 30 * HOUR + 24 * HOUR).unwrap(),
            Event::Ignored(Ignored::Expired)
        );
    }

    #[test]
    fn ending_stops_everything() {
        let (mut a, mut b) = paired();
        let (_, request) = a.request(T, &mut OsRng).unwrap();
        let end = b.end(T, &mut OsRng).unwrap();
        assert_eq!(a.receive(&end, T).unwrap(), Event::Ended);
        assert_eq!(a.request(T, &mut OsRng).unwrap_err(), Error::Ended);
        assert_eq!(
            b.receive(&request, T).unwrap(),
            Event::Ignored(Ignored::Ended)
        );
        assert_eq!(b.end(T, &mut OsRng).unwrap_err(), Error::Ended);
    }

    #[test]
    fn rejected_pairing_is_visible_on_both_sides() {
        // The inviting side rejects after comparing the code.
        let (pending, invitation) = PendingInvitation::create(address(1), None, T, &mut OsRng);
        let (a, hello) = accept(&invitation, address(3), T, &mut OsRng).unwrap();
        let mut a = ContactState::new(a);
        let mut b = ContactState::new(pending.receive_hello(&hello, T).unwrap());
        let end = b.end(T, &mut OsRng).unwrap();
        assert_eq!(a.receive(&end, T).unwrap(), Event::Rejected);
        assert!(a.ended);

        // The accepting side rejects before `hello_ok` arrived.
        let (pending, invitation) = PendingInvitation::create(address(1), None, T, &mut OsRng);
        let (a, hello) = accept(&invitation, address(3), T, &mut OsRng).unwrap();
        let mut a = ContactState::new(a);
        let mut b = ContactState::new(pending.receive_hello(&hello, T).unwrap());
        let end = a.end(T, &mut OsRng).unwrap();
        assert_eq!(b.receive(&end, T).unwrap(), Event::Rejected);
        assert_eq!(b.confirm(T, &mut OsRng).unwrap_err(), Error::Ended);
    }

    #[test]
    fn second_acceptance_of_an_invitation_is_reported() {
        let (pending, invitation) = PendingInvitation::create(address(1), None, T, &mut OsRng);
        let (_, first) = accept(&invitation, address(3), T, &mut OsRng).unwrap();
        let (_, second) = accept(&invitation, address(5), T + 60, &mut OsRng).unwrap();
        let mut b = ContactState::new(pending.receive_hello(&first, T).unwrap());

        // The first `hello` again asks for `reveal` again.
        assert_eq!(b.receive(&first, T + 1).unwrap(), Event::HelloAgain);
        assert_eq!(b.receive(&second, T + 61).unwrap(), Event::SecondAcceptance);
        assert_eq!(
            b.receive(&second, T + 62).unwrap(),
            Event::Ignored(Ignored::Duplicate)
        );
        // Also after confirming, a further acceptance is reported.
        b.confirm(T + 70, &mut OsRng).unwrap();
        let (_, third) = accept(&invitation, address(7), T + 80, &mut OsRng).unwrap();
        assert_eq!(b.receive(&third, T + 81).unwrap(), Event::SecondAcceptance);
        // Once confirmed, n is gone and nothing is revealed anymore.
        assert_eq!(
            b.receive(&first, T + 82).unwrap(),
            Event::Ignored(Ignored::Duplicate)
        );
    }

    fn rules_message(b: &ContactState, id: u8, at: u64, rules: Rules) -> [u8; ENVELOPE_LEN] {
        let message = crate::wire::Message {
            id: [id; 16],
            sent_at: at,
            body: Body::Rules(rules),
        };
        b.contact.seal_message(&message, &mut OsRng).unwrap()
    }

    #[test]
    fn newest_rules_of_the_contact_win() {
        let (mut a, b) = paired();
        let one = Rules {
            weekly_limit: 1,
            ..Rules::DEFAULT
        };
        let two = Rules {
            weekly_limit: 2,
            ..Rules::DEFAULT
        };
        let at = T + HOUR;
        assert_eq!(
            a.receive(&rules_message(&b, 5, at, one), at).unwrap(),
            Event::Rules(one)
        );
        // Older, or the same time with a smaller identifier: ignored.
        for (id, sent) in [(9, at - 1), (4, at)] {
            assert_eq!(
                a.receive(&rules_message(&b, id, sent, two), at).unwrap(),
                Event::Ignored(Ignored::Outdated)
            );
        }
        assert_eq!(a.peer_rules, one);
        // The same time with a larger identifier wins.
        assert_eq!(
            a.receive(&rules_message(&b, 6, at, two), at).unwrap(),
            Event::Rules(two)
        );
        assert_eq!(a.peer_rules_stamp, Stamp { at, id: [6; 16] });
    }

    #[test]
    fn rule_changes_within_one_second_arrive_in_order() {
        let (mut a, mut b) = paired();
        for limit in 1..20 {
            let rules = Rules {
                weekly_limit: limit,
                ..Rules::DEFAULT
            };
            let envelope = b.set_rules(rules, T + HOUR, &mut OsRng).unwrap();
            assert_eq!(a.receive(&envelope, T + HOUR).unwrap(), Event::Rules(rules));
        }
    }

    #[test]
    fn next_stamp_is_always_newer() {
        let last = Stamp {
            at: T,
            id: [0xFF; 16],
        };
        let next = super::next_stamp(last, T, &mut OsRng);
        assert_eq!(next.at, T + 1);
        // A clock that went back still gives a newer stamp.
        assert!(super::next_stamp(last, T - 100, &mut OsRng) > last);
        assert_eq!(super::next_stamp(last, T + 5, &mut OsRng).at, T + 5);
    }

    #[test]
    fn nothing_but_hello_ok_before_pairing() {
        let (pending, invitation) = PendingInvitation::create(address(1), None, T, &mut OsRng);
        let (a, hello) = accept(&invitation, address(3), T, &mut OsRng).unwrap();
        let mut a = ContactState::new(a);
        let mut b = ContactState::new(pending.receive_hello(&hello, T).unwrap());
        assert_eq!(a.request(T, &mut OsRng).unwrap_err(), Error::Unexpected);
        let early = b.contact.seal(Body::Request, T, &mut OsRng).unwrap();
        assert_eq!(
            a.receive(&early, T).unwrap(),
            Event::Ignored(Ignored::NotExpected)
        );
        let code = b.contact.verification_code().unwrap();
        let [hello_ok, _] = b.confirm(T, &mut OsRng).unwrap();
        // `hello_ok` before `reveal` is kept back, not applied.
        assert_eq!(a.receive(&hello_ok, T).unwrap(), Event::HelloOkKept);
        assert!(!a.is_paired());
        assert_eq!(a.request(T, &mut OsRng).unwrap_err(), Error::Unexpected);
        // The `reveal` b made before confirming completes it.
        let reveal = b
            .contact
            .seal(Body::Reveal(pending.secret.unwrap()), T, &mut OsRng)
            .unwrap();
        assert_eq!(
            a.receive(&reveal, T).unwrap(),
            Event::Revealed { code, paired: true }
        );
        assert!(a.is_paired());
        assert_eq!(a.contact.secret, None);
    }

    #[test]
    fn reveal_that_does_not_match_is_reported() {
        let (pending, invitation) = PendingInvitation::create(address(1), None, T, &mut OsRng);
        let (a, hello) = accept(&invitation, address(3), T, &mut OsRng).unwrap();
        let mut a = ContactState::new(a);
        let b = ContactState::new(pending.receive_hello(&hello, T).unwrap());
        let forged = b
            .contact
            .seal(Body::Reveal([1; 32]), T, &mut OsRng)
            .unwrap();
        assert_eq!(a.receive(&forged, T).unwrap(), Event::RevealMismatch);
        assert!(!a.is_paired());
        a.end(T, &mut OsRng).unwrap();
        assert!(a.contact.awaiting_reveal.is_none());
    }

    #[test]
    fn rejecting_deletes_the_secret_value() {
        let (pending, invitation) = PendingInvitation::create(address(1), None, T, &mut OsRng);
        let (a, hello) = accept(&invitation, address(3), T, &mut OsRng).unwrap();
        let mut a = ContactState::new(a);
        let mut b = ContactState::new(pending.receive_hello(&hello, T).unwrap());
        assert!(b.contact.secret.is_some());
        let end = a.end(T, &mut OsRng).unwrap();
        assert_eq!(b.receive(&end, T).unwrap(), Event::Rejected);
        assert_eq!(b.contact.secret, None);
        assert!(!b.pairing_failed(T));
    }
}
