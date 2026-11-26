//! The satellite as the full devices see it (M10, `docs/m10-satellite.md`,
//! part 2).
//!
//! A satellite reads the own contact mailboxes with a token of its own and
//! talks to the full devices only through its **channel**: one mailbox,
//! one symmetric key, envelopes of form C. Full devices accept only up
//! types (80–87) there and send only down types (88–99), so a reflected
//! message is always one the receiver drops.
//!
//! Here: what a full device stores about a satellite, the down messages it
//! queues next to its sync messages, the up messages it applies, the sync
//! messages about satellites between full devices (73–75), and the records
//! for the satellite (its contact record and the setup). No I/O: the
//! client registers readers and delivers the outboxes.

use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::contact::{Seen, is_unset, next_stamp, remember};
use crate::crypto::{self, GroupKey};
use crate::pairing::{check_time, new_message};
use crate::report::{MAX_DELAY_MINUTES, MAX_STATE_MINUTES};
use crate::rules::WEEK_SECS;
use crate::store::{Device, Outgoing, Parked};
use crate::wire::satellite::{
    self, ContactRecord as SatelliteContactRecord, OpenRequest, Recent, Record, SetupHeader,
    WifiName, WifiPassword,
};
use crate::wire::{
    Body, ContactFlags, ContactPart, ContactSettings, ContactSettingsFor, DeviceName, ENVELOPE_LEN,
    MailboxId, Message, MessageId, PartData, ReaderId, RelayUrl, ResponseKind, SYNC_KEEP_SECS,
    SatelliteSettings, SettingsFor, Stamp, Token, record,
};

/// Most relays a satellite talks to (decision 9): the relay of its
/// channel and the relays of the own mailboxes it reads.
pub const MAX_SATELLITE_RELAYS: usize = 3;

/// A satellite of the person, as every full device keeps it.
///
/// The same JSON form, without what only this device knows (own tokens,
/// outbox, parked messages, pending enrolments), is part of the handover.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Satellite {
    /// Reader identifier of the satellite on every mailbox.
    #[serde(with = "crate::hex")]
    pub reader: ReaderId,
    pub name: String,
    /// Relay of the channel mailbox.
    pub relay: RelayUrl,
    /// The channel mailbox.
    #[serde(with = "crate::hex")]
    pub mailbox: MailboxId,
    /// The shared token of the channel mailbox, or an enrolment token
    /// registered for this device; see [`crate::store::ReadAccess`].
    #[serde(with = "crate::hex")]
    pub read_token: Token,
    /// This device's own token for the channel mailbox.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::hex::option"
    )]
    pub own_token: Option<Token>,
    /// An own token sent to the relay without a known answer.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::hex::option"
    )]
    pub new_token: Option<Token>,
    /// Send token of the channel, held by the satellite and the full
    /// devices.
    #[serde(with = "crate::hex")]
    pub send_token: Token,
    /// A new send token sent to the relay without a known answer, after a
    /// full device was removed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::hex::option"
    )]
    pub new_send_token: Option<Token>,
    /// The channel key.
    pub key: GroupKey,
    /// Replay protection for the channel.
    #[serde(default)]
    pub seen: Vec<Seen>,
    #[serde(default)]
    pub settings: SatelliteSettings,
    #[serde(
        default,
        skip_serializing_if = "is_unset",
        with = "crate::contact::stamp"
    )]
    pub settings_stamp: Stamp,
    /// Settings per contact, only for contacts someone changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contacts: Vec<SatelliteContact>,
    /// Envelopes for the channel the relay has not accepted yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outbox: Vec<Outgoing>,
    /// Up messages about contacts this device does not know yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parked: Vec<Parked>,
    /// Contacts paired on this device that the satellite still has to be
    /// registered on and sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<Enrolment>,
}

/// The settings of a satellite for one contact, with their stamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SatelliteContact {
    /// Own mailbox of the contact.
    #[serde(with = "crate::hex")]
    pub mailbox: MailboxId,
    #[serde(flatten)]
    pub flags: ContactFlags,
    #[serde(
        default,
        skip_serializing_if = "is_unset",
        with = "crate::contact::stamp"
    )]
    pub stamp: Stamp,
}

/// A contact the satellite still has to get: registered as a reader of the
/// own mailbox with an enrolment token, then sent in `home_contact_part`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enrolment {
    #[serde(with = "crate::hex")]
    pub contact: MailboxId,
    /// The enrolment token, stored before it goes to the relay.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::hex::option"
    )]
    pub token: Option<Token>,
    /// Given up 24 hours after this.
    pub since: u64,
}

impl Satellite {
    /// A satellite set up on this device, with a new channel key and
    /// default settings.
    pub fn new(
        reader: ReaderId,
        name: String,
        relay: RelayUrl,
        mailbox: MailboxId,
        read_token: Token,
        send_token: Token,
        rng: &mut impl CryptoRngCore,
    ) -> Self {
        Self::from_parts(
            reader,
            name,
            relay,
            mailbox,
            read_token,
            send_token,
            GroupKey::generate(rng),
        )
    }

    fn from_parts(
        reader: ReaderId,
        name: String,
        relay: RelayUrl,
        mailbox: MailboxId,
        read_token: Token,
        send_token: Token,
        key: GroupKey,
    ) -> Self {
        Self {
            reader,
            name,
            relay,
            mailbox,
            read_token,
            own_token: None,
            new_token: None,
            send_token,
            new_send_token: None,
            key,
            seen: Vec::new(),
            settings: SatelliteSettings::DEFAULT,
            settings_stamp: Stamp::default(),
            contacts: Vec::new(),
            outbox: Vec::new(),
            parked: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// The settings for the contact with own mailbox `mailbox`; reported
    /// answers off and no ask button until someone changes them.
    pub fn contact_flags(&self, mailbox: &MailboxId) -> ContactFlags {
        self.contact(mailbox).map(|c| c.flags).unwrap_or_default()
    }

    fn contact(&self, mailbox: &MailboxId) -> Option<&SatelliteContact> {
        self.contacts.iter().find(|c| &c.mailbox == mailbox)
    }

    fn contact_stamp(&self, mailbox: &MailboxId) -> Stamp {
        self.contact(mailbox).map(|c| c.stamp).unwrap_or_default()
    }

    fn set_contact(&mut self, mailbox: MailboxId, flags: ContactFlags, stamp: Stamp) {
        self.contacts.retain(|c| c.mailbox != mailbox);
        self.contacts.push(SatelliteContact {
            mailbox,
            flags,
            stamp,
        });
    }

    /// The record for the other full devices. The enrolment token in it is
    /// this device's `read_token`: the shared token of the channel on the
    /// device that set the satellite up, which is the one that sends it.
    fn record(&self) -> Record {
        Record {
            name: DeviceName::truncated(&self.name),
            relay: self.relay,
            mailbox: self.mailbox,
            enrolment_token: self.read_token,
            send_token: self.send_token,
            key: self.key.to_bytes(),
        }
    }

    /// The form for the handover: without own tokens, outbox, parked
    /// messages and pending enrolments.
    pub(crate) fn for_handover(&self) -> Self {
        Self {
            own_token: None,
            new_token: None,
            new_send_token: None,
            outbox: Vec::new(),
            parked: Vec::new(),
            pending: Vec::new(),
            ..self.clone()
        }
    }

    /// Encrypts a down message into the outbox of the channel.
    fn queue(
        &mut self,
        message: &Message,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Error> {
        debug_assert!(message.body.is_down());
        let envelope = crypto::seal_group(message, &self.key, rng)?;
        self.outbox.push(Outgoing {
            envelope,
            created_at: now,
        });
        Ok(())
    }
}

/// What an up message from a satellite meant for this device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelEvent {
    /// The satellite answered this request, as `responded`.
    Answered {
        satellite: ReaderId,
        contact: MailboxId,
        request: MessageId,
        kind: ResponseKind,
        at: u64,
    },
    /// The satellite asked this contact, as `requested`.
    Asked {
        satellite: ReaderId,
        contact: MailboxId,
        request: MessageId,
    },
    /// The satellite shared its state (`Some`) or ended it (`None`); the
    /// handed state here changed accordingly.
    StateHanded {
        satellite: ReaderId,
        reason: Option<String>,
    },
    /// About a contact not known yet; kept for 24 hours.
    Parked,
    /// Dropped.
    Ignored(ChannelIgnored),
}

/// Why a valid channel message was dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelIgnored {
    /// The identifier was seen before.
    Duplicate,
    /// Not an up type: a down type (also every own one), a type of the
    /// sync mailbox or of contacts, or one this version does not know.
    NotUp,
    /// A shared or cleared state not newer than the one applied last.
    Outdated,
}

/// The down message a full device sends next to a sync message, with the
/// same time and identifier, if there is one.
fn down_of(body: &Body) -> Option<Body> {
    Some(match body {
        Body::Requested(r) => Body::HomeRequested(r.clone()),
        Body::Responded(r) => Body::HomeResponded(r.clone()),
        Body::RulesSet(r) => Body::HomeRules(r.clone()),
        Body::StateShared(s) => Body::HomeStateShared(s.clone()),
        Body::StateCleared => Body::HomeStateCleared,
        _ => return None,
    })
}

fn channel_state(satellite: ReaderId, event: crate::group::SyncEvent) -> ChannelEvent {
    match event {
        crate::group::SyncEvent::StateHanded { reason } => {
            ChannelEvent::StateHanded { satellite, reason }
        }
        _ => ChannelEvent::Ignored(ChannelIgnored::Outdated),
    }
}

/// Checks settings set on a full device: a waiting time of 1 to
/// [`MAX_DELAY_MINUTES`], a preset of 1 to [`MAX_STATE_MINUTES`] with a
/// reason.
pub fn settings_valid(settings: &SatelliteSettings) -> bool {
    let waiting = (1..=MAX_DELAY_MINUTES).contains(&u32::from(settings.waiting_minutes));
    let preset = settings.preset.is_none_or(|p| {
        (1..=MAX_STATE_MINUTES).contains(&p.minutes) && !p.reason.as_str().trim().is_empty()
    });
    waiting && preset
}

/// The key under which the parts of a satellite record are collected in
/// [`Device::satellite_transfers`]: the reader identifier, padded with
/// zero bytes.
pub(crate) fn transfer_key(reader: &ReaderId) -> MailboxId {
    let mut key = [0; 32];
    key[..16].copy_from_slice(reader);
    key
}

impl Device {
    pub fn satellite(&self, reader: &ReaderId) -> Option<&Satellite> {
        self.satellites.iter().find(|s| &s.reader == reader)
    }

    pub fn satellite_mut(&mut self, reader: &ReaderId) -> Option<&mut Satellite> {
        self.satellites.iter_mut().find(|s| &s.reader == reader)
    }

    /// The satellite whose channel mailbox is `mailbox`.
    pub fn satellite_by_mailbox(&self, mailbox: &MailboxId) -> Option<&Satellite> {
        self.satellites.iter().find(|s| &s.mailbox == mailbox)
    }

    /// Adds a satellite set up on this device; fails if its reader is
    /// known already.
    pub fn add_satellite(&mut self, satellite: Satellite) -> Result<(), Error> {
        if satellite.reader == self.reader_id
            || self.satellite(&satellite.reader).is_some()
            || self.devices.iter().any(|d| d.reader == satellite.reader)
        {
            return Err(Error::Unexpected);
        }
        self.satellites.push(satellite);
        Ok(())
    }

    /// Forgets a satellite, after it was removed or its channel is gone.
    pub fn forget_satellite(&mut self, reader: &ReaderId) -> Option<Satellite> {
        let pos = self.satellites.iter().position(|s| &s.reader == reader)?;
        Some(self.satellites.remove(pos))
    }

    /// Queues the down counterpart of a sync message for every satellite.
    pub(crate) fn mirror_down(
        &mut self,
        message: &Message,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Error> {
        let Some(body) = down_of(&message.body) else {
            return Ok(());
        };
        let down = Message {
            id: message.id,
            sent_at: message.sent_at,
            body,
        };
        for satellite in &mut self.satellites {
            satellite.queue(&down, now, rng)?;
        }
        Ok(())
    }

    /// Tells the other full devices about a satellite set up on this one
    /// (`satellite_part` into the sync outbox). Nothing without a group.
    pub fn announce_satellite(
        &mut self,
        reader: &ReaderId,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Error> {
        let record = self.satellite(reader).ok_or(Error::Unexpected)?.record();
        let mut bytes = [0; Record::MAX_LEN];
        let len = record.to_bytes(&mut bytes);
        let parts = record::split(&bytes[..len]);
        let count = u8::try_from(parts.len()).expect("at most two parts");
        let mut transfer = [0; 16];
        rng.fill_bytes(&mut transfer);
        for (index, data) in parts.enumerate() {
            let body = Body::SatellitePart(satellite::Part {
                reader: *reader,
                transfer,
                index: index as u8,
                count,
                data: PartData::new(data).expect("split to fit"),
            });
            let message = new_message(body, now, rng);
            self.queue_sync(&message, now, rng)?;
        }
        Ok(())
    }

    /// Changes the settings of a satellite: `home_settings` to it and
    /// `satellite_settings` to the other full devices, with the same
    /// stamp. Fails for unknown satellites and invalid settings.
    pub fn set_satellite_settings(
        &mut self,
        reader: &ReaderId,
        settings: SatelliteSettings,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Error> {
        if !settings_valid(&settings) {
            return Err(Error::Unexpected);
        }
        let satellite = self.satellite_mut(reader).ok_or(Error::Unexpected)?;
        let stamp = next_stamp(satellite.settings_stamp, now, rng);
        satellite.settings = settings;
        satellite.settings_stamp = stamp;
        let down = Message {
            id: stamp.id,
            sent_at: stamp.at,
            body: Body::HomeSettings(settings),
        };
        satellite.queue(&down, now, rng)?;
        let sync = Message {
            body: Body::SatelliteSettings(SettingsFor {
                reader: *reader,
                settings,
            }),
            ..down
        };
        self.queue_sync(&sync, now, rng)
    }

    /// Changes what a satellite does for one contact. An ask button bound
    /// to another contact is taken from it first, so that a button always
    /// names one contact.
    pub fn set_satellite_contact(
        &mut self,
        reader: &ReaderId,
        contact: &MailboxId,
        flags: ContactFlags,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Error> {
        if self.contact_by_mailbox(contact).is_none() {
            return Err(Error::Unexpected);
        }
        let satellite = self.satellite(reader).ok_or(Error::Unexpected)?;
        if flags.ask_button != 0 {
            let others: Vec<SatelliteContact> = satellite
                .contacts
                .iter()
                .filter(|c| &c.mailbox != contact && c.flags.ask_button == flags.ask_button)
                .copied()
                .collect();
            for other in others {
                let freed = ContactFlags {
                    ask_button: 0,
                    ..other.flags
                };
                self.queue_contact_settings(reader, &other.mailbox, freed, now, rng)?;
            }
        }
        self.queue_contact_settings(reader, contact, flags, now, rng)
    }

    fn queue_contact_settings(
        &mut self,
        reader: &ReaderId,
        contact: &MailboxId,
        flags: ContactFlags,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Error> {
        let satellite = self.satellite_mut(reader).ok_or(Error::Unexpected)?;
        let stamp = next_stamp(satellite.contact_stamp(contact), now, rng);
        satellite.set_contact(*contact, flags, stamp);
        let settings = ContactSettings {
            contact: *contact,
            of: flags,
        };
        let down = Message {
            id: stamp.id,
            sent_at: stamp.at,
            body: Body::HomeContactSettings(settings),
        };
        satellite.queue(&down, now, rng)?;
        let sync = Message {
            body: Body::SatelliteContactSettings(ContactSettingsFor {
                reader: *reader,
                settings,
            }),
            ..down
        };
        self.queue_sync(&sync, now, rng)
    }

    /// The satellite contact record of a paired contact, with the
    /// satellite's enrolment token on the own mailbox.
    pub fn satellite_contact_record(
        &self,
        reader: &ReaderId,
        contact: &MailboxId,
        enrolment: Token,
        now: u64,
    ) -> Result<SatelliteContactRecord, Error> {
        let satellite = self.satellite(reader).ok_or(Error::Unexpected)?;
        let stored = self
            .contact_by_mailbox(contact)
            .filter(|c| c.state.is_paired() && !c.state.ended)
            .ok_or(Error::Unexpected)?;
        let state = &stored.state;
        let outgoing = &state.outgoing;
        let open_request = outgoing
            .open(&state.peer_rules, now)
            .map(|e| OpenRequest { id: e.id, at: e.at });
        let times: Vec<u64> = outgoing
            .entries
            .iter()
            .map(|e| e.at)
            .filter(|&at| now < at.saturating_add(WEEK_SECS))
            .collect();
        Ok(SatelliteContactRecord {
            secret_key: state.contact.key.to_bytes(),
            peer_key: state.contact.peer_key,
            enrolment_token: enrolment,
            own_relay: stored.own_relay,
            peer_mailbox: state.contact.peer.mailbox,
            peer_send_token: state.contact.peer.send_token,
            peer_relay: state.contact.peer.relay,
            own_rules: state.own_rules,
            own_rules_stamp: state.own_rules_stamp,
            peer_rules: state.peer_rules,
            peer_rules_stamp: state.peer_rules_stamp,
            settings: satellite.contact_flags(contact),
            open_request,
            last_answer: outgoing.last_answer,
            recent: Recent::newest(&times),
            name: crate::wire::ContactName::truncated(&stored.name),
        })
    }

    /// Sends a contact to the satellite (`home_contact_part` into the
    /// outbox of its channel), after its reader was registered on the own
    /// mailbox with `enrolment`.
    pub fn queue_satellite_contact(
        &mut self,
        reader: &ReaderId,
        contact: &MailboxId,
        enrolment: Token,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Error> {
        let record = self.satellite_contact_record(reader, contact, enrolment, now)?;
        let mut bytes = [0; SatelliteContactRecord::MAX_LEN];
        let len = record.to_bytes(&mut bytes);
        let parts = record::split(&bytes[..len]);
        let count = u8::try_from(parts.len()).expect("at most four parts");
        let mut transfer = [0; 16];
        rng.fill_bytes(&mut transfer);
        let satellite = self.satellite_mut(reader).expect("checked by the record");
        for (index, data) in parts.enumerate() {
            let body = Body::HomeContactPart(ContactPart {
                contact: *contact,
                transfer,
                index: index as u8,
                count,
                data: PartData::new(data).expect("split to fit"),
            });
            satellite.queue(&new_message(body, now, rng), now, rng)?;
        }
        Ok(())
    }

    /// The setup for a satellite, as BLE or USB serial carries it: the
    /// channel with the satellite's enrolment token on it, the settings,
    /// and every contact in `contacts` with the satellite's enrolment
    /// token on its own mailbox. Fails above 255 contacts.
    pub fn satellite_setup(
        &self,
        reader: &ReaderId,
        wifi_name: WifiName,
        wifi_password: WifiPassword,
        channel_enrolment: Token,
        contacts: &[(MailboxId, Token)],
        now: u64,
    ) -> Result<Vec<u8>, Error> {
        let satellite = self.satellite(reader).ok_or(Error::Unexpected)?;
        let count = u8::try_from(contacts.len()).map_err(|_| crate::wire::Error::TooLong)?;
        let header = SetupHeader {
            reader: *reader,
            wifi_name,
            wifi_password,
            relay: satellite.relay,
            mailbox: satellite.mailbox,
            enrolment_token: channel_enrolment,
            send_token: satellite.send_token,
            key: satellite.key.to_bytes(),
            settings_stamp: satellite.settings_stamp,
            settings: satellite.settings,
            contacts: count,
        };
        let mut head = [0; SetupHeader::MAX_LEN];
        let len = header.to_bytes(&mut head);
        let mut out = head[..len].to_vec();
        for (contact, token) in contacts {
            let record = self.satellite_contact_record(reader, contact, *token, now)?;
            let mut entry = [0; satellite::MAX_ENTRY_LEN];
            let len = satellite::write_entry(contact, &record, &mut entry);
            out.extend_from_slice(&entry[..len]);
        }
        Ok(out)
    }

    /// The send token of the channel was replaced on the relay (after a
    /// full device was removed): uses it from now on and tells the
    /// satellite (`home_send_token`).
    pub fn channel_token_replaced(
        &mut self,
        reader: &ReaderId,
        token: Token,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Error> {
        let satellite = self.satellite_mut(reader).ok_or(Error::Unexpected)?;
        satellite.send_token = token;
        satellite.new_send_token = None;
        let message = new_message(Body::HomeSendToken(token), now, rng);
        satellite.queue(&message, now, rng)
    }

    /// Notes that every satellite still has to get this newly paired
    /// contact; the client registers it and sends the record.
    pub(crate) fn enrol_satellites(&mut self, contact: &MailboxId, now: u64) {
        for satellite in &mut self.satellites {
            if !satellite.pending.iter().any(|p| &p.contact == contact) {
                satellite.pending.push(Enrolment {
                    contact: *contact,
                    token: None,
                    since: now,
                });
            }
        }
    }

    /// Drops what is older than 24 hours in the outboxes and parked
    /// messages of the channels, and enrolments given up.
    pub(crate) fn expire_satellites(&mut self, now: u64) {
        let fresh = |since: u64| now < since.saturating_add(SYNC_KEEP_SECS);
        for satellite in &mut self.satellites {
            satellite.outbox.retain(|o| fresh(o.created_at));
            satellite.parked.retain(|p| fresh(p.received_at));
            satellite.pending.retain(|p| fresh(p.since));
        }
        self.satellite_transfers.retain(|t| fresh(t.first_seen));
    }

    /// Handles an envelope from the channel mailbox `mailbox`.
    ///
    /// Errors mean it is not a valid message of the channel; the caller
    /// drops it silently, as it drops everything that is not an up type.
    pub fn receive_channel(
        &mut self,
        mailbox: &MailboxId,
        envelope: &[u8],
        now: u64,
    ) -> Result<Vec<ChannelEvent>, Error> {
        self.expire_satellites(now);
        let satellite = self
            .satellites
            .iter_mut()
            .find(|s| &s.mailbox == mailbox)
            .ok_or(Error::Unexpected)?;
        let message = crypto::open_group(envelope, &satellite.key)?;
        check_time(&message, now)?;
        // Down types, own ones included, are not remembered: they are
        // never applied here.
        if !message.body.is_up() {
            return Ok(vec![ChannelEvent::Ignored(ChannelIgnored::NotUp)]);
        }
        if remember(&mut satellite.seen, &message.id, message.sent_at, now) {
            return Ok(vec![ChannelEvent::Ignored(ChannelIgnored::Duplicate)]);
        }
        let reader = satellite.reader;
        let envelope: [u8; ENVELOPE_LEN] = envelope.try_into().expect("checked on opening");
        Ok(vec![self.apply_up(reader, message, Some(envelope), now)])
    }

    /// Applies an up message. `envelope` is given for a message just
    /// received, so that it can be parked.
    fn apply_up(
        &mut self,
        satellite: ReaderId,
        message: Message,
        envelope: Option<[u8; ENVELOPE_LEN]>,
        now: u64,
    ) -> ChannelEvent {
        let stamp = Stamp {
            at: message.sent_at,
            id: message.id,
        };
        match message.body {
            Body::SatStateShared(shared) => {
                channel_state(satellite, self.apply_shared_state(Some(shared), stamp))
            }
            Body::SatStateCleared => channel_state(satellite, self.apply_shared_state(None, stamp)),
            Body::SatAnswered(_) | Body::SatAsked(_) => {
                let contact = *message.body.channel_contact().expect("both have one");
                let Some(stored) = self.contact_by_mailbox_mut(&contact) else {
                    if let Some(envelope) = envelope
                        && let Some(s) = self.satellite_mut(&satellite)
                    {
                        s.parked.push(Parked {
                            envelope,
                            received_at: now,
                        });
                    }
                    return ChannelEvent::Parked;
                };
                let state = &mut stored.state;
                match message.body {
                    Body::SatAnswered(r) => {
                        // The satellite's clock is not trusted beyond the
                        // own one.
                        let at = r.at.min(now);
                        state.apply_responded(&r.request, &r.kind, at);
                        ChannelEvent::Answered {
                            satellite,
                            contact,
                            request: r.request,
                            kind: r.kind,
                            at,
                        }
                    }
                    Body::SatAsked(r) => {
                        state.apply_requested(r.request, r.at.min(now), now);
                        ChannelEvent::Asked {
                            satellite,
                            contact,
                            request: r.request,
                        }
                    }
                    _ => unreachable!("matched above"),
                }
            }
            _ => ChannelEvent::Ignored(ChannelIgnored::NotUp),
        }
    }

    /// Applies the parked up messages about a contact that just arrived.
    pub(crate) fn apply_parked_up(&mut self, contact: &MailboxId, now: u64) -> Vec<ChannelEvent> {
        let mut events = Vec::new();
        for i in 0..self.satellites.len() {
            let satellite = &mut self.satellites[i];
            let reader = satellite.reader;
            let key = satellite.key.clone();
            for parked in std::mem::take(&mut satellite.parked) {
                let Ok(message) = crypto::open_group(&parked.envelope, &key) else {
                    continue;
                };
                if message.body.channel_contact() == Some(contact) {
                    events.push(self.apply_up(reader, message, None, now));
                } else {
                    self.satellites[i].parked.push(parked);
                }
            }
        }
        events
    }

    /// Applies `satellite_settings` from another full device, if newer.
    pub(crate) fn apply_satellite_settings(&mut self, set: SettingsFor, stamp: Stamp) -> bool {
        let Some(satellite) = self.satellite_mut(&set.reader) else {
            return false;
        };
        if stamp <= satellite.settings_stamp {
            return false;
        }
        satellite.settings = set.settings;
        satellite.settings_stamp = stamp;
        true
    }

    /// Applies `satellite_contact_settings` from another full device, if
    /// newer.
    pub(crate) fn apply_satellite_contact(
        &mut self,
        set: ContactSettingsFor,
        stamp: Stamp,
    ) -> bool {
        let Some(satellite) = self.satellite_mut(&set.reader) else {
            return false;
        };
        let contact = set.settings.contact;
        if stamp <= satellite.contact_stamp(&contact) {
            return false;
        }
        satellite.set_contact(contact, set.settings.of, stamp);
        true
    }

    /// A satellite from its record, as another full device sent it.
    pub(crate) fn satellite_from_record(reader: ReaderId, record: Record) -> Satellite {
        Satellite::from_parts(
            reader,
            record.name.as_str().to_owned(),
            record.relay,
            record.mailbox,
            record.enrolment_token,
            record.send_token,
            GroupKey::from_bytes(record.key),
        )
    }
}

#[cfg(test)]
mod tests {
    use rand_core::OsRng;

    use super::{ChannelEvent, ChannelIgnored, Satellite, settings_valid};
    use crate::crypto;
    use crate::group::SyncEvent;
    use crate::group::tests::{T, device_with_group, pair};
    use crate::wire::satellite::{ContactRecord, SetupHeader, WifiName, WifiPassword};
    use crate::wire::{
        Body, ContactFlags, Message, Preset, Reason, RelayUrl, Requested, Responded, ResponseKind,
        SatelliteSettings, StateShared,
    };

    const ANNA: [u8; 32] = [1; 32];
    const SAT: [u8; 16] = [0x5A; 16];

    fn with_satellite() -> (crate::store::Device, crate::contact::ContactState) {
        let mut device = device_with_group();
        let (stored, peer) = pair(1, "Anna");
        device.contacts.push(stored);
        let satellite = Satellite::new(
            SAT,
            "Hallway".into(),
            RelayUrl::new("https://relay.example").unwrap(),
            [0xC0; 32],
            [0xC1; 32],
            [0xC2; 32],
            &mut OsRng,
        );
        device.add_satellite(satellite).unwrap();
        (device, peer)
    }

    fn up(device: &crate::store::Device, id: u8, at: u64, body: Body) -> Vec<u8> {
        let key = &device.satellite(&SAT).unwrap().key;
        let message = Message {
            id: [id; 16],
            sent_at: at,
            body,
        };
        crypto::seal_group(&message, key, &mut OsRng)
            .unwrap()
            .to_vec()
    }

    fn opened(device: &crate::store::Device) -> Vec<Message> {
        let satellite = device.satellite(&SAT).unwrap();
        satellite
            .outbox
            .iter()
            .map(|o| crypto::open_group(&o.envelope, &satellite.key).unwrap())
            .collect()
    }

    #[test]
    fn sync_messages_are_mirrored_down_with_the_same_stamp() {
        let (mut device, _) = with_satellite();
        device
            .sync_requested(&ANNA, [7; 16], T, T, &mut OsRng)
            .unwrap();
        device
            .sync_responded(&ANNA, [8; 16], ResponseKind::Manual, T, T, &mut OsRng)
            .unwrap();
        device.contacts[0]
            .state
            .set_rules(crate::wire::Rules::DEFAULT, T, &mut OsRng)
            .unwrap();
        device.sync_rules_set(&ANNA, T, &mut OsRng).unwrap();
        device
            .share_state(Reason::new("asleep").unwrap(), 60, T, &mut OsRng)
            .unwrap();
        device.clear_shared_state(T + 1, &mut OsRng).unwrap();
        device.sync_contact_removed(&ANNA, T, &mut OsRng).unwrap();
        let kinds: Vec<u8> = opened(&device)
            .iter()
            .map(|m| m.body.kind().unwrap())
            .collect();
        assert_eq!(kinds, [90, 89, 91, 94, 95]);
        let stamp = device.contacts[0].state.own_rules_stamp;
        let rules = &opened(&device)[2];
        assert_eq!((rules.sent_at, rules.id), (stamp.at, stamp.id));
        assert_eq!(device.outbox.len(), 6, "the sync messages as before");
        // Without a group the down messages still go out.
        device.group = None;
        device
            .sync_requested(&ANNA, [9; 16], T, T, &mut OsRng)
            .unwrap();
        assert_eq!(opened(&device).len(), 6);
    }

    #[test]
    fn full_devices_apply_only_up_types() {
        let (mut device, mut peer) = with_satellite();
        let (id, request) = peer.request(T, &mut OsRng).unwrap();
        device.contacts[0].state.receive(&request, T).unwrap();
        let mailbox = device.satellite(&SAT).unwrap().mailbox;

        // Down types, also replays of own ones, and contact types: dropped.
        for body in [
            Body::HomeStateCleared,
            Body::HomeSendToken([1; 32]),
            Body::Request,
            Body::StateCleared,
            Body::Responded(Responded {
                contact: ANNA,
                request: id,
                kind: ResponseKind::Manual,
                at: T,
            }),
        ] {
            let envelope = up(&device, 1, T, body);
            assert_eq!(
                device.receive_channel(&mailbox, &envelope, T).unwrap(),
                [ChannelEvent::Ignored(ChannelIgnored::NotUp)]
            );
        }
        assert_eq!(device.contacts[0].state.open_request(T), Some(id));
        // A message under another key does not open.
        let foreign = crypto::seal_group(
            &Message {
                id: [2; 16],
                sent_at: T,
                body: Body::SatStateCleared,
            },
            &crypto::GroupKey::generate(&mut OsRng),
            &mut OsRng,
        )
        .unwrap();
        assert!(device.receive_channel(&mailbox, &foreign, T).is_err());

        let answered = up(
            &device,
            3,
            T + 60,
            Body::SatAnswered(Responded {
                contact: ANNA,
                request: id,
                kind: ResponseKind::Manual,
                at: T + 60,
            }),
        );
        assert!(matches!(
            device.receive_channel(&mailbox, &answered, T + 61).unwrap()[..],
            [ChannelEvent::Answered { .. }]
        ));
        assert_eq!(device.contacts[0].state.open_request(T + 61), None);
        assert_eq!(
            device.receive_channel(&mailbox, &answered, T + 62).unwrap(),
            [ChannelEvent::Ignored(ChannelIgnored::Duplicate)]
        );
    }

    #[test]
    fn asked_and_shared_states_from_the_satellite() {
        let (mut device, _) = with_satellite();
        let mailbox = device.satellite(&SAT).unwrap().mailbox;
        let asked = up(
            &device,
            1,
            T,
            Body::SatAsked(Requested {
                contact: ANNA,
                request: [4; 16],
                at: T,
            }),
        );
        device.receive_channel(&mailbox, &asked, T).unwrap();
        assert!(device.contacts[0].state.next_request_at(T) > T);

        let shared = up(
            &device,
            2,
            T + 5,
            Body::SatStateShared(StateShared {
                reason: Reason::new("asleep").unwrap(),
                until: T + 3600,
            }),
        );
        assert_eq!(
            device.receive_channel(&mailbox, &shared, T + 5).unwrap(),
            [ChannelEvent::StateHanded {
                satellite: SAT,
                reason: Some("asleep".into())
            }]
        );
        assert!(device.current_state(T + 6).unwrap().handed);
        let older = up(&device, 3, T + 4, Body::SatStateCleared);
        assert_eq!(
            device.receive_channel(&mailbox, &older, T + 6).unwrap(),
            [ChannelEvent::Ignored(ChannelIgnored::Outdated)]
        );
        let cleared = up(&device, 4, T + 7, Body::SatStateCleared);
        device.receive_channel(&mailbox, &cleared, T + 7).unwrap();
        assert_eq!(device.current_state(T + 8), None);
    }

    #[test]
    fn up_messages_about_unknown_contacts_are_parked() {
        let (mut device, _) = with_satellite();
        let mailbox = device.satellite(&SAT).unwrap().mailbox;
        let bert = [2; 32];
        let asked = up(
            &device,
            1,
            T,
            Body::SatAsked(Requested {
                contact: bert,
                request: [4; 16],
                at: T,
            }),
        );
        assert_eq!(
            device.receive_channel(&mailbox, &asked, T).unwrap(),
            [ChannelEvent::Parked]
        );
        let (stored, _) = pair(2, "Bert");
        device.contacts.push(stored);
        let events = device.apply_parked_up(&bert, T + 1);
        assert!(matches!(events[..], [ChannelEvent::Asked { .. }]));
        assert!(device.satellite(&SAT).unwrap().parked.is_empty());
    }

    #[test]
    fn settings_go_down_and_to_the_other_devices() {
        let (mut device, _) = with_satellite();
        let mut other =
            crate::store::Device::from_handover(device.handover([0; 32]).unwrap(), [9; 16]);
        assert_eq!(other.satellites.len(), 1);
        let settings = SatelliteSettings {
            waiting_minutes: 30,
            preset: Some(Preset {
                minutes: 480,
                share: true,
                reason: Reason::new("asleep").unwrap(),
            }),
        };
        device
            .set_satellite_settings(&SAT, settings, T, &mut OsRng)
            .unwrap();
        let flags = ContactFlags {
            reported: true,
            ask_button: 1,
        };
        device
            .set_satellite_contact(&SAT, &ANNA, flags, T, &mut OsRng)
            .unwrap();
        let down: Vec<u8> = opened(&device)
            .iter()
            .map(|m| m.body.kind().unwrap())
            .collect();
        assert_eq!(down, [93, 92]);
        let events: Vec<SyncEvent> = std::mem::take(&mut device.outbox)
            .iter()
            .flat_map(|o| other.receive_sync(&o.envelope, T).unwrap())
            .collect();
        assert_eq!(
            events,
            [
                SyncEvent::SatelliteSettings { satellite: SAT },
                SyncEvent::SatelliteContactSettings {
                    satellite: SAT,
                    contact: ANNA
                }
            ]
        );
        let there = other.satellite(&SAT).unwrap();
        assert_eq!(there.settings, settings);
        assert_eq!(there.contact_flags(&ANNA), flags);
        assert!(there.outbox.is_empty(), "nothing is passed on");

        // Binding the button to another contact frees it at the first.
        let (stored, _) = pair(2, "Bert");
        device.contacts.push(stored);
        device
            .set_satellite_contact(&SAT, &[2; 32], flags, T + 1, &mut OsRng)
            .unwrap();
        let satellite = device.satellite(&SAT).unwrap();
        assert_eq!(satellite.contact_flags(&ANNA).ask_button, 0);
        assert!(satellite.contact_flags(&ANNA).reported);
        assert_eq!(satellite.contact_flags(&[2; 32]).ask_button, 1);

        // Invalid settings are refused.
        let mut bad = settings;
        bad.waiting_minutes = 0;
        assert!(!settings_valid(&bad));
        assert!(
            device
                .set_satellite_settings(&SAT, bad, T, &mut OsRng)
                .is_err()
        );
    }

    #[test]
    fn satellite_part_adds_the_satellite_on_the_other_device() {
        let (mut device, _) = with_satellite();
        let mut other =
            crate::store::Device::from_handover(device.handover([0; 32]).unwrap(), [9; 16]);
        other.satellites.clear();
        // Long enough for two parts.
        device.satellite_mut(&SAT).unwrap().relay = RelayUrl::new(&"r".repeat(120)).unwrap();
        device.announce_satellite(&SAT, T, &mut OsRng).unwrap();
        assert_eq!(device.outbox.len(), 2);
        let events: Vec<SyncEvent> = std::mem::take(&mut device.outbox)
            .iter()
            .rev()
            .flat_map(|o| other.receive_sync(&o.envelope, T).unwrap())
            .collect();
        assert_eq!(
            events,
            [
                SyncEvent::PartStored,
                SyncEvent::SatelliteAdded {
                    satellite: SAT,
                    name: "Hallway".into()
                }
            ]
        );
        let there = other.satellite(&SAT).unwrap();
        let here = device.satellite(&SAT).unwrap();
        assert_eq!(there.key, here.key);
        assert_eq!(there.read_token, here.read_token);
        assert_eq!(there.mailbox, here.mailbox);
        assert!(other.satellite_transfers.is_empty());
    }

    #[test]
    fn contact_record_and_setup_carry_the_contact() {
        let (mut device, _) = with_satellite();
        let (id, _) = device.contacts[0].state.request(T, &mut OsRng).unwrap();
        let record = device
            .satellite_contact_record(&SAT, &ANNA, [0xE0; 32], T + 1)
            .unwrap();
        assert_eq!(record.enrolment_token, [0xE0; 32]);
        assert_eq!(record.open_request.unwrap().id, id);
        assert_eq!(record.recent.as_slice(), [T]);
        assert_eq!(record.name.as_str(), "Anna");

        device
            .queue_satellite_contact(&SAT, &ANNA, [0xE0; 32], T + 1, &mut OsRng)
            .unwrap();
        let parts = opened(&device);
        assert_eq!(parts.len(), 2);
        let bytes: Vec<u8> = parts
            .iter()
            .flat_map(|m| match &m.body {
                Body::HomeContactPart(p) => p.data.as_bytes().to_vec(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(ContactRecord::from_bytes(&bytes).unwrap(), record);
        assert_eq!(parts.len(), bytes.len().div_ceil(crate::wire::MAX_PART_LEN));

        let setup = device
            .satellite_setup(
                &SAT,
                WifiName::new("home").unwrap(),
                WifiPassword::new("pw").unwrap(),
                [0xE1; 32],
                &[(ANNA, [0xE0; 32])],
                T + 1,
            )
            .unwrap();
        let (header, mut entries) = SetupHeader::from_bytes(&setup).unwrap();
        assert_eq!(header.reader, SAT);
        assert_eq!(header.enrolment_token, [0xE1; 32]);
        assert_eq!(header.contacts, 1);
        let (contact, read) = entries.next().unwrap().unwrap();
        assert_eq!((contact, read), (ANNA, record));
    }

    #[test]
    fn handover_leaves_out_what_only_this_device_knows() {
        let (mut device, _) = with_satellite();
        let satellite = device.satellite_mut(&SAT).unwrap();
        satellite.own_token = Some([0x71; 32]);
        satellite.new_send_token = Some([0x72; 32]);
        device
            .sync_requested(&ANNA, [7; 16], T, T, &mut OsRng)
            .unwrap();
        let handover = device.handover([0; 32]).unwrap();
        let text = String::from_utf8(handover.to_json()).unwrap();
        assert!(!text.contains(&crate::hex::encode(&[0x71; 32])));
        assert!(!text.contains(&crate::hex::encode(&[0x72; 32])));
        assert!(handover.satellites[0].outbox.is_empty());
        assert_eq!(handover.satellites[0].read_token, [0xC1; 32]);
    }
}
