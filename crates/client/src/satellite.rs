//! Satellites on a full device (`docs/m10-satellite.md`, part 2): setting
//! one up, its settings, removing it, and the network work of its channel.
//!
//! Setting up registers the satellite's reader, with role `satellite` and
//! a fresh enrolment token, on the channel and on every own contact
//! mailbox, and returns the setup (channel, settings, all contact records)
//! that BLE or, for development, USB serial carries to it. Contacts paired
//! later are registered for it and sent down by the device that paired
//! them. Removing revokes it everywhere and deletes the channel, which the
//! satellite takes as the sign to wipe itself.

use std::collections::{BTreeSet, HashMap};

use lz_core::satellite::{MAX_SATELLITE_RELAYS, Satellite, settings_valid};
use lz_core::store::{Device, ReadAccess, RelayAccount};
use lz_core::wire::satellite::{WifiName, WifiPassword};
use lz_core::wire::{ContactFlags, MailboxId, Preset, Reason, RelayUrl, Token};
use rand_core::{OsRng, RngCore};
use serde::Serialize;

use crate::ctx::{Ctx, Result, core, random_token};
use crate::devices::{device_name, from_hex, to_hex};
use crate::relay::{MailboxRead, ReaderChange, ReaderId, Relay, RelayError, Role};
use crate::show::{Notice, minutes};
use crate::tokens::{Own, Taken, held};

/// What setting up a satellite produced.
#[derive(Clone, Debug)]
pub struct SatelliteSetup {
    /// The setup for the satellite. It holds keys and the WiFi password.
    pub bytes: Vec<u8>,
    /// Reader identifier of the satellite.
    pub reader: ReaderId,
    /// Names of the contacts it got.
    pub contacts: Vec<String>,
    /// Names of the contacts it did not get, as their relay lacks
    /// `reader_tokens`.
    pub old_relays: Vec<String>,
}

/// A satellite for display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SatelliteView {
    pub name: String,
    /// Reader identifier in hex.
    pub reader: String,
    pub waiting_minutes: u16,
    pub preset: Option<PresetView>,
    /// Settings per contact, for every contact.
    pub contacts: Vec<SatelliteContactView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PresetView {
    pub minutes: u32,
    pub share: bool,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SatelliteContactView {
    pub name: String,
    pub reported: bool,
    pub ask_button: u8,
}

/// A change of the long-press preset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresetChange {
    /// No preset.
    None,
    Set {
        minutes: u32,
        share: bool,
        reason: String,
    },
}

/// The own mailboxes of paired contacts, with their names and relays.
fn paired_contacts(device: &Device) -> Vec<(MailboxId, String, RelayUrl, Token)> {
    device
        .contacts
        .iter()
        .filter(|c| !c.state.ended && c.state.is_paired())
        .map(|c| {
            (
                c.state.contact.own_mailbox,
                c.name.clone(),
                c.own_relay,
                c.token(),
            )
        })
        .collect()
}

impl<R: Relay> Ctx<R> {
    /// Sets up a satellite named `name` with reader `reader` (drawn if
    /// `None`) and returns its setup. Refuses a relay for the channel
    /// without `reader_tokens` and more than 3 relays (decision 9);
    /// contacts whose own mailbox lies on a relay without `reader_tokens`
    /// are left out and named. On failure nothing is left behind.
    pub async fn add_satellite(
        &mut self,
        name: &str,
        reader: Option<ReaderId>,
        wifi_name: &str,
        wifi_password: &str,
    ) -> Result<SatelliteSetup> {
        let t = self.texts();
        let name = device_name(t, name)?;
        let wifi = WifiName::new(wifi_name)
            .and_then(|n| WifiPassword::new(wifi_password).map(|p| (n, p)))
            .map_err(|_| fail!(t, "error-satellite-wifi"))?;
        let device = self.load()?;
        let taken = device.satellites.iter().any(|s| s.name == name)
            || device.devices.iter().any(|d| d.name == name)
            || device.name.as_deref() == Some(name.as_str());
        if taken {
            return Err(fail!(t, "error-satellite-exists", name = &name));
        }
        let reader = reader.unwrap_or_else(|| {
            let mut reader = [0; 16];
            OsRng.fill_bytes(&mut reader);
            reader
        });
        let known = reader == device.reader_id
            || device.devices.iter().any(|d| d.reader == reader)
            || device.satellite(&reader).is_some();
        if known {
            return Err(fail!(t, "error-satellite-reader"));
        }

        // Which contacts it can get, and on how many relays.
        let channel_relay = self.channel_relay(&device).await?;
        let mut offers: HashMap<RelayUrl, bool> = HashMap::new();
        let mut contacts = Vec::new();
        let mut old_relays = Vec::new();
        let mut relays = BTreeSet::from([channel_relay.to_string()]);
        for (mailbox, contact_name, relay, token) in paired_contacts(&device) {
            let offered = match offers.get(&relay) {
                Some(&offered) => offered,
                None => {
                    let offered = self
                        .reader_tokens(&relay)
                        .await
                        .map_err(|e| fail!(t, "error-satellite-not-set-up", error = &e.text(t)))?;
                    offers.insert(relay, offered);
                    offered
                }
            };
            if offered {
                relays.insert(relay.to_string());
                contacts.push((mailbox, contact_name, relay, token));
            } else {
                old_relays.push(contact_name);
            }
        }
        if relays.len() > MAX_SATELLITE_RELAYS {
            return Err(fail!(
                t,
                "error-satellite-too-many-relays",
                max = MAX_SATELLITE_RELAYS,
                count = relays.len()
            ));
        }

        // The channel, in the old form: its shared token is the enrolment
        // token of the other full devices, as for contact mailboxes.
        let shared = random_token();
        let send_token = random_token();
        let not_set_up =
            |error: RelayError| fail!(t, "error-satellite-not-set-up", error = &error.text(t));
        let mailbox = self
            .create_channel(&device, &channel_relay, &shared, &send_token)
            .await?;
        let own = match self
            .join_new_mailbox(&channel_relay, &mailbox, &shared, &device)
            .await
        {
            Ok(Some(own)) => own,
            Ok(None) => {
                self.delete_own(&channel_relay, &mailbox, &shared).await;
                return Err(fail!(
                    t,
                    "error-satellite-old-relay",
                    relay = &channel_relay.to_string()
                ));
            }
            Err(error) => {
                self.delete_own(&channel_relay, &mailbox, &shared).await;
                return Err(not_set_up(error));
            }
        };
        let channel_enrolment = random_token();
        let mut registered: Vec<(RelayUrl, MailboxId, Token)> = Vec::new();
        let mut enrolment = Vec::new();
        let mut failure = self
            .register_satellite(&channel_relay, &mailbox, &own, &reader, &channel_enrolment)
            .await
            .err();
        if failure.is_none() {
            for (contact, _, relay, token) in &contacts {
                let token_for_it = random_token();
                match self
                    .register_satellite(relay, contact, token, &reader, &token_for_it)
                    .await
                {
                    Ok(()) => {
                        registered.push((*relay, *contact, *token));
                        enrolment.push((*contact, token_for_it));
                    }
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
        }
        if let Some(error) = failure {
            self.undo_satellite(&reader, &registered, &channel_relay, &mailbox, &own)
                .await;
            return Err(not_set_up(error));
        }

        let now = self.now();
        let mut satellite = Satellite::new(
            reader,
            name.clone(),
            channel_relay,
            mailbox,
            shared,
            send_token,
            &mut OsRng,
        );
        satellite.own_token = Some(own);
        let stored = self.update(|device| {
            device.add_satellite(satellite).map_err(core)?;
            device
                .announce_satellite(&reader, now, &mut OsRng)
                .map_err(core)?;
            device
                .satellite_setup(&reader, wifi.0, wifi.1, channel_enrolment, &enrolment, now)
                .map_err(core)
        });
        let bytes = match stored {
            Ok(bytes) => bytes,
            Err(error) => {
                self.undo_satellite(&reader, &registered, &channel_relay, &mailbox, &own)
                    .await;
                return Err(error);
            }
        };
        self.flush_outbox().await?;
        let names: Vec<String> = contacts.into_iter().map(|c| c.1).collect();
        if !old_relays.is_empty() {
            let notice = Notice::SatelliteOldRelay {
                satellite: name.clone(),
                names: old_relays.clone(),
            };
            self.show(&notice);
        }
        self.say(&tr!(
            t,
            "out-satellite-added",
            name = &name,
            contacts = names.len()
        ));
        Ok(SatelliteSetup {
            bytes,
            reader,
            contacts: names,
            old_relays,
        })
    }

    /// The relay for a new channel: that of the relay account, if it has an
    /// access token, else that of an own mailbox created with a voucher.
    async fn channel_relay(&self, device: &Device) -> Result<RelayUrl> {
        let t = self.texts();
        let relay = match &device.relay {
            Some(RelayAccount {
                url,
                access_token: Some(_),
            }) => Some(*url),
            _ => device
                .contacts
                .iter()
                .find(|c| !c.state.ended)
                .map(|c| c.own_relay),
        }
        .ok_or_else(|| fail!(t, "error-satellite-needs-access"))?;
        match self.reader_tokens(&relay).await {
            Ok(true) => Ok(relay),
            Ok(false) => Err(fail!(
                t,
                "error-satellite-old-relay",
                relay = &relay.to_string()
            )),
            Err(error) => Err(fail!(
                t,
                "error-satellite-not-set-up",
                error = &error.text(t)
            )),
        }
    }

    /// Creates the channel mailbox on `relay` in the old form, with the
    /// access token or derived from an own voucher mailbox there.
    async fn create_channel(
        &self,
        device: &Device,
        relay: &RelayUrl,
        shared: &Token,
        send_token: &Token,
    ) -> Result<MailboxId> {
        let t = self.texts();
        let not_set_up =
            |error: RelayError| fail!(t, "error-satellite-not-set-up", error = &error.text(t));
        if let Some(RelayAccount {
            url,
            access_token: Some(access),
        }) = &device.relay
            && url == relay
        {
            return self
                .relay
                .create_mailbox(relay, access, MailboxRead::Shared(shared), send_token)
                .await
                .map_err(not_set_up);
        }
        for contact in device
            .contacts
            .iter()
            .filter(|c| !c.state.ended && &c.own_relay == relay)
        {
            match self
                .relay
                .create_mailbox(
                    relay,
                    &contact.token(),
                    MailboxRead::Shared(shared),
                    send_token,
                )
                .await
            {
                Ok(mailbox) => return Ok(mailbox),
                Err(RelayError::Refused {
                    status: 401 | 429, ..
                }) => continue,
                Err(error) => return Err(not_set_up(error)),
            }
        }
        Err(fail!(t, "error-satellite-needs-access"))
    }

    /// Registers the satellite's reader on a mailbox, with role
    /// `satellite`, its enrolment token and no push address.
    async fn register_satellite(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        token: &Token,
        reader: &ReaderId,
        enrolment: &Token,
    ) -> std::result::Result<(), RelayError> {
        let change = ReaderChange {
            push_url: None,
            token: Some(enrolment),
            role: Some(Role::Satellite),
        };
        self.relay
            .put_reader(relay, mailbox, token, reader, change)
            .await
    }

    /// Takes back a setup that failed halfway. Best effort.
    async fn undo_satellite(
        &mut self,
        reader: &ReaderId,
        registered: &[(RelayUrl, MailboxId, Token)],
        channel_relay: &RelayUrl,
        channel: &MailboxId,
        own: &Token,
    ) {
        for (relay, mailbox, token) in registered {
            let _ = self
                .relay
                .delete_reader(relay, mailbox, token, reader)
                .await;
        }
        self.delete_own(channel_relay, channel, own).await;
    }

    /// The satellites of the person.
    pub fn satellites(&self) -> Result<Vec<SatelliteView>> {
        let device = self.load()?;
        Ok(device
            .satellites
            .iter()
            .map(|s| SatelliteView {
                name: s.name.clone(),
                reader: to_hex(&s.reader),
                waiting_minutes: s.settings.waiting_minutes,
                preset: s.settings.preset.map(|p| PresetView {
                    minutes: p.minutes,
                    share: p.share,
                    reason: p.reason.to_string(),
                }),
                contacts: device
                    .contacts
                    .iter()
                    .filter(|c| !c.state.ended && c.state.is_paired())
                    .map(|c| {
                        let flags = s.contact_flags(&c.state.contact.own_mailbox);
                        SatelliteContactView {
                            name: c.name.clone(),
                            reported: flags.reported,
                            ask_button: flags.ask_button,
                        }
                    })
                    .collect(),
            })
            .collect())
    }

    /// `lz satellite list`.
    pub fn list_satellites(&mut self) -> Result<()> {
        let t = self.texts();
        let satellites = self.satellites()?;
        if satellites.is_empty() {
            self.say(&tr!(t, "out-no-satellites"));
        }
        let yes_no = |b: bool| {
            tr!(
                t,
                if b {
                    "out-satellite-yes"
                } else {
                    "out-satellite-no"
                }
            )
        };
        for satellite in satellites {
            let preset = match &satellite.preset {
                Some(p) => tr!(
                    t,
                    "out-satellite-preset",
                    reason = &p.reason,
                    duration = &minutes(t, p.minutes),
                    share = &if p.share {
                        tr!(t, "out-satellite-preset-shared")
                    } else {
                        String::new()
                    }
                ),
                None => tr!(t, "out-satellite-no-preset"),
            };
            let line = tr!(
                t,
                "out-satellite-line",
                name = &satellite.name,
                wait = &minutes(t, satellite.waiting_minutes.into()),
                preset = &preset
            );
            self.say(&line);
            for contact in &satellite.contacts {
                let line = tr!(
                    t,
                    "out-satellite-contact-line",
                    name = &contact.name,
                    reported = &yes_no(contact.reported),
                    button = &contact.ask_button.to_string()
                );
                self.say(&line);
            }
        }
        Ok(())
    }

    fn satellite_named(&self, device: &Device, name: &str) -> Result<ReaderId> {
        let t = self.texts();
        device
            .satellites
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.reader)
            .ok_or_else(|| fail!(t, "error-satellite-unknown", name = name))
    }

    /// Changes the waiting time and the long-press preset of a satellite;
    /// what is `None` stays. The satellite and the other full devices
    /// learn the new settings.
    pub async fn set_satellite_settings(
        &mut self,
        name: &str,
        waiting_minutes: Option<u16>,
        preset: Option<PresetChange>,
    ) -> Result<()> {
        let t = self.texts();
        let device = self.load()?;
        let reader = self.satellite_named(&device, name)?;
        let mut settings = device.satellite(&reader).expect("found").settings;
        if let Some(waiting) = waiting_minutes {
            settings.waiting_minutes = waiting;
        }
        match preset {
            None => {}
            Some(PresetChange::None) => settings.preset = None,
            Some(PresetChange::Set {
                minutes,
                share,
                reason,
            }) => {
                let reason =
                    Reason::new(reason.trim()).map_err(|_| fail!(t, "error-satellite-settings"))?;
                settings.preset = Some(Preset {
                    minutes,
                    share,
                    reason,
                });
            }
        }
        if !settings_valid(&settings) {
            return Err(fail!(t, "error-satellite-settings"));
        }
        self.change_satellite(|device, now| {
            device.set_satellite_settings(&reader, settings, now, &mut OsRng)
        })
        .await?;
        self.say(&tr!(t, "out-satellite-settings-changed", name = name));
        Ok(())
    }

    /// Changes what a satellite does for a contact; what is `None` stays.
    pub async fn set_satellite_contact(
        &mut self,
        name: &str,
        contact: &str,
        reported: Option<bool>,
        ask_button: Option<u8>,
    ) -> Result<()> {
        let t = self.texts();
        let device = self.load()?;
        let reader = self.satellite_named(&device, name)?;
        let mailbox = self.own_mailbox_of(contact)?;
        let current = device
            .satellite(&reader)
            .expect("found")
            .contact_flags(&mailbox);
        let flags = ContactFlags {
            reported: reported.unwrap_or(current.reported),
            ask_button: ask_button.unwrap_or(current.ask_button),
        };
        self.change_satellite(|device, now| {
            device.set_satellite_contact(&reader, &mailbox, flags, now, &mut OsRng)
        })
        .await?;
        self.say(&tr!(t, "out-satellite-settings-changed", name = name));
        Ok(())
    }

    /// Applies a change that queues messages for the channel and the sync
    /// mailbox, and sends them.
    async fn change_satellite(
        &mut self,
        change: impl FnOnce(&mut Device, u64) -> std::result::Result<(), lz_core::Error>,
    ) -> Result<()> {
        let now = self.now();
        self.update(|device| change(device, now).map_err(core))?;
        self.flush_outbox().await
    }

    /// Removes a satellite: revokes its reader on every own contact mailbox
    /// and on the channel, then deletes the channel. The group stays. Each
    /// step can be repeated; the satellite is forgotten only once all
    /// succeeded.
    pub async fn remove_satellite(&mut self, name: &str) -> Result<()> {
        let device = self.load()?;
        let reader = self.satellite_named(&device, name)?;
        self.remove_satellite_reader(&reader, name).await
    }

    pub(crate) async fn remove_satellite_reader(
        &mut self,
        reader: &ReaderId,
        name: &str,
    ) -> Result<()> {
        let t = self.texts();
        let device = self.load()?;
        let satellite = device
            .satellite(reader)
            .expect("checked by the caller")
            .clone();
        let mut first_error = None;
        let mut owns: Vec<Own> = device
            .contacts
            .iter()
            .filter(|c| !c.state.ended)
            .map(|c| Own::Contact(c.state.contact.own_mailbox))
            .collect();
        owns.push(Own::Channel(satellite.mailbox));
        for own in owns {
            let Some(held) = held(&self.load()?, own) else {
                continue;
            };
            match self
                .relay
                .delete_reader(&held.relay, &held.mailbox, &held.token, reader)
                .await
            {
                Ok(()) | Err(RelayError::UnknownMailbox) => {}
                Err(RelayError::Refused { status: 404, .. }) => {}
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        if first_error.is_none() {
            let token = satellite.token();
            match self
                .relay
                .delete_mailbox(&satellite.relay, &satellite.mailbox, &token)
                .await
            {
                Ok(()) | Err(RelayError::UnknownMailbox) => {}
                Err(error) => first_error = Some(error),
            }
        }
        if let Some(error) = first_error {
            return Err(fail!(
                t,
                "error-satellite-not-removed",
                name = name,
                error = &error.text(t)
            ));
        }
        self.update(|device| {
            device.forget_satellite(reader);
            Ok(())
        })?;
        self.say(&tr!(t, "out-satellite-removed", name = name));
        Ok(())
    }

    /// Registers the satellites on contacts paired on this device and
    /// queues their records (`home_contact_part`). What cannot be done now
    /// is tried again by the next call, for 24 hours.
    pub(crate) async fn enrol_satellites(&mut self) -> Result<()> {
        let device = self.load()?;
        if device.satellites.iter().all(|s| s.pending.is_empty()) {
            return Ok(());
        }
        let work: Vec<(ReaderId, String, MailboxId)> = device
            .satellites
            .iter()
            .flat_map(|s| {
                s.pending
                    .iter()
                    .map(move |p| (s.reader, s.name.clone(), p.contact))
            })
            .collect();
        for (reader, satellite_name, contact) in work {
            let device = self.load()?;
            let Some(satellite) = device.satellite(&reader) else {
                continue;
            };
            let stored = device
                .contact_by_mailbox(&contact)
                .filter(|c| !c.state.ended && c.state.is_paired());
            let Some(stored) = stored else {
                self.drop_enrolment(&reader, &contact)?;
                continue;
            };
            let (contact_name, relay, token) =
                (stored.name.clone(), stored.own_relay, stored.token());
            match self.reader_tokens(&relay).await {
                Ok(true) => {}
                Ok(false) => {
                    self.drop_enrolment(&reader, &contact)?;
                    let notice = Notice::SatelliteOldRelay {
                        satellite: satellite_name,
                        names: vec![contact_name],
                    };
                    self.show(&notice);
                    continue;
                }
                Err(_) => continue,
            }
            let mut relays: BTreeSet<String> = device
                .contacts
                .iter()
                .filter(|c| !c.state.ended && c.state.contact.own_mailbox != contact)
                .map(|c| c.own_relay.to_string())
                .collect();
            relays.insert(satellite.relay.to_string());
            relays.insert(relay.to_string());
            if relays.len() > MAX_SATELLITE_RELAYS {
                self.drop_enrolment(&reader, &contact)?;
                let notice = Notice::SatelliteTooManyRelays {
                    satellite: satellite_name,
                    names: vec![contact_name],
                };
                self.show(&notice);
                continue;
            }
            // Stored before it goes to the relay, so that a lost answer is
            // resolved by the next try with the same token.
            let enrolment = match satellite
                .pending
                .iter()
                .find(|p| p.contact == contact)
                .and_then(|p| p.token)
            {
                Some(token) => token,
                None => {
                    let fresh = random_token();
                    self.update(|device| {
                        if let Some(p) = device
                            .satellite_mut(&reader)
                            .and_then(|s| s.pending.iter_mut().find(|p| p.contact == contact))
                        {
                            p.token = Some(fresh);
                        }
                        Ok(())
                    })?;
                    fresh
                }
            };
            match self
                .register_satellite(&relay, &contact, &token, &reader, &enrolment)
                .await
            {
                Ok(()) => {
                    let now = self.now();
                    self.update(|device| {
                        device
                            .queue_satellite_contact(&reader, &contact, enrolment, now, &mut OsRng)
                            .map_err(core)?;
                        if let Some(s) = device.satellite_mut(&reader) {
                            s.pending.retain(|p| p.contact != contact);
                        }
                        Ok(())
                    })?;
                }
                Err(error) if error.is_transport() => {}
                Err(error) => {
                    self.drop_enrolment(&reader, &contact)?;
                    let t = self.texts();
                    let notice = Notice::Problem(tr!(
                        t,
                        "problem-not-registered",
                        name = &contact_name,
                        error = &error.text(t)
                    ));
                    self.show(&notice);
                }
            }
        }
        Ok(())
    }

    fn drop_enrolment(&self, reader: &ReaderId, contact: &MailboxId) -> Result<()> {
        self.update(|device| {
            if let Some(s) = device.satellite_mut(reader) {
                s.pending.retain(|p| &p.contact != contact);
            }
            Ok(())
        })
    }

    /// Delivers the outboxes of the channels, each in order up to its first
    /// refusal. A channel that is gone means the satellite was removed: it
    /// is forgotten. A refused send token (another full device replaced it
    /// after a removal, and this one was not added again) keeps the
    /// messages until they expire.
    pub(crate) async fn flush_channels(&mut self) -> Result<()> {
        let device = self.load()?;
        if device.satellites.is_empty() {
            return Ok(());
        }
        let now = self.now();
        for satellite in &device.satellites {
            if satellite.outbox.is_empty() {
                continue;
            }
            for outgoing in &satellite.outbox {
                let delivered = self
                    .relay
                    .deliver(
                        &satellite.relay,
                        &satellite.mailbox,
                        &satellite.send_token,
                        &outgoing.envelope,
                    )
                    .await;
                match delivered {
                    Ok(()) => {
                        self.update(|device| {
                            if let Some(s) = device.satellite_mut(&satellite.reader) {
                                s.outbox.retain(|o| o.envelope != outgoing.envelope);
                            }
                            Ok(())
                        })?;
                    }
                    Err(RelayError::UnknownMailbox) => {
                        self.satellite_gone(&satellite.reader)?;
                        break;
                    }
                    Err(_) => break,
                }
            }
        }
        self.update(|device| {
            device.expire_sync(now);
            Ok(())
        })
    }

    /// The channel of this satellite is gone: forgets it and says so.
    pub(crate) fn satellite_gone(&mut self, reader: &ReaderId) -> Result<Option<Notice>> {
        let forgotten = self.update(|device| Ok(device.forget_satellite(reader)))?;
        Ok(forgotten.map(|s| {
            let notice = Notice::SatelliteGone { name: s.name };
            self.show(&notice);
            notice
        }))
    }

    /// When a full device is removed: on the channel of every satellite,
    /// revokes every reader but `keep` and drops the shared token, then
    /// replaces the send token and tells the satellite. Returns the first
    /// error; the other channels are still done.
    pub(crate) async fn lock_out_of_channels(
        &mut self,
        keep: &[ReaderId],
        removed: &ReaderId,
        hidden: &mut usize,
    ) -> Result<Option<RelayError>> {
        let device = self.load()?;
        let mut first_error = None;
        for satellite in &device.satellites {
            let own = Own::Channel(satellite.mailbox);
            let result = match self.take_own_token(own).await? {
                Taken::Gone => {
                    self.satellite_gone(&satellite.reader)?;
                    continue;
                }
                Taken::Failed(error) => Err(error),
                // Not set up on such a relay; nothing to lock.
                Taken::Unsupported => Ok(()),
                Taken::Done => {
                    self.replace_channel_token(satellite.reader, keep, removed, hidden)
                        .await?
                }
            };
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
        Ok(first_error)
    }

    async fn replace_channel_token(
        &mut self,
        reader: ReaderId,
        keep: &[ReaderId],
        removed: &ReaderId,
        hidden: &mut usize,
    ) -> Result<std::result::Result<(), RelayError>> {
        let device = self.load()?;
        let Some(satellite) = device.satellite(&reader) else {
            return Ok(Ok(()));
        };
        let Some(held) = held(&device, Own::Channel(satellite.mailbox)) else {
            return Ok(Ok(()));
        };
        match self
            .relay
            .revoke_readers(&held.relay, &held.mailbox, &held.token, keep, true)
            .await
        {
            Ok(revoked) => *hidden += revoked.iter().filter(|r| *r != removed).count(),
            Err(RelayError::UnknownMailbox) => {
                self.satellite_gone(&reader)?;
                return Ok(Ok(()));
            }
            Err(error) => return Ok(Err(error)),
        }
        let new = match satellite.new_send_token {
            Some(token) => token,
            None => {
                let token = random_token();
                self.update(|device| {
                    if let Some(s) = device.satellite_mut(&reader) {
                        s.new_send_token = Some(token);
                    }
                    Ok(())
                })?;
                token
            }
        };
        if let Err(error) = self
            .relay
            .put_send_token(&held.relay, &held.mailbox, &held.token, &new)
            .await
        {
            return Ok(Err(error));
        }
        let now = self.now();
        self.update(|device| {
            device
                .channel_token_replaced(&reader, new, now, &mut OsRng)
                .map_err(core)
        })?;
        Ok(Ok(()))
    }
}

/// Reads a reader identifier in hex, for `lz satellite add --reader`.
pub fn reader_from_hex(t: crate::i18n::Texts, text: &str) -> Result<ReaderId> {
    from_hex(text.trim()).ok_or_else(|| fail!(t, "error-satellite-reader"))
}

/// The setup as hex lines of 64 digits, for a serial console.
pub fn setup_hex(bytes: &[u8]) -> String {
    bytes
        .chunks(32)
        .map(crate::relay::hex)
        .collect::<Vec<_>>()
        .join("\n")
}
