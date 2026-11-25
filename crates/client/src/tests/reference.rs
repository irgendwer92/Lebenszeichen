//! A reference satellite (M10, `docs/m10-satellite.md`, part 2), for the
//! tests of the full-device side and as the executable model the C library
//! is checked against.
//!
//! [`SatState`] is the satellite itself without I/O: what it keeps in
//! flash, what it does with every message, button and tick. It takes time
//! and randomness as arguments, like `lz-core`, so that it can produce
//! test vectors ([`vectors`]). [`RefSatellite`] drives it against the fake
//! relay: own tokens, subscriptions, outbox, wiping itself when its channel
//! is gone.
//!
//! The protocol rules come from `lz-core` (`ContactState`: envelopes,
//! direction marker, replay protection per contact, rules for requests on
//! both sides); what is specific to the satellite is written out here.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lz_core::contact::{ContactState, Event, Seen};
use lz_core::crypto::{self, ContactKey, GroupKey};
use lz_core::pairing::{Address, Contact, PairingState};
use lz_core::rules::Entry;
use lz_core::store::ReportedState;
use lz_core::wire::satellite::{ContactRecord, SetupHeader};
use lz_core::wire::{
    Body, ContactFlags, ContactPart, MAX_CLOCK_AHEAD_SECS, MAX_MESSAGE_AGE_SECS, MailboxId,
    Message, MessageId, REPLAY_MEMORY_SECS, ReaderId, Reason, RelayUrl, Requested, Responded,
    ResponseKind, SYNC_KEEP_SECS, SatelliteSettings, Stamp, StateShared, Token, record,
};
use rand_core::{CryptoRng, CryptoRngCore, OsRng, RngCore};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::relay::fake::FakeRelay;
use crate::relay::{ReaderChange, Relay, RelayError, ServerFrame};

/// How long the answer to an own request stays on the display (decision
/// 13).
pub const ANSWER_SHOWN_SECS: u64 = 24 * 60 * 60;

/// A stamp in the stored state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct St(pub u64, pub MessageId);

impl St {
    fn of(message: &Message) -> Self {
        Self(message.sent_at, message.id)
    }

    fn stamp(self) -> Stamp {
        Stamp {
            at: self.0,
            id: self.1,
        }
    }

    fn from(stamp: Stamp) -> Self {
        Self(stamp.at, stamp.id)
    }
}

/// The channel, as the satellite keeps it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Channel {
    pub relay: RelayUrl,
    pub mailbox: MailboxId,
    pub enrolment: Token,
    pub own_token: Option<Token>,
    pub new_token: Option<Token>,
    pub send_token: Token,
    pub key: GroupKey,
    pub seen: Vec<Seen>,
}

/// A contact on the satellite.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SatContact {
    pub own_mailbox: MailboxId,
    pub own_relay: RelayUrl,
    pub enrolment: Token,
    pub own_token: Option<Token>,
    pub new_token: Option<Token>,
    pub name: String,
    pub state: ContactState,
    pub flags: ContactFlags,
    pub flags_stamp: St,
}

impl SatContact {
    fn token(&self) -> Token {
        self.own_token.unwrap_or(self.enrolment)
    }
}

/// Where an outgoing envelope goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    /// The mailbox of the contact with this own mailbox.
    Peer(MailboxId),
    /// The channel, with the send token current at sending.
    Channel,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Out {
    pub target: Target,
    pub envelope: Vec<u8>,
    pub created_at: u64,
    /// Type of the message, for tests and vectors.
    pub kind: u8,
}

/// The answer to an own request, shown until the next press or 24 hours.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub contact: MailboxId,
    /// The reason of a reported answer; `None` for a manual one.
    pub reported: Option<Reason>,
    pub at: u64,
    pub shown_until: u64,
}

/// A part of `home_contact_part` received so far.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Parts {
    pub contact: MailboxId,
    pub transfer: [u8; 16],
    pub count: u8,
    pub parts: Vec<(u8, Vec<u8>)>,
    pub first_seen: u64,
    pub discarded: bool,
}

/// Everything the satellite keeps in flash.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SatState {
    pub reader: ReaderId,
    pub channel: Channel,
    pub settings: SatelliteSettings,
    pub settings_stamp: St,
    pub contacts: Vec<SatContact>,
    /// The own state (long press, sensor).
    pub own_state: Option<ReportedState>,
    /// The state handed down by a full device.
    pub handed: Option<ReportedState>,
    pub share_stamp: St,
    pub transfers: Vec<Parts>,
    /// Down messages about contacts not known yet, with their arrival.
    pub parked: Vec<(Vec<u8>, u64)>,
    pub outbox: Vec<Out>,
    pub answers: Vec<Answer>,
}

/// What the satellite did with something, for tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Did {
    /// A request is shown.
    Request(String),
    /// An answer to an own request is shown.
    Answer(String, ResponseKind),
    /// A contact arrived.
    ContactAdded(String),
    /// A contact was updated by a record again.
    ContactUpdated(String),
    /// A contact ended.
    Ended(String),
    /// Applied a down message of this type.
    Applied(u8),
    /// Dropped a message: not for the satellite, a replay, or unreadable.
    Dropped(&'static str),
    /// Parked a down message about an unknown contact.
    Parked,
}

/// Rejects messages outside the window against replay, as everywhere.
fn in_window(message: &Message, now: u64) -> bool {
    message.sent_at.saturating_add(MAX_MESSAGE_AGE_SECS) >= now
        && message.sent_at <= now.saturating_add(MAX_CLOCK_AHEAD_SECS)
}

/// Remembers an identifier; `true` if it was seen before.
fn remember(seen: &mut Vec<Seen>, message: &Message, now: u64) -> bool {
    seen.retain(|s| now < s.at.saturating_add(REPLAY_MEMORY_SECS));
    if seen.iter().any(|s| s.id == message.id) {
        return true;
    }
    seen.push(Seen {
        id: message.id,
        at: message.sent_at,
    });
    false
}

/// A newer stamp than `last`: now with a random identifier, a second
/// later if that would not be newer.
fn next_stamp(last: St, now: u64, rng: &mut impl CryptoRngCore) -> St {
    let mut id = [0; 16];
    rng.fill_bytes(&mut id);
    let mut stamp = St(now.max(last.0), id);
    if stamp <= last {
        stamp.0 = last.0.saturating_add(1);
    }
    stamp
}

/// A contact from its satellite contact record.
fn contact_from(own_mailbox: MailboxId, record: ContactRecord) -> SatContact {
    let mut state = ContactState::new(Contact {
        key: ContactKey::from_bytes(record.secret_key),
        peer_key: record.peer_key,
        peer: Address {
            relay: record.peer_relay,
            mailbox: record.peer_mailbox,
            send_token: record.peer_send_token,
        },
        own_mailbox,
        state: PairingState::Paired,
        secret: None,
        awaiting_reveal: None,
    });
    state.own_rules = record.own_rules;
    state.own_rules_stamp = record.own_rules_stamp;
    state.peer_rules = record.peer_rules;
    state.peer_rules_stamp = record.peer_rules_stamp;
    // The own requests: counted ones without identifier, and the open one.
    let mut times = record.recent.as_slice().to_vec();
    if let Some(open) = record.open_request {
        if let Some(pos) = times.iter().position(|&at| at == open.at) {
            times.remove(pos);
        }
        state.outgoing.record(open.id, open.at);
    }
    for at in times {
        state.outgoing.insert(Entry {
            id: [0; 16],
            at,
            answered_at: Some(at),
            reported: None,
        });
    }
    state.outgoing.last_answer = record.last_answer;
    SatContact {
        own_mailbox,
        own_relay: record.own_relay,
        enrolment: record.enrolment_token,
        own_token: None,
        new_token: None,
        name: record.name.as_str().to_owned(),
        state,
        flags: record.settings,
        flags_stamp: St::default(),
    }
}

impl SatState {
    /// The state from a setup.
    pub fn install(setup: &[u8]) -> Result<Self, lz_core::wire::Error> {
        let (header, entries) = SetupHeader::from_bytes(setup)?;
        let mut contacts = Vec::new();
        for entry in entries {
            let (mailbox, record) = entry?;
            contacts.push(contact_from(mailbox, record));
        }
        let SetupHeader {
            reader,
            relay,
            mailbox,
            enrolment_token,
            send_token,
            key,
            settings_stamp,
            settings,
            ..
        } = header;
        Ok(Self {
            reader,
            channel: Channel {
                relay,
                mailbox,
                enrolment: enrolment_token,
                own_token: None,
                new_token: None,
                send_token,
                key: GroupKey::from_bytes(key),
                seen: Vec::new(),
            },
            settings,
            settings_stamp: St::from(settings_stamp),
            contacts,
            own_state: None,
            handed: None,
            share_stamp: St::default(),
            transfers: Vec::new(),
            parked: Vec::new(),
            outbox: Vec::new(),
            answers: Vec::new(),
        })
    }

    pub fn contact(&self, name: &str) -> Option<&SatContact> {
        self.contacts.iter().find(|c| c.name == name)
    }

    fn index_of(&self, mailbox: &MailboxId) -> Option<usize> {
        self.contacts.iter().position(|c| &c.own_mailbox == mailbox)
    }

    /// Names of the contacts whose request is open: the light is on.
    pub fn open_requests(&self, now: u64) -> Vec<String> {
        self.contacts
            .iter()
            .filter(|c| !c.state.ended && c.state.open_request(now).is_some())
            .map(|c| c.name.clone())
            .collect()
    }

    /// The answers to own requests on the display.
    pub fn shown_answers(&self, now: u64) -> Vec<(String, ResponseKind)> {
        self.answers
            .iter()
            .filter(|a| now < a.shown_until)
            .filter_map(|a| {
                let name = self.contacts.iter().find(|c| c.own_mailbox == a.contact)?;
                let kind = match a.reported {
                    Some(reason) => ResponseKind::Reported(reason),
                    None => ResponseKind::Manual,
                };
                Some((name.name.clone(), kind))
            })
            .collect()
    }

    /// The state that applies: of the own and the handed one, the valid
    /// one set last.
    pub fn current_state(&self, now: u64) -> Option<ReportedState> {
        let valid = |s: &Option<ReportedState>| s.filter(|s| now < s.until);
        match (valid(&self.own_state), valid(&self.handed)) {
            (Some(own), Some(handed)) => Some(if handed.since > own.since {
                handed
            } else {
                own
            }),
            (own, handed) => own.or(handed),
        }
    }

    fn queue_peer(&mut self, contact: MailboxId, envelope: [u8; 297], kind: u8, now: u64) {
        self.outbox.push(Out {
            target: Target::Peer(contact),
            envelope: envelope.to_vec(),
            created_at: now,
            kind,
        });
    }

    fn queue_up(&mut self, message: &Message, now: u64, rng: &mut impl CryptoRngCore) {
        debug_assert!(message.body.is_up());
        let envelope = crypto::seal_group(message, &self.channel.key, rng).expect("valid key");
        self.outbox.push(Out {
            target: Target::Channel,
            envelope: envelope.to_vec(),
            created_at: now,
            kind: message.body.kind().expect("known"),
        });
    }

    fn up(&mut self, body: Body, now: u64, rng: &mut impl CryptoRngCore) {
        let mut id = [0; 16];
        rng.fill_bytes(&mut id);
        let message = Message {
            id,
            sent_at: now,
            body,
        };
        self.queue_up(&message, now, rng);
    }

    /// An envelope from an own mailbox: a contact's or the channel.
    pub fn receive(&mut self, mailbox: &MailboxId, envelope: &[u8], now: u64) -> Vec<Did> {
        if *mailbox == self.channel.mailbox {
            return self.receive_channel(envelope, now);
        }
        let Some(i) = self.index_of(mailbox) else {
            return vec![Did::Dropped("unknown mailbox")];
        };
        let contact = &mut self.contacts[i];
        let name = contact.name.clone();
        match contact.state.receive(envelope, now) {
            Ok(Event::Request(_)) => vec![Did::Request(name)],
            Ok(Event::Response { kind, at, .. }) => {
                self.answers.retain(|a| &a.contact != mailbox);
                let reported = match &kind {
                    ResponseKind::Reported(reason) => Some(*reason),
                    ResponseKind::Manual => None,
                };
                self.answers.push(Answer {
                    contact: *mailbox,
                    reported,
                    at,
                    shown_until: now + ANSWER_SHOWN_SECS,
                });
                vec![Did::Answer(name, kind)]
            }
            Ok(Event::Ended) => vec![Did::Ended(name)],
            Ok(Event::Rules(_)) => vec![Did::Applied(3)],
            Ok(_) => vec![Did::Dropped("not for the satellite")],
            Err(_) => vec![Did::Dropped("unreadable")],
        }
    }

    /// An envelope from the channel. Only down types are taken.
    pub fn receive_channel(&mut self, envelope: &[u8], now: u64) -> Vec<Did> {
        self.expire(now);
        let Ok(message) = crypto::open_group(envelope, &self.channel.key) else {
            return vec![Did::Dropped("unreadable")];
        };
        if !in_window(&message, now) {
            return vec![Did::Dropped("out of time")];
        }
        if !message.body.is_down() {
            return vec![Did::Dropped("not a down type")];
        }
        if remember(&mut self.channel.seen, &message, now) {
            return vec![Did::Dropped("replay")];
        }
        self.apply_down(message, Some(envelope.to_vec()), now)
    }

    fn apply_down(&mut self, message: Message, envelope: Option<Vec<u8>>, now: u64) -> Vec<Did> {
        let stamp = St::of(&message);
        let kind = message.body.kind().expect("known");
        if let Some(contact) = message.body.channel_contact()
            && !matches!(message.body, Body::HomeContactPart(_))
            && self.index_of(contact).is_none()
        {
            if let Some(envelope) = envelope {
                self.parked.push((envelope, now));
            }
            return vec![Did::Parked];
        }
        match message.body {
            Body::HomeContactPart(part) => return self.apply_part(part, now),
            Body::HomeResponded(r) => {
                let i = self.index_of(&r.contact).expect("checked");
                self.contacts[i]
                    .state
                    .apply_responded(&r.request, &r.kind, r.at.min(now));
            }
            Body::HomeRequested(r) => {
                let i = self.index_of(&r.contact).expect("checked");
                self.contacts[i]
                    .state
                    .apply_requested(r.request, r.at.min(now), now);
            }
            Body::HomeRules(r) => {
                let i = self.index_of(&r.contact).expect("checked");
                if !self.contacts[i]
                    .state
                    .apply_own_rules(r.rules, stamp.stamp())
                {
                    return vec![Did::Dropped("outdated")];
                }
            }
            Body::HomeContactSettings(s) => {
                let i = self.index_of(&s.contact).expect("checked");
                let contact = &mut self.contacts[i];
                if stamp <= contact.flags_stamp {
                    return vec![Did::Dropped("outdated")];
                }
                contact.flags = s.of;
                contact.flags_stamp = stamp;
            }
            Body::HomeSettings(settings) => {
                if stamp <= self.settings_stamp {
                    return vec![Did::Dropped("outdated")];
                }
                self.settings = settings;
                self.settings_stamp = stamp;
            }
            Body::HomeStateShared(shared) => {
                if !self.apply_share(Some(shared), stamp) {
                    return vec![Did::Dropped("outdated")];
                }
            }
            Body::HomeStateCleared => {
                if !self.apply_share(None, stamp) {
                    return vec![Did::Dropped("outdated")];
                }
            }
            // The last one received wins: the relay keeps the order, and
            // only the device that replaced the token on the relay can
            // still send on the channel.
            Body::HomeSendToken(token) => self.channel.send_token = token,
            _ => return vec![Did::Dropped("not a down type")],
        }
        vec![Did::Applied(kind)]
    }

    /// `home_state_shared` and `home_state_cleared`, as M8c.
    fn apply_share(&mut self, shared: Option<StateShared>, stamp: St) -> bool {
        if stamp <= self.share_stamp {
            return false;
        }
        self.share_stamp = stamp;
        if self.own_state.is_some_and(|s| s.shared) {
            self.own_state = None;
        }
        self.handed = shared.map(|s| ReportedState {
            reason: s.reason,
            until: s.until,
            since: stamp.0,
            shared: false,
        });
        true
    }

    fn apply_part(&mut self, part: ContactPart, now: u64) -> Vec<Did> {
        let pos = match self
            .transfers
            .iter()
            .position(|t| t.contact == part.contact && t.transfer == part.transfer)
        {
            Some(pos) => pos,
            None => {
                self.transfers.push(Parts {
                    contact: part.contact,
                    transfer: part.transfer,
                    count: part.count,
                    parts: Vec::new(),
                    first_seen: now,
                    discarded: false,
                });
                self.transfers.len() - 1
            }
        };
        let entry = &mut self.transfers[pos];
        let data = part.data.as_bytes();
        let same = entry.parts.iter().find(|(i, _)| *i == part.index);
        if entry.discarded
            || part.count != entry.count
            || part.index >= part.count
            || same.is_some_and(|(_, d)| d != data)
        {
            entry.discarded = true;
            entry.parts.clear();
            return vec![Did::Dropped("inconsistent transfer")];
        }
        if same.is_none() {
            entry.parts.push((part.index, data.to_vec()));
            entry.parts.sort_by_key(|(i, _)| *i);
        }
        if entry.parts.len() < usize::from(entry.count) {
            return vec![];
        }
        let bytes: Vec<u8> = entry.parts.iter().flat_map(|(_, d)| d.clone()).collect();
        self.transfers.remove(pos);
        let Ok(record) = ContactRecord::from_bytes(&bytes) else {
            return vec![Did::Dropped("unreadable record")];
        };
        let mut did = Vec::new();
        match self.index_of(&part.contact) {
            // Known: the name, and rules whose stamp is newer; the settings
            // in the record carry no stamp and stay.
            Some(i) => {
                let contact = &mut self.contacts[i];
                contact.name = record.name.as_str().to_owned();
                contact
                    .state
                    .apply_own_rules(record.own_rules, record.own_rules_stamp);
                if record.peer_rules_stamp > contact.state.peer_rules_stamp {
                    contact.state.peer_rules = record.peer_rules;
                    contact.state.peer_rules_stamp = record.peer_rules_stamp;
                }
                did.push(Did::ContactUpdated(contact.name.clone()));
            }
            None => {
                let contact = contact_from(part.contact, record);
                did.push(Did::ContactAdded(contact.name.clone()));
                self.contacts.push(contact);
                // Down messages that waited for it.
                for (envelope, at) in std::mem::take(&mut self.parked) {
                    let Ok(message) = crypto::open_group(&envelope, &self.channel.key) else {
                        continue;
                    };
                    if message.body.channel_contact() == Some(&part.contact) {
                        did.extend(self.apply_down(message, None, now));
                    } else {
                        self.parked.push((envelope, at));
                    }
                }
            }
        }
        did
    }

    /// The answer button: answers every open request manually, and
    /// dismisses the shown answers. Returns how many it answered.
    pub fn press(&mut self, now: u64, rng: &mut impl CryptoRngCore) -> usize {
        self.answers.clear();
        let mut answered = 0;
        for i in 0..self.contacts.len() {
            let contact = &mut self.contacts[i];
            let Some(request) = contact.state.open_request(now) else {
                continue;
            };
            let Ok(envelope) = contact.state.respond(&request, now, rng) else {
                continue;
            };
            let mailbox = contact.own_mailbox;
            self.queue_peer(mailbox, envelope, 5, now);
            self.up(
                Body::SatAnswered(Responded {
                    contact: mailbox,
                    request,
                    kind: ResponseKind::Manual,
                    at: now,
                }),
                now,
                rng,
            );
            answered += 1;
        }
        answered
    }

    /// The long press: sets the preset as the own state, sharing it if the
    /// preset says so; ends the own state if one is set.
    pub fn long_press(&mut self, now: u64, rng: &mut impl CryptoRngCore) {
        if let Some(state) = self.own_state.filter(|s| now < s.until) {
            self.own_state = None;
            if state.shared {
                self.share_stamp = next_stamp(self.share_stamp, now, rng);
                let message = Message {
                    id: self.share_stamp.1,
                    sent_at: self.share_stamp.0,
                    body: Body::SatStateCleared,
                };
                self.handed = None;
                self.queue_up(&message, now, rng);
            }
            return;
        }
        let Some(preset) = self.settings.preset else {
            return;
        };
        let until = now + u64::from(preset.minutes) * 60;
        self.own_state = Some(ReportedState {
            reason: preset.reason,
            until,
            since: now,
            shared: preset.share,
        });
        if preset.share {
            self.share_stamp = next_stamp(self.share_stamp, now, rng);
            let message = Message {
                id: self.share_stamp.1,
                sent_at: self.share_stamp.0,
                body: Body::SatStateShared(StateShared {
                    reason: preset.reason,
                    until,
                }),
            };
            self.handed = None;
            self.queue_up(&message, now, rng);
        }
    }

    /// An ask button: asks the contact bound to it if the contact's rules
    /// allow it, else returns when they will.
    pub fn ask(&mut self, button: u8, now: u64, rng: &mut impl CryptoRngCore) -> Result<(), u64> {
        let Some(i) = self
            .contacts
            .iter()
            .position(|c| button != 0 && c.flags.ask_button == button && !c.state.ended)
        else {
            return Err(u64::MAX);
        };
        let contact = &mut self.contacts[i];
        match contact.state.request(now, rng) {
            Ok((request, envelope)) => {
                let mailbox = contact.own_mailbox;
                self.queue_peer(mailbox, envelope, 4, now);
                self.up(
                    Body::SatAsked(Requested {
                        contact: mailbox,
                        request,
                        at: now,
                    }),
                    now,
                    rng,
                );
                Ok(())
            }
            Err(lz_core::Error::NotYet(at)) => Err(at),
            Err(_) => Err(u64::MAX),
        }
    }

    /// When the reported answer to the open request of contact `i` is due,
    /// if one may be sent at all: reported answers allowed for it, none
    /// sent yet, here or elsewhere. The waiting time runs from the time of
    /// the request, so it survives a restart.
    fn report_due(&self, i: usize, now: u64) -> Option<(MessageId, u64)> {
        let contact = &self.contacts[i];
        if !contact.flags.reported || contact.state.ended {
            return None;
        }
        let state = &contact.state;
        let entry = state.incoming.open(&state.own_rules, now)?;
        if entry.reported.is_some() {
            return None;
        }
        let due = entry
            .at
            .saturating_add(u64::from(self.settings.waiting_minutes) * 60);
        Some((entry.id, due))
    }

    /// Sends the reported answers that are due, once per request, and
    /// forgets what is too old.
    pub fn tick(&mut self, now: u64, rng: &mut impl CryptoRngCore) -> usize {
        self.expire(now);
        let Some(state) = self.current_state(now) else {
            return 0;
        };
        let mut sent = 0;
        for i in 0..self.contacts.len() {
            let Some((request, due)) = self.report_due(i, now) else {
                continue;
            };
            if due > now {
                continue;
            }
            let contact = &mut self.contacts[i];
            let Ok(envelope) = contact.state.report(&request, state.reason, now, rng) else {
                continue;
            };
            let mailbox = contact.own_mailbox;
            self.queue_peer(mailbox, envelope, 5, now);
            self.up(
                Body::SatAnswered(Responded {
                    contact: mailbox,
                    request,
                    kind: ResponseKind::Reported(state.reason),
                    at: now,
                }),
                now,
                rng,
            );
            sent += 1;
        }
        sent
    }

    fn expire(&mut self, now: u64) {
        let fresh = |since: u64| now < since.saturating_add(SYNC_KEEP_SECS);
        self.outbox.retain(|o| fresh(o.created_at));
        self.parked.retain(|(_, at)| fresh(*at));
        self.transfers.retain(|t| fresh(t.first_seen));
        self.answers.retain(|a| now < a.shown_until);
    }

    /// Every own mailbox with the token to read it: the channel first.
    pub fn mailboxes(&self) -> Vec<(RelayUrl, MailboxId, Token)> {
        let channel = &self.channel;
        let mut list = vec![(
            channel.relay,
            channel.mailbox,
            channel.own_token.unwrap_or(channel.enrolment),
        )];
        list.extend(
            self.contacts
                .iter()
                .filter(|c| !c.state.ended)
                .map(|c| (c.own_relay, c.own_mailbox, c.token())),
        );
        list
    }
}

/// The reference satellite against the fake relay.
pub struct RefSatellite {
    /// `None` once it wiped itself.
    pub state: Option<SatState>,
    relay: FakeRelay,
    clock: Arc<AtomicU64>,
    /// What it did in the last poll.
    pub log: Vec<Did>,
}

impl RefSatellite {
    /// Installs a setup and replaces the enrolment tokens.
    pub async fn install(relay: &FakeRelay, clock: &Arc<AtomicU64>, setup: &[u8]) -> Self {
        let mut satellite = Self {
            state: Some(SatState::install(setup).unwrap()),
            relay: relay.clone(),
            clock: clock.clone(),
            log: Vec::new(),
        };
        satellite.take_own_tokens().await;
        satellite
    }

    fn now(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }

    pub fn state(&self) -> &SatState {
        self.state.as_ref().expect("not wiped")
    }

    pub fn state_mut(&mut self) -> &mut SatState {
        self.state.as_mut().expect("not wiped")
    }

    pub fn wiped(&self) -> bool {
        self.state.is_none()
    }

    /// A restart: only what is in flash survives.
    pub fn restart(&mut self) {
        if let Some(state) = &self.state {
            let flash = serde_json::to_vec(state).unwrap();
            self.state = Some(serde_json::from_slice(&flash).unwrap());
        }
    }

    /// Replaces every enrolment token with an own one, stored before it
    /// goes out.
    async fn take_own_tokens(&mut self) {
        let Some(state) = &mut self.state else { return };
        let reader = state.reader;
        let mut work = Vec::new();
        if state.channel.own_token.is_none() {
            let new = *state.channel.new_token.get_or_insert_with(random);
            work.push((
                None,
                state.channel.relay,
                state.channel.mailbox,
                state.channel.enrolment,
                new,
            ));
        }
        for (i, c) in state.contacts.iter_mut().enumerate() {
            if c.own_token.is_none() && !c.state.ended {
                let new = *c.new_token.get_or_insert_with(random);
                work.push((Some(i), c.own_relay, c.own_mailbox, c.enrolment, new));
            }
        }
        for (index, relay, mailbox, enrolment, new) in work {
            let change = ReaderChange {
                push_url: None,
                token: Some(&new),
                role: None,
            };
            let mut result = self
                .relay
                .put_reader(&relay, &mailbox, &enrolment, &reader, change)
                .await;
            if matches!(result, Err(RelayError::Refused { status: 401, .. })) {
                result = self
                    .relay
                    .put_reader(&relay, &mailbox, &new, &reader, change)
                    .await;
            }
            if result.is_ok() {
                let state = self.state.as_mut().unwrap();
                match index {
                    None => {
                        state.channel.own_token = Some(new);
                        state.channel.new_token = None;
                    }
                    Some(i) => {
                        let contact = &mut state.contacts[i];
                        contact.own_token = Some(new);
                        contact.new_token = None;
                    }
                }
            }
        }
    }

    /// Delivers the outbox in order; what fails stays.
    async fn deliver(&mut self) {
        let Some(state) = &self.state else { return };
        let outbox = state.outbox.clone();
        let channel = (
            state.channel.relay,
            state.channel.mailbox,
            state.channel.send_token,
        );
        let peers: HashMap<MailboxId, Address> = state
            .contacts
            .iter()
            .map(|c| (c.own_mailbox, c.state.contact.peer.clone()))
            .collect();
        let mut delivered = Vec::new();
        for (i, out) in outbox.iter().enumerate() {
            let result = match out.target {
                Target::Channel => {
                    self.relay
                        .deliver(&channel.0, &channel.1, &channel.2, &out.envelope)
                        .await
                }
                Target::Peer(contact) => match peers.get(&contact) {
                    Some(peer) => {
                        self.relay
                            .deliver(&peer.relay, &peer.mailbox, &peer.send_token, &out.envelope)
                            .await
                    }
                    None => Ok(()),
                },
            };
            if result.is_ok() {
                delivered.push(i);
            }
        }
        let state = self.state.as_mut().unwrap();
        let mut i = 0;
        state.outbox.retain(|_| {
            let keep = !delivered.contains(&i);
            i += 1;
            keep
        });
    }

    /// One connection: own tokens, outbox, everything waiting, the timers,
    /// the outbox again. A channel that is gone, or no longer knows the
    /// satellite, wipes it.
    pub async fn poll(&mut self) -> Vec<Did> {
        self.log.clear();
        for _ in 0..3 {
            if self.state.is_none() {
                break;
            }
            self.take_own_tokens().await;
            self.deliver().await;
            let before = self.state().contacts.len();
            self.read_all().await;
            if self
                .state
                .as_ref()
                .is_none_or(|s| s.contacts.len() == before)
            {
                break;
            }
        }
        if let Some(state) = &mut self.state {
            let now = self.clock.load(Ordering::SeqCst);
            state.tick(now, &mut OsRng);
        }
        self.deliver().await;
        self.log.clone()
    }

    async fn read_all(&mut self) {
        let Some(state) = &self.state else { return };
        let reader = state.reader;
        let channel = state.channel.mailbox;
        let mut by_relay: HashMap<RelayUrl, Vec<(MailboxId, Token)>> = HashMap::new();
        for (relay, mailbox, token) in state.mailboxes() {
            by_relay.entry(relay).or_default().push((mailbox, token));
        }
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut connections = HashMap::new();
        for (relay, mailboxes) in by_relay {
            let Ok(mut connection) = self.relay.connect(&relay, tx.clone()).await else {
                continue;
            };
            for (mailbox, token) in mailboxes {
                let _ = connection.subscribe(&mailbox, &token, &reader);
            }
            connections.insert(relay, connection);
        }
        while let Ok(delivery) = rx.try_recv() {
            let now = self.now();
            match delivery.frame {
                ServerFrame::Message { mailbox, seq, data } => {
                    let Some(state) = &mut self.state else { break };
                    let did = state.receive(&mailbox, &data, now);
                    self.log.extend(did);
                    if let Some(c) = connections.get_mut(&delivery.relay) {
                        let _ = c.ack(&mailbox, seq);
                    }
                }
                ServerFrame::Error {
                    mailbox: Some(mailbox),
                    code,
                } => {
                    if mailbox == channel && (code == "unknown_reader" || code == "unknown_mailbox")
                    {
                        // Removed: erase everything.
                        self.state = None;
                        break;
                    }
                    if code == "unknown_mailbox"
                        && let Some(state) = &mut self.state
                    {
                        state.contacts.retain(|c| c.own_mailbox != mailbox);
                    }
                }
                _ => {}
            }
        }
    }

    /// Presses a button of the satellite now.
    pub fn press(&mut self) -> usize {
        let now = self.now();
        self.state_mut().press(now, &mut OsRng)
    }

    pub fn long_press(&mut self) {
        let now = self.now();
        self.state_mut().long_press(now, &mut OsRng);
    }

    pub fn ask(&mut self, button: u8) -> Result<(), u64> {
        let now = self.now();
        self.state_mut().ask(button, now, &mut OsRng)
    }
}

fn random() -> Token {
    let mut token = [0; 32];
    OsRng.fill_bytes(&mut token);
    token
}

/// A reproducible source of randomness for test vectors: SHA-256 of a seed
/// and a counter.
pub struct VectorRng {
    seed: [u8; 32],
    counter: u64,
    buffer: Vec<u8>,
}

impl VectorRng {
    pub fn new(seed: &[u8]) -> Self {
        Self {
            seed: crypto::sha256(&[b"lebenszeichen-test-vectors", seed]),
            counter: 0,
            buffer: Vec::new(),
        }
    }
}

impl RngCore for VectorRng {
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
        for byte in dest {
            if self.buffer.is_empty() {
                self.counter += 1;
                self.buffer = crypto::sha256(&[&self.seed, &self.counter.to_be_bytes()]).to_vec();
            }
            *byte = self.buffer.remove(0);
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl CryptoRng for VectorRng {}

fn hex(bytes: &[u8]) -> String {
    crate::relay::hex(bytes)
}

/// Test vectors for the C library (decision 16), reproducible from the
/// seed: channel envelopes of every up and down type, the records, a setup,
/// and cases of the rules on the satellite. Written by the test
/// `vectors_are_reproducible` to the file in `LZ_VECTORS_OUT`, if set.
pub fn vectors(seed: &[u8]) -> serde_json::Value {
    use lz_core::wire::satellite::{OpenRequest, Recent};
    use lz_core::wire::{ContactName, ContactSettings, PADDED_PLAINTEXT_LEN, Preset, RulesSet};
    use serde_json::json;

    let mut rng = VectorRng::new(seed);
    let mut key = [0; 32];
    rng.fill_bytes(&mut key);
    let channel_key = GroupKey::from_bytes(key);
    let reason = Reason::new("asleep").unwrap();
    let contact = [0x11; 32];
    let request = [0x22; 16];
    let t = 1_794_600_000;
    let responded = |kind| Responded {
        contact,
        request,
        kind,
        at: t,
    };
    let requested = Requested {
        contact,
        request,
        at: t,
    };
    let state = StateShared {
        reason,
        until: t + 8 * 3600,
    };
    let settings = SatelliteSettings {
        waiting_minutes: 15,
        preset: Some(Preset {
            minutes: 480,
            share: true,
            reason,
        }),
    };
    let record = ContactRecord {
        secret_key: [0x31; 32],
        peer_key: ContactKey::from_bytes([0x32; 32]).public_key(),
        enrolment_token: [0x33; 32],
        own_relay: RelayUrl::new("https://relay.example").unwrap(),
        peer_mailbox: [0x34; 32],
        peer_send_token: [0x35; 32],
        peer_relay: RelayUrl::new("https://other.example").unwrap(),
        own_rules: lz_core::wire::Rules::DEFAULT,
        own_rules_stamp: Stamp {
            at: t - 100,
            id: [0x36; 16],
        },
        peer_rules: lz_core::wire::Rules::DEFAULT,
        peer_rules_stamp: Stamp::default(),
        settings: ContactFlags {
            reported: true,
            ask_button: 1,
        },
        open_request: Some(OpenRequest {
            id: [0x37; 16],
            at: t - 50,
        }),
        last_answer: Some(t - 7 * 3600),
        recent: Recent::newest(&[t - 7 * 3600 - 60, t - 50]),
        name: ContactName::new("Mum").unwrap(),
    };
    let mut record_bytes = [0; ContactRecord::MAX_LEN];
    let record_len = record.to_bytes(&mut record_bytes);
    let parts: Vec<ContactPart> = record::split(&record_bytes[..record_len])
        .enumerate()
        .map(|(i, data)| ContactPart {
            contact,
            transfer: [0x44; 16],
            index: i as u8,
            count: record_len.div_ceil(lz_core::wire::MAX_PART_LEN) as u8,
            data: lz_core::wire::PartData::new(data).unwrap(),
        })
        .collect();

    let mut bodies = vec![
        (
            "sat_answered manual",
            Body::SatAnswered(responded(ResponseKind::Manual)),
        ),
        (
            "sat_answered reported",
            Body::SatAnswered(responded(ResponseKind::Reported(reason))),
        ),
        ("sat_asked", Body::SatAsked(requested.clone())),
        ("sat_state_shared", Body::SatStateShared(state.clone())),
        ("sat_state_cleared", Body::SatStateCleared),
        (
            "home_responded manual",
            Body::HomeResponded(responded(ResponseKind::Manual)),
        ),
        ("home_requested", Body::HomeRequested(requested)),
        (
            "home_rules",
            Body::HomeRules(RulesSet {
                contact,
                rules: lz_core::wire::Rules::DEFAULT,
            }),
        ),
        (
            "home_contact_settings",
            Body::HomeContactSettings(ContactSettings {
                contact,
                of: ContactFlags {
                    reported: true,
                    ask_button: 1,
                },
            }),
        ),
        ("home_settings", Body::HomeSettings(settings)),
        ("home_state_shared", Body::HomeStateShared(state)),
        ("home_state_cleared", Body::HomeStateCleared),
        ("home_send_token", Body::HomeSendToken([0x55; 32])),
    ];
    for part in parts {
        bodies.push(("home_contact_part", Body::HomeContactPart(part)));
    }
    let channel: Vec<_> = bodies
        .into_iter()
        .map(|(name, body)| {
            let mut id = [0; 16];
            rng.fill_bytes(&mut id);
            let message = Message {
                id,
                sent_at: t,
                body,
            };
            let mut plaintext = [0; PADDED_PLAINTEXT_LEN];
            message.encode(&mut plaintext).unwrap();
            let envelope = crypto::seal_group(&message, &channel_key, &mut rng).unwrap();
            json!({
                "name": name,
                "type": message.body.kind().unwrap(),
                "direction": if message.body.is_up() { "up" } else { "down" },
                "plaintext": hex(&plaintext),
                "envelope": hex(&envelope),
            })
        })
        .collect();

    // A setup with the record, and what the satellite makes of it.
    let header = SetupHeader {
        reader: [0x66; 16],
        wifi_name: lz_core::wire::satellite::WifiName::new("home").unwrap(),
        wifi_password: lz_core::wire::satellite::WifiPassword::new("secret").unwrap(),
        relay: RelayUrl::new("https://relay.example").unwrap(),
        mailbox: [0x67; 32],
        enrolment_token: [0x68; 32],
        send_token: [0x69; 32],
        key,
        settings_stamp: Stamp::default(),
        settings: SatelliteSettings::DEFAULT,
        contacts: 1,
    };
    let mut head = [0; SetupHeader::MAX_LEN];
    let len = header.to_bytes(&mut head);
    let mut setup = head[..len].to_vec();
    let mut entry = [0; lz_core::wire::satellite::MAX_ENTRY_LEN];
    let n = lz_core::wire::satellite::write_entry(&contact, &record, &mut entry);
    setup.extend_from_slice(&entry[..n]);
    let installed = SatState::install(&setup).unwrap();
    let mum = &installed.contacts[0].state;

    // Rule cases on the satellite: when it may ask next, and when a
    // reported answer is due.
    let asking: Vec<_> = [t, t + 3600, t - 50 + 24 * 3600, t + 2 * 24 * 3600]
        .into_iter()
        .map(|now| json!({"now": now, "next_request_at": mum.next_request_at(now)}))
        .collect();

    json!({
        "seed": hex(seed),
        "channel_key": hex(&key),
        "channel": channel,
        "satellite_contact_record": hex(&record_bytes[..record_len]),
        "setup": hex(&setup),
        "asking": asking,
        "reported": {
            "waiting_minutes": installed.settings.waiting_minutes,
            "request_at": t,
            "due_at": t + u64::from(installed.settings.waiting_minutes) * 60,
        },
    })
}
