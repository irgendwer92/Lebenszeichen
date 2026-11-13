//! Everything that happens with a contact after pairing: rules, requests,
//! responses, ending the contact.
//!
//! [`ContactState`] holds the whole state of one contact and is the unit the
//! caller stores. Like the rest of the protocol part it takes time and
//! randomness as arguments and does no I/O.

use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::pairing::{Contact, PairingState, new_message};
use crate::rules::Ledger;
use crate::wire::{
    Body, ENVELOPE_LEN, MessageId, REPLAY_MEMORY_SECS, Reason, Response, ResponseKind, Rules,
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
}

/// A received message, remembered for [`REPLAY_MEMORY_SECS`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    #[serde(with = "crate::hex")]
    pub id: MessageId,
    /// Sending time of the message.
    pub at: u64,
}

/// What a received envelope meant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// `hello_ok` arrived. The caller should now send [`ContactState::announce_rules`].
    Paired,
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
        let hello_ok = self.contact.confirm(now, rng)?;
        Ok([hello_ok, self.announce_rules(now, rng)?])
    }

    /// The own rules as a message, sent after pairing and after each change.
    pub fn announce_rules(
        &self,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<[u8; ENVELOPE_LEN], Error> {
        self.contact.seal(Body::Rules(self.own_rules), now, rng)
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
    /// The caller then deletes the own mailbox of this contact.
    pub fn end(
        &mut self,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<[u8; ENVELOPE_LEN], Error> {
        self.check_active()?;
        let envelope = self.contact.seal(Body::End, now, rng)?;
        self.ended = true;
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
        let message = self.contact.open(envelope, now)?;

        self.seen
            .retain(|seen| now < seen.at.saturating_add(REPLAY_MEMORY_SECS));
        if self.seen.iter().any(|seen| seen.id == message.id) {
            return Ok(Event::Ignored(Ignored::Duplicate));
        }
        self.seen.push(Seen {
            id: message.id,
            at: message.sent_at,
        });

        if !self.is_paired() {
            return Ok(match self.contact.receive_hello_ok(&message) {
                Ok(()) => Event::Paired,
                Err(_) => Event::Ignored(Ignored::NotExpected),
            });
        }

        // Times from the other side are capped at the own clock.
        let at = message.sent_at.min(now);
        Ok(match message.body {
            Body::Hello(_) | Body::HelloOk => Event::Ignored(Ignored::NotExpected),
            Body::Rules(rules) => {
                self.peer_rules = rules;
                Event::Rules(rules)
            }
            Body::Request => self.accept_request(message.id, at, now),
            Body::Response(response) => self.accept_response(response, at),
            Body::End => {
                self.ended = true;
                Event::Ended
            }
            Body::Unknown(_) => Event::Ignored(Ignored::UnknownType),
        })
    }

    fn accept_request(&mut self, id: MessageId, at: u64, now: u64) -> Event {
        let rules = self.own_rules;
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
    use crate::wire::{Body, Reason, RelayUrl, ResponseKind, Rules};

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
        let [hello_ok, _] = b.confirm(T, &mut OsRng).unwrap();
        assert_eq!(a.receive(&hello_ok, T).unwrap(), Event::Paired);
    }
}
