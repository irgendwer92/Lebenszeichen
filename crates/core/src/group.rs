//! Messages between the own devices of a person, through the sync mailbox.
//!
//! Every device reads every contact mailbox, so incoming requests and
//! responses are seen everywhere. What the other devices do not see are the
//! actions of one device towards contacts; those go into the sync mailbox in
//! the group form. Builders here put them into [`Device::outbox`];
//! [`Device::receive_sync`] applies what arrives. No I/O.

use rand_core::CryptoRngCore;

use crate::Error;
use crate::contact::{ContactState, remember};
use crate::crypto::{self, ContactKey};
use crate::pairing::{Address, Contact, PairingState, check_time, new_message};
use crate::store::{Device, KnownDevice, Outgoing, Parked, StoredContact, Transfer, TransferPart};
use crate::wire::message::contact_part_ids;
use crate::wire::{
    Body, ContactName, ContactPart, ContactRecord, DeviceAnnounce, DeviceName, ENVELOPE_LEN,
    MailboxId, Message, MessageId, PartData, ReaderId, RelayUrl, Requested, Responded,
    ResponseKind, Rules, RulesSet, SYNC_KEEP_SECS, Stamp, Token, TransferId, record,
};

/// What a sync message meant for this device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncEvent {
    /// Another device announced itself or changed its name.
    DeviceAdded { reader: ReaderId, name: String },
    /// Another device was removed.
    DeviceRemoved { reader: ReaderId },
    /// This device was removed. The caller shows it and deletes the state.
    Removed,
    /// Another device sent this request; its response is accepted here.
    Requested {
        contact: MailboxId,
        request: MessageId,
    },
    /// Another device answered this request.
    Responded {
        contact: MailboxId,
        request: MessageId,
        kind: ResponseKind,
        at: u64,
    },
    /// Another device changed the own rules for this contact.
    RulesSet { contact: MailboxId, rules: Rules },
    /// A contact paired on another device is complete here. The caller
    /// registers this device as a reader of the own mailbox `contact` on
    /// `relay`, with `read_token` and its push address. Since M10
    /// `read_token` is the enrolment token for the other devices (the
    /// mailbox's shared token): where the relay has `reader_tokens`, the
    /// caller gives itself its own token in the same step.
    ContactAdded {
        contact: MailboxId,
        relay: RelayUrl,
        read_token: Token,
    },
    /// Another device handed on its state (`Some`) or ended it (`None`);
    /// the handed state here changed accordingly.
    StateHanded { reason: Option<String> },
    /// Another device ended this contact; it was deleted here.
    ContactRemoved { contact: MailboxId, name: String },
    /// A part of a contact transfer was stored; more are missing.
    PartStored,
    /// The message is about a contact not known yet and is kept for 24
    /// hours.
    Parked,
    /// The message was dropped.
    Ignored(SyncIgnored),
}

/// Why a valid sync message was dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncIgnored {
    /// The identifier was seen before, including own messages.
    Duplicate,
    /// A message type for contacts, not for the sync mailbox.
    NotExpected,
    /// A message type this version does not know.
    UnknownType,
    /// Rules, or a shared or cleared state, not newer than the ones
    /// applied last.
    Outdated,
    /// A `device` or `device_removed` about this device, or a transfer of a
    /// contact that exists already.
    Known,
    /// The transfer was inconsistent or unreadable and was discarded.
    Discarded,
}

impl Device {
    /// Handles an envelope from the sync mailbox.
    ///
    /// Errors mean it is not a valid message of the own devices; the caller
    /// drops it silently. A `contact_part` whose body cannot be read still
    /// discards its transfer.
    pub fn receive_sync(&mut self, envelope: &[u8], now: u64) -> Result<Vec<SyncEvent>, Error> {
        self.expire_sync(now);
        let group = self.group.as_ref().ok_or(Error::Unexpected)?;
        let plaintext = crypto::open_group_plaintext(envelope, &group.key)?;
        let message = match Message::decode(&plaintext) {
            Ok(message) => message,
            Err(error) => {
                if let Some((contact, transfer)) = contact_part_ids(&plaintext) {
                    self.discard_transfer(contact, transfer, now);
                }
                return Err(error.into());
            }
        };
        check_time(&message, now)?;
        let group = self.group.as_mut().expect("checked above");
        if remember(&mut group.seen, &message.id, message.sent_at, now) {
            return Ok(vec![SyncEvent::Ignored(SyncIgnored::Duplicate)]);
        }
        let envelope: [u8; ENVELOPE_LEN] = envelope.try_into().expect("checked on opening");
        let mut events = Vec::new();
        self.apply_sync(message, Some(envelope), now, &mut events);
        Ok(events)
    }

    /// Drops sync state older than 24 hours: undelivered envelopes,
    /// parked messages and incomplete transfers.
    pub fn expire_sync(&mut self, now: u64) {
        let fresh = |since: u64| now < since.saturating_add(SYNC_KEEP_SECS);
        self.outbox.retain(|o| fresh(o.created_at));
        self.parked.retain(|p| fresh(p.received_at));
        self.transfers.retain(|t| fresh(t.first_seen));
    }

    /// `envelope` is given for a message just received, so that it can be
    /// parked; parked messages being applied have none.
    fn apply_sync(
        &mut self,
        message: Message,
        envelope: Option<[u8; ENVELOPE_LEN]>,
        now: u64,
        events: &mut Vec<SyncEvent>,
    ) {
        let event = match message.body {
            Body::Device(device) => self.apply_device(device),
            Body::DeviceRemoved(reader) if reader == self.reader_id => SyncEvent::Removed,
            Body::DeviceRemoved(reader) => {
                self.devices.retain(|d| d.reader != reader);
                SyncEvent::DeviceRemoved { reader }
            }
            Body::ContactPart(part) => return self.apply_part(part, now, events),
            Body::StateShared(shared) => {
                let stamp = Stamp {
                    at: message.sent_at,
                    id: message.id,
                };
                self.apply_shared_state(Some(shared), stamp)
            }
            Body::StateCleared => {
                let stamp = Stamp {
                    at: message.sent_at,
                    id: message.id,
                };
                self.apply_shared_state(None, stamp)
            }
            Body::Requested(_)
            | Body::Responded(_)
            | Body::RulesSet(_)
            | Body::ContactRemoved(_) => {
                let contact = *message.body.sync_contact().expect("all have a contact");
                if self.contact_by_mailbox(&contact).is_none() {
                    let Some(envelope) = envelope else { return };
                    self.parked.push(Parked {
                        envelope,
                        received_at: now,
                    });
                    SyncEvent::Parked
                } else {
                    self.apply_to_contact(contact, message, now)
                }
            }
            Body::Hello(_)
            | Body::HelloOk
            | Body::Rules(_)
            | Body::Request
            | Body::Response(_)
            | Body::End
            | Body::Reveal(_) => SyncEvent::Ignored(SyncIgnored::NotExpected),
            Body::Unknown(_) => SyncEvent::Ignored(SyncIgnored::UnknownType),
        };
        events.push(event);
    }

    fn apply_device(&mut self, device: DeviceAnnounce) -> SyncEvent {
        if device.reader == self.reader_id {
            return SyncEvent::Ignored(SyncIgnored::Known);
        }
        let name = device.name.as_str().to_owned();
        match self.devices.iter_mut().find(|d| d.reader == device.reader) {
            Some(known) => known.name.clone_from(&name),
            None => self.devices.push(KnownDevice {
                reader: device.reader,
                name: name.clone(),
            }),
        }
        SyncEvent::DeviceAdded {
            reader: device.reader,
            name,
        }
    }

    /// Applies a message about a contact this device knows.
    fn apply_to_contact(&mut self, contact: MailboxId, message: Message, now: u64) -> SyncEvent {
        let stamp = Stamp {
            at: message.sent_at,
            id: message.id,
        };
        if let Body::ContactRemoved(_) = message.body {
            let pos = self
                .contacts
                .iter()
                .position(|c| c.state.contact.own_mailbox == contact)
                .expect("checked by the caller");
            let removed = self.contacts.remove(pos);
            return SyncEvent::ContactRemoved {
                contact,
                name: removed.name,
            };
        }
        let state = &mut self
            .contact_by_mailbox_mut(&contact)
            .expect("checked by the caller")
            .state;
        match message.body {
            Body::Requested(r) => {
                state.apply_requested(r.request, r.at, now);
                SyncEvent::Requested {
                    contact,
                    request: r.request,
                }
            }
            Body::Responded(r) => {
                state.apply_responded(&r.request, &r.kind, r.at);
                SyncEvent::Responded {
                    contact,
                    request: r.request,
                    kind: r.kind,
                    at: r.at,
                }
            }
            Body::RulesSet(set) => {
                if state.apply_own_rules(set.rules, stamp) {
                    SyncEvent::RulesSet {
                        contact,
                        rules: set.rules,
                    }
                } else {
                    SyncEvent::Ignored(SyncIgnored::Outdated)
                }
            }
            _ => unreachable!("only messages about a contact"),
        }
    }

    fn apply_part(&mut self, part: ContactPart, now: u64, events: &mut Vec<SyncEvent>) {
        if self.contact_by_mailbox(&part.contact).is_some() {
            return events.push(SyncEvent::Ignored(SyncIgnored::Known));
        }
        let pos = match self
            .transfers
            .iter()
            .position(|t| t.contact == part.contact && t.transfer == part.transfer)
        {
            Some(pos) => pos,
            None => {
                self.transfers.push(Transfer {
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
        let transfer = &mut self.transfers[pos];
        if transfer.discarded {
            return events.push(SyncEvent::Ignored(SyncIgnored::Discarded));
        }
        let data = part.data.as_bytes();
        let slot = transfer
            .parts
            .binary_search_by_key(&part.index, |p| p.index);
        let consistent = part.count == transfer.count
            && part.index < part.count
            && match slot {
                Ok(i) => transfer.parts[i].data == data,
                Err(_) => true,
            };
        if !consistent {
            transfer.discarded = true;
            transfer.parts.clear();
            return events.push(SyncEvent::Ignored(SyncIgnored::Discarded));
        }
        let Err(slot) = slot else {
            // The same part again, under a new message identifier.
            return events.push(SyncEvent::Ignored(SyncIgnored::Duplicate));
        };
        transfer.parts.insert(
            slot,
            TransferPart {
                index: part.index,
                data: data.to_vec(),
            },
        );
        if transfer.parts.len() < usize::from(transfer.count) {
            return events.push(SyncEvent::PartStored);
        }

        let bytes: Vec<u8> = transfer.parts.iter().flat_map(|p| p.data.clone()).collect();
        let Ok(record) = ContactRecord::from_bytes(&bytes) else {
            self.discard_transfer(part.contact, part.transfer, now);
            return events.push(SyncEvent::Ignored(SyncIgnored::Discarded));
        };
        self.transfers.remove(pos);
        events.push(SyncEvent::ContactAdded {
            contact: part.contact,
            relay: record.own_relay,
            read_token: record.read_token,
        });
        self.contacts
            .push(contact_from_record(part.contact, record));
        self.apply_parked(part.contact, now, events);
    }

    /// Marks a transfer as discarded, so that its further parts are ignored
    /// until it expires.
    fn discard_transfer(&mut self, contact: MailboxId, transfer: TransferId, now: u64) {
        match self
            .transfers
            .iter_mut()
            .find(|t| t.contact == contact && t.transfer == transfer)
        {
            Some(t) => {
                t.discarded = true;
                t.parts.clear();
            }
            None => self.transfers.push(Transfer {
                contact,
                transfer,
                count: 0,
                parts: Vec::new(),
                first_seen: now,
                discarded: true,
            }),
        }
    }

    /// Applies the parked messages about a contact that just arrived, in the
    /// order they were received.
    fn apply_parked(&mut self, contact: MailboxId, now: u64, events: &mut Vec<SyncEvent>) {
        let Some(group) = &self.group else { return };
        let key = group.key.clone();
        for parked in std::mem::take(&mut self.parked) {
            let Ok(message) = crypto::open_group(&parked.envelope, &key) else {
                continue;
            };
            if message.body.sync_contact() == Some(&contact) {
                self.apply_sync(message, None, now, events);
            } else {
                self.parked.push(parked);
            }
        }
    }

    /// Encrypts a sync message into the outbox and enters its identifier
    /// into the replay protection, so the own copy is dropped on reading.
    ///
    /// Does nothing without a group: then there are no other devices.
    pub(crate) fn queue_sync(
        &mut self,
        message: &Message,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Error> {
        let Some(group) = &mut self.group else {
            return Ok(());
        };
        remember(&mut group.seen, &message.id, message.sent_at, now);
        let envelope = crypto::seal_group(message, &group.key, rng)?;
        self.outbox.push(Outgoing {
            envelope,
            created_at: now,
        });
        Ok(())
    }

    fn queue_body(
        &mut self,
        body: Body,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Error> {
        let message = new_message(body, now, rng);
        self.queue_sync(&message, now, rng)
    }

    /// After sending request `request` at `at` to the contact with own
    /// mailbox `contact`.
    pub fn sync_requested(
        &mut self,
        contact: &MailboxId,
        request: MessageId,
        at: u64,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Error> {
        let body = Body::Requested(Requested {
            contact: *contact,
            request,
            at,
        });
        self.queue_body(body, now, rng)
    }

    /// After sending a manual or reported response at `at`.
    pub fn sync_responded(
        &mut self,
        contact: &MailboxId,
        request: MessageId,
        kind: ResponseKind,
        at: u64,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Error> {
        let body = Body::Responded(Responded {
            contact: *contact,
            request,
            kind,
            at,
        });
        self.queue_body(body, now, rng)
    }

    /// After [`ContactState::set_rules`]: tells the other devices, with the
    /// same time and identifier as the `rules` sent to the contact, so that
    /// both sides pick the same winner.
    pub fn sync_rules_set(
        &mut self,
        contact: &MailboxId,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Error> {
        let state = &self
            .contact_by_mailbox(contact)
            .ok_or(Error::Unexpected)?
            .state;
        let message = Message {
            id: state.own_rules_stamp.id,
            sent_at: state.own_rules_stamp.at,
            body: Body::RulesSet(RulesSet {
                contact: *contact,
                rules: state.own_rules,
            }),
        };
        self.queue_sync(&message, now, rng)
    }

    /// After sending or receiving `hello_ok`, and after sending the own
    /// rules: hands the contact to the other devices.
    pub fn sync_contact_added(
        &mut self,
        contact: &MailboxId,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Error> {
        let stored = self.contact_by_mailbox(contact).ok_or(Error::Unexpected)?;
        if !stored.state.is_paired() || stored.state.ended {
            return Err(Error::Unexpected);
        }
        let mut bytes = [0; record::MAX_LEN];
        let len = record_of(stored).to_bytes(&mut bytes);
        let parts = record::split(&bytes[..len]);
        let count = u8::try_from(parts.len()).expect("at most four parts");
        let mut transfer = [0; 16];
        rng.fill_bytes(&mut transfer);
        for (index, data) in parts.enumerate() {
            let body = Body::ContactPart(ContactPart {
                contact: *contact,
                transfer,
                index: index as u8,
                count,
                data: PartData::new(data).expect("split to fit"),
            });
            self.queue_body(body, now, rng)?;
        }
        Ok(())
    }

    /// After ending the contact, sending `end` and deleting the mailbox.
    pub fn sync_contact_removed(
        &mut self,
        contact: &MailboxId,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Error> {
        self.queue_body(Body::ContactRemoved(*contact), now, rng)
    }

    /// Announces this device with its name, after installing a handover.
    /// Names are cut to 32 bytes.
    pub fn announce_device(&mut self, now: u64, rng: &mut impl CryptoRngCore) -> Result<(), Error> {
        let body = Body::Device(DeviceAnnounce {
            reader: self.reader_id,
            name: DeviceName::truncated(self.name.as_deref().unwrap_or_default()),
        });
        self.queue_body(body, now, rng)
    }

    /// After unregistering another device from all mailboxes.
    pub fn sync_device_removed(
        &mut self,
        reader: &ReaderId,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Error> {
        self.devices.retain(|d| &d.reader != reader);
        self.queue_body(Body::DeviceRemoved(*reader), now, rng)
    }
}

/// The record of a contact for the other devices. Names are cut to 64
/// bytes. The read token in it is the one this device created the mailbox
/// with, its shared token, which serves the other devices as enrolment
/// token (M10); this device's own token never goes into it.
fn record_of(stored: &StoredContact) -> ContactRecord {
    let state = &stored.state;
    ContactRecord {
        secret_key: state.contact.key.to_bytes(),
        peer_key: state.contact.peer_key,
        read_token: stored.read_token,
        own_relay: stored.own_relay,
        peer_mailbox: state.contact.peer.mailbox,
        peer_send_token: state.contact.peer.send_token,
        peer_relay: state.contact.peer.relay,
        own_rules: state.own_rules,
        own_rules_stamp: state.own_rules_stamp,
        peer_rules: state.peer_rules,
        peer_rules_stamp: state.peer_rules_stamp,
        name: ContactName::truncated(&stored.name),
    }
}

/// A paired contact from its record, with empty replay protection and
/// counters.
fn contact_from_record(own_mailbox: MailboxId, record: ContactRecord) -> StoredContact {
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
    StoredContact {
        name: record.name.as_str().to_owned(),
        own_relay: record.own_relay,
        read_token: record.read_token,
        own_token: None,
        new_token: None,
        state,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use rand_core::OsRng;

    use super::{SyncEvent, SyncIgnored};
    use crate::Error;
    use crate::contact::{ContactState, Event, Ignored};
    use crate::crypto::{self, GroupKey};
    use crate::pairing::{Address, PendingInvitation, accept};
    use crate::store::{Device, Group, KnownDevice, RelayAccount, StoredContact};
    use crate::wire::{
        Body, ContactPart, DeviceAnnounce, DeviceName, ENVELOPE_LEN, MAX_MESSAGE_AGE_SECS, Message,
        PADDED_PLAINTEXT_LEN, PROTOCOL_VERSION, PartData, Reason, RelayUrl, ResponseKind, Rules,
        RulesSet, SYNC_KEEP_SECS,
    };

    pub(crate) const T: u64 = 1_794_600_000;
    const HOUR: u64 = 3600;
    const ANNA: [u8; 32] = [1; 32];
    const BERT: [u8; 32] = [2; 32];

    pub(crate) fn address(fill: u8) -> Address {
        Address {
            relay: RelayUrl::new("https://relay.example").unwrap(),
            mailbox: [fill; 32],
            send_token: [fill + 1; 32],
        }
    }

    /// A contact paired at `T`: the own side as stored, and the other person.
    pub(crate) fn pair(fill: u8, name: &str) -> (StoredContact, ContactState) {
        let (pending, invitation) = PendingInvitation::create(address(fill), None, T, &mut OsRng);
        let (peer, hello) = accept(&invitation, address(fill + 100), T, &mut OsRng).unwrap();
        let mut peer = ContactState::new(peer);
        let mut own = ContactState::new(pending.receive_hello(&hello, T).unwrap());
        let reveal = own.contact.reveal(T, &mut OsRng).unwrap();
        assert!(matches!(
            peer.receive(&reveal, T).unwrap(),
            Event::Revealed { paired: false, .. }
        ));
        let [hello_ok, rules] = own.confirm(T, &mut OsRng).unwrap();
        assert_eq!(peer.receive(&hello_ok, T).unwrap(), Event::Paired);
        peer.receive(&rules, T).unwrap();
        let rules = peer.announce_rules(T, &mut OsRng).unwrap();
        own.receive(&rules, T).unwrap();
        let stored = StoredContact {
            name: name.into(),
            own_relay: RelayUrl::new("https://relay.example").unwrap(),
            read_token: [fill + 50; 32],
            own_token: None,
            new_token: None,
            state: own,
        };
        (stored, peer)
    }

    pub(crate) fn device_with_group() -> Device {
        let mut device = Device::new(&mut OsRng);
        device.name = Some("Laptop".into());
        device.relay = Some(RelayAccount {
            url: RelayUrl::new("https://relay.example").unwrap(),
            access_token: None,
        });
        device.group = Some(Group::new(address(200), [202; 32], &mut OsRng));
        device
    }

    /// Two devices of one person sharing contact Anna, and Anna.
    fn two_devices() -> (Device, Device, ContactState) {
        let mut a = device_with_group();
        let (stored, peer) = pair(1, "Anna");
        a.contacts.push(stored);
        let mut b = Device::from_handover(a.handover([0; 32]).unwrap(), [9; 16]);
        b.name = Some("Phone".into());
        (a, b, peer)
    }

    /// Moves everything in the outbox of `from` to `to`.
    fn deliver(from: &mut Device, to: &mut Device, now: u64) -> Vec<SyncEvent> {
        std::mem::take(&mut from.outbox)
            .iter()
            .flat_map(|o| to.receive_sync(&o.envelope, now).unwrap())
            .collect()
    }

    fn sealed(device: &Device, message: &Message) -> [u8; ENVELOPE_LEN] {
        crypto::seal_group(message, &device.group.as_ref().unwrap().key, &mut OsRng).unwrap()
    }

    fn message(id: u8, sent_at: u64, body: Body) -> Message {
        Message {
            id: [id; 16],
            sent_at,
            body,
        }
    }

    fn state<'a>(device: &'a mut Device, contact: &[u8; 32]) -> &'a mut ContactState {
        &mut device.contact_by_mailbox_mut(contact).unwrap().state
    }

    #[test]
    fn requested_lets_the_other_device_accept_the_response() {
        let (mut a, mut b, mut peer) = two_devices();
        let (id, request) = state(&mut a, &ANNA).request(T + HOUR, &mut OsRng).unwrap();
        a.sync_requested(&ANNA, id, T + HOUR, T + HOUR, &mut OsRng)
            .unwrap();
        assert_eq!(
            deliver(&mut a, &mut b, T + HOUR),
            [SyncEvent::Requested {
                contact: ANNA,
                request: id,
            }]
        );
        let b_state = state(&mut b, &ANNA);
        assert_eq!(b_state.next_request_at(T + HOUR), T + 25 * HOUR);

        peer.receive(&request, T + HOUR).unwrap();
        let response = peer.respond(&id, T + 2 * HOUR, &mut OsRng).unwrap();
        assert!(matches!(
            b_state.receive(&response, T + 2 * HOUR).unwrap(),
            Event::Response {
                kind: ResponseKind::Manual,
                ..
            }
        ));
    }

    #[test]
    fn responded_closes_the_request_and_starts_the_interval() {
        let (mut a, mut b, mut peer) = two_devices();
        let (id, request) = peer.request(T + HOUR, &mut OsRng).unwrap();
        for device in [&mut a, &mut b] {
            let event = state(device, &ANNA).receive(&request, T + HOUR).unwrap();
            assert_eq!(event, Event::Request(id));
        }
        let reason = Reason::new("schläft").unwrap();
        let at = T + 2 * HOUR;
        state(&mut a, &ANNA)
            .report(&id, reason, at, &mut OsRng)
            .unwrap();
        let kind = ResponseKind::Reported(reason);
        a.sync_responded(&ANNA, id, kind, at, at, &mut OsRng)
            .unwrap();
        deliver(&mut a, &mut b, at);
        assert_eq!(state(&mut b, &ANNA).open_request(at), Some(id));
        assert_eq!(
            state(&mut b, &ANNA).incoming.entries[0].reported,
            Some(reason)
        );

        let at = T + 3 * HOUR;
        state(&mut a, &ANNA).respond(&id, at, &mut OsRng).unwrap();
        a.sync_responded(&ANNA, id, ResponseKind::Manual, at, at, &mut OsRng)
            .unwrap();
        assert_eq!(
            deliver(&mut a, &mut b, at),
            [SyncEvent::Responded {
                contact: ANNA,
                request: id,
                kind: ResponseKind::Manual,
                at,
            }]
        );
        let b_state = state(&mut b, &ANNA);
        assert_eq!(b_state.open_request(at), None);
        assert_eq!(b_state.incoming.last_answer, Some(at));
        assert_eq!(
            b_state.incoming.next_allowed(&b_state.own_rules, at),
            at + 6 * HOUR
        );
    }

    #[test]
    fn responded_before_the_request_arrived() {
        let (mut a, mut b, mut peer) = two_devices();
        let (id, request) = peer.request(T + HOUR, &mut OsRng).unwrap();
        let at = T + HOUR + 60;
        state(&mut a, &ANNA).receive(&request, T + HOUR).unwrap();
        state(&mut a, &ANNA).respond(&id, at, &mut OsRng).unwrap();
        a.sync_responded(&ANNA, id, ResponseKind::Manual, at, at, &mut OsRng)
            .unwrap();
        deliver(&mut a, &mut b, at);

        // The request reaches b only now; it is not shown, but counted at
        // its own time.
        let b_state = state(&mut b, &ANNA);
        assert_eq!(
            b_state.receive(&request, at + 60).unwrap(),
            Event::Ignored(Ignored::AlreadyAnswered)
        );
        assert_eq!(b_state.open_request(at + 60), None);
        assert_eq!(b_state.incoming.entries.len(), 1);
        assert_eq!(b_state.incoming.entries[0].at, T + HOUR);
    }

    #[test]
    fn rules_set_takes_the_newest() {
        let (mut a, mut b, mut peer) = two_devices();
        let changed = Rules {
            min_interval_minutes: 60,
            expiry_minutes: 120,
            weekly_limit: 3,
        };
        let announce = state(&mut a, &ANNA)
            .set_rules(changed, T + HOUR, &mut OsRng)
            .unwrap();
        let stamp = state(&mut a, &ANNA).own_rules_stamp;
        a.sync_rules_set(&ANNA, T + HOUR, &mut OsRng).unwrap();
        assert_eq!(
            deliver(&mut a, &mut b, T + HOUR),
            [SyncEvent::RulesSet {
                contact: ANNA,
                rules: changed,
            }]
        );
        assert_eq!(state(&mut b, &ANNA).own_rules, changed);
        assert_eq!(state(&mut b, &ANNA).own_rules_stamp, stamp);
        // The contact sees the same time and identifier.
        assert_eq!(
            peer.receive(&announce, T + HOUR).unwrap(),
            Event::Rules(changed)
        );
        assert_eq!(peer.peer_rules_stamp, stamp);

        let set = |rules| {
            Body::RulesSet(RulesSet {
                contact: ANNA,
                rules,
            })
        };
        // Older, and the same time with a smaller identifier: ignored.
        for (id, at) in [(0xEE, stamp.at - 1), (0, stamp.at)] {
            let older = sealed(&b, &message(id, at, set(Rules::DEFAULT)));
            assert_eq!(
                b.receive_sync(&older, T + HOUR).unwrap(),
                [SyncEvent::Ignored(SyncIgnored::Outdated)]
            );
        }
        // The same time with a larger identifier wins.
        let newer = Message {
            id: [0xFF; 16],
            sent_at: stamp.at,
            body: set(Rules::DEFAULT),
        };
        assert!(matches!(
            b.receive_sync(&sealed(&b, &newer), T + HOUR).unwrap()[..],
            [SyncEvent::RulesSet { .. }]
        ));
        assert_eq!(state(&mut b, &ANNA).own_rules, Rules::DEFAULT);
    }

    #[test]
    fn own_messages_are_dropped_and_old_ones_refused() {
        let (mut a, mut b, _) = two_devices();
        a.sync_contact_removed(&[7; 32], T, &mut OsRng).unwrap();
        let envelope = a.outbox[0].envelope;
        assert_eq!(
            a.receive_sync(&envelope, T).unwrap(),
            [SyncEvent::Ignored(SyncIgnored::Duplicate)]
        );
        assert_eq!(b.receive_sync(&envelope, T).unwrap(), [SyncEvent::Parked]);
        assert_eq!(
            b.receive_sync(&envelope, T + 1).unwrap(),
            [SyncEvent::Ignored(SyncIgnored::Duplicate)]
        );
        let (_, mut c, _) = two_devices();
        assert_eq!(
            c.receive_sync(&envelope, T + MAX_MESSAGE_AGE_SECS + 1),
            Err(Error::Decrypt)
        );
        let late = sealed(&b, &message(5, T, Body::ContactRemoved(ANNA)));
        assert_eq!(
            b.receive_sync(&late, T + MAX_MESSAGE_AGE_SECS + 1),
            Err(Error::Expired)
        );
        let foreign = crypto::seal_group(
            &message(1, T, Body::ContactRemoved(ANNA)),
            &GroupKey::generate(&mut OsRng),
            &mut OsRng,
        )
        .unwrap();
        assert_eq!(b.receive_sync(&foreign, T), Err(Error::Decrypt));
        // Messages for contacts are not expected in the sync mailbox.
        let request = sealed(&b, &message(2, T, Body::Request));
        assert_eq!(
            b.receive_sync(&request, T).unwrap(),
            [SyncEvent::Ignored(SyncIgnored::NotExpected)]
        );
    }

    #[test]
    fn without_group_nothing_is_queued_or_read() {
        let mut device = Device::new(&mut OsRng);
        device.sync_contact_removed(&ANNA, T, &mut OsRng).unwrap();
        assert!(device.outbox.is_empty());
        assert_eq!(
            device.receive_sync(&[PROTOCOL_VERSION; ENVELOPE_LEN], T),
            Err(Error::Unexpected)
        );
    }

    #[test]
    fn contact_removed_deletes_locally() {
        let (mut a, mut b, _) = two_devices();
        a.sync_contact_removed(&ANNA, T, &mut OsRng).unwrap();
        assert_eq!(
            deliver(&mut a, &mut b, T),
            [SyncEvent::ContactRemoved {
                contact: ANNA,
                name: "Anna".into(),
            }]
        );
        assert!(b.contacts.is_empty());
    }

    #[test]
    fn devices_announce_and_remove() {
        let (mut a, mut b, _) = two_devices();
        assert_eq!(
            b.devices,
            [KnownDevice {
                reader: a.reader_id,
                name: "Laptop".into(),
            }]
        );
        b.announce_device(T, &mut OsRng).unwrap();
        assert_eq!(
            deliver(&mut b, &mut a, T),
            [SyncEvent::DeviceAdded {
                reader: [9; 16],
                name: "Phone".into(),
            }]
        );
        assert_eq!(a.devices.len(), 1);
        // A `device` about this device itself is ignored.
        let body = Body::Device(DeviceAnnounce {
            reader: a.reader_id,
            name: DeviceName::new("Fake").unwrap(),
        });
        let me = sealed(&a, &message(1, T, body));
        assert_eq!(
            a.receive_sync(&me, T).unwrap(),
            [SyncEvent::Ignored(SyncIgnored::Known)]
        );

        a.sync_device_removed(&[9; 16], T, &mut OsRng).unwrap();
        assert!(a.devices.is_empty());
        assert_eq!(deliver(&mut a, &mut b, T), [SyncEvent::Removed]);
        let other = sealed(&a, &message(2, T, Body::DeviceRemoved(a.reader_id)));
        assert_eq!(
            b.receive_sync(&other, T).unwrap(),
            [SyncEvent::DeviceRemoved {
                reader: a.reader_id,
            }]
        );
        assert!(b.devices.is_empty());
    }

    /// `a` pairs Bert and queues the parts; returns them and Bert.
    fn bert_parts(a: &mut Device) -> (Vec<[u8; ENVELOPE_LEN]>, ContactState) {
        let (stored, peer) = pair(2, "Bert");
        a.contacts.push(stored);
        a.sync_contact_added(&BERT, T, &mut OsRng).unwrap();
        let parts = std::mem::take(&mut a.outbox)
            .into_iter()
            .map(|o| o.envelope)
            .collect();
        (parts, peer)
    }

    #[test]
    fn contact_part_out_of_order_adds_the_contact() {
        let (mut a, mut b, _) = two_devices();
        let (parts, _) = bert_parts(&mut a);
        assert_eq!(parts.len(), 2);
        assert_eq!(
            b.receive_sync(&parts[1], T).unwrap(),
            [SyncEvent::PartStored]
        );
        assert_eq!(b.transfers.len(), 1);
        assert_eq!(
            b.receive_sync(&parts[0], T + 1).unwrap(),
            [SyncEvent::ContactAdded {
                contact: BERT,
                relay: RelayUrl::new("https://relay.example").unwrap(),
                read_token: [52; 32],
            }]
        );
        assert!(b.transfers.is_empty());
        let added = b.contact_by_mailbox(&BERT).unwrap();
        let original = a.contact_by_mailbox(&BERT).unwrap();
        assert_eq!(added.name, "Bert");
        assert_eq!(
            added.state.contact.key.to_bytes(),
            original.state.contact.key.to_bytes()
        );
        assert_eq!(added.state.contact.peer, original.state.contact.peer);
        assert_eq!(added.state.own_rules_stamp, original.state.own_rules_stamp);
        assert_eq!(
            added.state.peer_rules_stamp,
            original.state.peer_rules_stamp
        );
        assert!(added.state.seen.is_empty() && added.state.incoming.entries.is_empty());
        assert!(added.state.is_paired());

        // A further transfer of an existing contact is ignored.
        a.sync_contact_added(&BERT, T, &mut OsRng).unwrap();
        let envelope = a.outbox[0].envelope;
        assert_eq!(
            b.receive_sync(&envelope, T + 2).unwrap(),
            [SyncEvent::Ignored(SyncIgnored::Known)]
        );
    }

    #[test]
    fn added_contact_works_on_the_new_device() {
        let (mut a, mut b, _) = two_devices();
        let (parts, mut bert) = bert_parts(&mut a);
        for part in &parts {
            b.receive_sync(part, T).unwrap();
        }
        let (id, request) = bert.request(T + HOUR, &mut OsRng).unwrap();
        let b_state = state(&mut b, &BERT);
        assert_eq!(
            b_state.receive(&request, T + HOUR).unwrap(),
            Event::Request(id)
        );
        let response = b_state.respond(&id, T + HOUR, &mut OsRng).unwrap();
        assert!(matches!(
            bert.receive(&response, T + HOUR).unwrap(),
            Event::Response { .. }
        ));
    }

    #[test]
    fn longest_names_are_cut_to_fit() {
        let (mut a, mut b, _) = two_devices();
        let (mut stored, _) = pair(2, &"ä".repeat(40));
        stored.state.contact.peer.relay = RelayUrl::new(&"r".repeat(120)).unwrap();
        stored.own_relay = RelayUrl::new(&"o".repeat(120)).unwrap();
        a.contacts.push(stored);
        a.sync_contact_added(&BERT, T, &mut OsRng).unwrap();
        assert_eq!(a.outbox.len(), 4);
        deliver(&mut a, &mut b, T);
        assert_eq!(b.contact_by_mailbox(&BERT).unwrap().name, "ä".repeat(32));
    }

    fn part_message(id: u8, index: u8, count: u8, data: &[u8]) -> Message {
        message(
            id,
            T,
            Body::ContactPart(ContactPart {
                contact: [3; 32],
                transfer: [4; 16],
                index,
                count,
                data: PartData::new(data).unwrap(),
            }),
        )
    }

    #[test]
    fn inconsistent_transfers_are_discarded() {
        for (second, why) in [
            (part_message(2, 1, 3, &[1]), "other count"),
            (part_message(2, 2, 2, &[1]), "index not below count"),
            (part_message(2, 0, 2, &[9]), "same index, other data"),
        ] {
            let (_, mut b, _) = two_devices();
            let first = sealed(&b, &part_message(1, 0, 2, &[1]));
            assert_eq!(b.receive_sync(&first, T).unwrap(), [SyncEvent::PartStored]);
            assert_eq!(
                b.receive_sync(&sealed(&b, &second), T).unwrap(),
                [SyncEvent::Ignored(SyncIgnored::Discarded)],
                "{why}"
            );
            // Later parts of the same transfer are ignored too.
            let late = sealed(&b, &part_message(3, 1, 2, &[1]));
            assert_eq!(
                b.receive_sync(&late, T).unwrap(),
                [SyncEvent::Ignored(SyncIgnored::Discarded)]
            );
            assert!(b.contact_by_mailbox(&[3; 32]).is_none());
        }
    }

    #[test]
    fn repeated_part_and_unreadable_record() {
        let (_, mut b, _) = two_devices();
        let first = sealed(&b, &part_message(1, 0, 2, &[1]));
        b.receive_sync(&first, T).unwrap();
        let repeated = sealed(&b, &part_message(2, 0, 2, &[1]));
        assert_eq!(
            b.receive_sync(&repeated, T).unwrap(),
            [SyncEvent::Ignored(SyncIgnored::Duplicate)]
        );
        // Complete, but far too short for a record.
        let last = sealed(&b, &part_message(3, 1, 2, &[1]));
        assert_eq!(
            b.receive_sync(&last, T).unwrap(),
            [SyncEvent::Ignored(SyncIgnored::Discarded)]
        );
        assert!(b.transfers[0].discarded);

        let (_, mut b, _) = two_devices();
        let zero = sealed(&b, &part_message(4, 0, 0, &[]));
        assert_eq!(
            b.receive_sync(&zero, T).unwrap(),
            [SyncEvent::Ignored(SyncIgnored::Discarded)]
        );
    }

    #[test]
    fn oversized_part_discards_its_transfer() {
        let (_, mut b, _) = two_devices();
        let first = sealed(&b, &part_message(1, 0, 2, &[1]));
        b.receive_sync(&first, T).unwrap();

        // A part claiming 179 bytes cannot be encoded; alter the plaintext.
        let mut plaintext = [0; PADDED_PLAINTEXT_LEN];
        part_message(2, 1, 2, &[1; 178])
            .encode(&mut plaintext)
            .unwrap();
        plaintext[27 + 50] = 179;
        let key = b.group.as_ref().unwrap().key.to_bytes();
        let envelope = raw_group_envelope(&plaintext, &key);
        assert!(matches!(b.receive_sync(&envelope, T), Err(Error::Wire(_))));
        assert!(b.transfers[0].discarded);
        assert!(b.transfers[0].parts.is_empty());
    }

    fn raw_group_envelope(plaintext: &[u8; PADDED_PLAINTEXT_LEN], key: &[u8; 32]) -> Vec<u8> {
        use crypto_secretbox::aead::{Aead, KeyInit};
        let nonce = [7; 24];
        let sealed = crypto_secretbox::XSalsa20Poly1305::new(key.into())
            .encrypt(&nonce.into(), &plaintext[..])
            .unwrap();
        // The tag in front, as in libsodium.
        [&[PROTOCOL_VERSION][..], &nonce, &sealed].concat()
    }

    #[test]
    fn incomplete_transfer_is_dropped_after_a_day() {
        let (mut a, mut b, _) = two_devices();
        let (parts, _) = bert_parts(&mut a);
        b.receive_sync(&parts[0], T).unwrap();
        b.expire_sync(T + SYNC_KEEP_SECS - 1);
        assert_eq!(b.transfers.len(), 1);
        b.expire_sync(T + SYNC_KEEP_SECS);
        assert!(b.transfers.is_empty());
    }

    #[test]
    fn parked_messages_apply_when_the_contact_arrives() {
        let (mut a, mut b, _) = two_devices();
        let (stored, _) = pair(2, "Bert");
        a.contacts.push(stored);
        let changed = Rules {
            weekly_limit: 1,
            ..Rules::DEFAULT
        };
        // The parts go out first, but b reads the later messages first.
        a.sync_contact_added(&BERT, T, &mut OsRng).unwrap();
        let parts = std::mem::take(&mut a.outbox);
        state(&mut a, &BERT)
            .set_rules(changed, T + 10, &mut OsRng)
            .unwrap();
        a.sync_rules_set(&BERT, T + 10, &mut OsRng).unwrap();
        a.sync_requested(&BERT, [8; 16], T + 20, T + 20, &mut OsRng)
            .unwrap();
        assert_eq!(
            deliver(&mut a, &mut b, T + 30),
            [SyncEvent::Parked, SyncEvent::Parked]
        );
        assert_eq!(b.parked.len(), 2);

        let mut events = Vec::new();
        for part in &parts {
            events.extend(b.receive_sync(&part.envelope, T + 40).unwrap());
        }
        assert!(matches!(
            events[..],
            [
                SyncEvent::PartStored,
                SyncEvent::ContactAdded { .. },
                SyncEvent::RulesSet { .. },
                SyncEvent::Requested { .. },
            ]
        ));
        assert!(b.parked.is_empty());
        let b_state = state(&mut b, &BERT);
        assert_eq!(b_state.own_rules, changed);
        assert_eq!(b_state.outgoing.entries[0].id, [8; 16]);
    }

    fn reason(text: &str) -> crate::wire::Reason {
        crate::wire::Reason::new(text).unwrap()
    }

    fn shown(device: &Device, now: u64) -> Option<(String, bool, bool)> {
        device
            .current_state(now)
            .map(|s| (s.reason.as_str().to_owned(), s.shared, s.handed))
    }

    #[test]
    fn a_shared_state_is_handed_to_the_other_device() {
        let (mut a, mut b, _) = two_devices();
        a.share_state(reason("Phone empty"), 60, T, &mut OsRng)
            .unwrap();
        assert_eq!(shown(&a, T), Some(("Phone empty".into(), true, false)));
        assert_eq!(
            deliver(&mut a, &mut b, T),
            [SyncEvent::StateHanded {
                reason: Some("Phone empty".into())
            }]
        );
        assert_eq!(shown(&b, T + 1), Some(("Phone empty".into(), false, true)));
        assert_eq!(b.current_state(T + 1).unwrap().until, T + 3600);
        assert_eq!(shown(&b, T + 3600), None, "ends with the state");
    }

    #[test]
    fn clearing_with_share_ends_it_everywhere_but_keeps_unshared_states() {
        let (mut a, mut b, _) = two_devices();
        b.set_state(reason("asleep"), 480, T).unwrap();
        a.share_state(reason("Phone empty"), 60, T + 1, &mut OsRng)
            .unwrap();
        deliver(&mut a, &mut b, T + 1);
        // The handed state was set later, so it applies.
        assert_eq!(shown(&b, T + 2), Some(("Phone empty".into(), false, true)));
        a.clear_shared_state(T + 3, &mut OsRng).unwrap();
        assert_eq!(shown(&a, T + 3), None);
        assert_eq!(
            deliver(&mut a, &mut b, T + 3),
            [SyncEvent::StateHanded { reason: None }]
        );
        assert_eq!(shown(&b, T + 4), Some(("asleep".into(), false, false)));
    }

    #[test]
    fn clearing_without_share_stays_local() {
        let (mut a, mut b, _) = two_devices();
        a.share_state(reason("Phone empty"), 60, T, &mut OsRng)
            .unwrap();
        deliver(&mut a, &mut b, T);
        a.clear_state();
        assert!(a.outbox.is_empty());
        assert_eq!(shown(&a, T + 1), None);
        b.clear_state();
        assert_eq!(shown(&b, T + 1), Some(("Phone empty".into(), false, true)));
    }

    #[test]
    fn the_cross_on_a_handed_state_ends_it_on_the_sharing_device() {
        let (mut a, mut b, _) = two_devices();
        a.share_state(reason("Phone empty"), 60, T, &mut OsRng)
            .unwrap();
        deliver(&mut a, &mut b, T);
        b.end_current_state(T + 5, &mut OsRng).unwrap();
        assert_eq!(shown(&b, T + 5), None);
        deliver(&mut b, &mut a, T + 5);
        assert_eq!(shown(&a, T + 6), None);
    }

    #[test]
    fn the_cross_on_an_unshared_state_stays_local() {
        let (_, mut b, _) = two_devices();
        b.set_state(reason("asleep"), 60, T).unwrap();
        b.end_current_state(T, &mut OsRng).unwrap();
        assert!(b.outbox.is_empty());
        assert_eq!(shown(&b, T), None);
    }

    #[test]
    fn a_newer_share_replaces_the_shared_state_everywhere() {
        let (mut a, mut b, _) = two_devices();
        a.share_state(reason("Phone empty"), 60, T, &mut OsRng)
            .unwrap();
        deliver(&mut a, &mut b, T);
        b.share_state(reason("asleep"), 480, T + 10, &mut OsRng)
            .unwrap();
        deliver(&mut b, &mut a, T + 10);
        assert_eq!(shown(&a, T + 11), Some(("asleep".into(), false, true)));
        assert_eq!(a.reports.state, None, "the own shared state was replaced");
        // A clear from b ends it on a as well.
        b.clear_shared_state(T + 20, &mut OsRng).unwrap();
        deliver(&mut b, &mut a, T + 20);
        assert_eq!(shown(&a, T + 21), None);
    }

    #[test]
    fn an_older_share_or_clear_arriving_late_is_ignored() {
        let (mut a, mut b, _) = two_devices();
        a.share_state(reason("Phone empty"), 60, T, &mut OsRng)
            .unwrap();
        let old_share = std::mem::take(&mut a.outbox);
        a.clear_shared_state(T + 5, &mut OsRng).unwrap();
        deliver(&mut a, &mut b, T + 5);
        let events: Vec<_> = old_share
            .iter()
            .flat_map(|o| b.receive_sync(&o.envelope, T + 6).unwrap())
            .collect();
        assert_eq!(events, [SyncEvent::Ignored(SyncIgnored::Outdated)]);
        assert_eq!(shown(&b, T + 6), None);
    }

    #[test]
    fn sharing_without_other_devices_sets_it_here() {
        let mut device = Device::new(&mut OsRng);
        device
            .share_state(reason("Phone empty"), 60, T, &mut OsRng)
            .unwrap();
        assert!(device.outbox.is_empty());
        assert_eq!(shown(&device, T), Some(("Phone empty".into(), true, false)));
        device.clear_shared_state(T, &mut OsRng).unwrap();
        assert_eq!(shown(&device, T), None);
    }

    #[test]
    fn battery_hands_over_once_and_ends_it_when_charging() {
        use crate::report::BatteryChange::{Cleared, None as Nothing, Shared};
        let (mut a, mut b, _) = two_devices();
        assert_eq!(a.check_battery(1, false, T, &mut OsRng), Ok(Nothing), "off");
        a.set_battery(Some(reason("Phone empty"))).unwrap();
        assert_eq!(a.check_battery(3, false, T, &mut OsRng), Ok(Nothing));
        assert_eq!(
            a.check_battery(2, true, T, &mut OsRng),
            Ok(Nothing),
            "charging"
        );
        assert_eq!(a.check_battery(2, false, T, &mut OsRng), Ok(Shared));
        deliver(&mut a, &mut b, T);
        assert_eq!(shown(&b, T + 1), Some(("Phone empty".into(), false, true)));
        assert_eq!(b.current_state(T).unwrap().until, T + 24 * 3600);
        // Once per crossing: still low, nothing new, even after a ✕ elsewhere.
        b.end_current_state(T + 2, &mut OsRng).unwrap();
        deliver(&mut b, &mut a, T + 2);
        assert_eq!(a.check_battery(1, false, T + 3, &mut OsRng), Ok(Nothing));
        assert_eq!(shown(&a, T + 3), None);
        // Recovered: nothing to end any more, but armed again.
        assert_eq!(a.check_battery(4, false, T + 4, &mut OsRng), Ok(Nothing));
        assert_eq!(a.check_battery(5, false, T + 4, &mut OsRng), Ok(Nothing));
        assert_eq!(a.check_battery(2, false, T + 5, &mut OsRng), Ok(Shared));
        deliver(&mut a, &mut b, T + 5);
        assert_eq!(a.check_battery(2, true, T + 6, &mut OsRng), Ok(Cleared));
        deliver(&mut a, &mut b, T + 6);
        assert_eq!(shown(&b, T + 7), None);
    }

    #[test]
    fn battery_leaves_a_state_shared_by_another_device() {
        use crate::report::BatteryChange::{None as Nothing, Shared};
        let (mut a, mut b, _) = two_devices();
        a.set_battery(Some(reason("Phone empty"))).unwrap();
        assert_eq!(a.check_battery(2, false, T, &mut OsRng), Ok(Shared));
        deliver(&mut a, &mut b, T);
        b.share_state(reason("asleep"), 480, T + 10, &mut OsRng)
            .unwrap();
        deliver(&mut b, &mut a, T + 10);
        assert_eq!(a.check_battery(50, true, T + 20, &mut OsRng), Ok(Nothing));
        assert!(a.outbox.is_empty());
        assert_eq!(shown(&a, T + 21), Some(("asleep".into(), false, true)));
    }

    #[test]
    fn parked_messages_and_outbox_expire() {
        let (mut a, mut b, _) = two_devices();
        a.sync_requested(&BERT, [8; 16], T, T, &mut OsRng).unwrap();
        let outgoing = a.outbox.clone();
        deliver(&mut a, &mut b, T);
        assert_eq!(b.parked.len(), 1);
        b.expire_sync(T + SYNC_KEEP_SECS);
        assert!(b.parked.is_empty());

        a.outbox = outgoing;
        a.expire_sync(T + SYNC_KEEP_SECS - 1);
        assert_eq!(a.outbox.len(), 1);
        a.expire_sync(T + SYNC_KEEP_SECS);
        assert!(a.outbox.is_empty());
    }

    #[test]
    fn sync_bodies_from_a_contact_are_ignored() {
        let (mut a, _, peer) = two_devices();
        let envelope = peer
            .contact
            .seal(Body::ContactRemoved(ANNA), T, &mut OsRng)
            .unwrap();
        assert_eq!(
            state(&mut a, &ANNA).receive(&envelope, T).unwrap(),
            Event::Ignored(Ignored::NotExpected)
        );
    }
}
