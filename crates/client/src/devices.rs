//! Several devices of one person (`docs/wire-format.md`, "Several devices"):
//! adding a device on both sides, listing and removing devices, and the
//! outbox of the sync mailbox.
//!
//! Adding runs as a sequence of steps on each device. The steps that need
//! the person go through a [`Dialog`]: the command line asks on its
//! terminal, the app shows buttons, tests answer directly.

use std::io;
use std::time::Duration;

use lz_core::handover::{Adding, NewDevice};
use lz_core::pairing::Address;
use lz_core::store::{Device, Group, RelayAccount, Store};
use lz_core::wire::{
    DeviceCode, Hash, MAX_DEVICE_NAME_LEN, MailboxId, RelayUrl, Token, device_code,
};
use rand_core::OsRng;
use serde::Serialize;

use crate::ctx::{Ctx, Fail, Result, core, inline, random_token, relay_url};
use crate::relay::{ReaderId, Relay, RelayError};
use crate::show::{Hint, Notice, when, with_hints};
use crate::sync::{Mode, own_mailboxes};

/// How long to wait between two looks at a handover slot or the sync
/// mailbox while adding a device.
pub const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Which of the two check codes of adding a device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// Shown by both devices once the existing device has checked the
    /// revealed value; confirmed on the existing device.
    Device,
    /// Shown by both devices once the new device has opened the handover;
    /// confirmed on the new device.
    Handover,
}

/// What adding a device needs from the person, or from a test.
///
/// The futures are not required to be `Send`, like those of
/// [`Relay`].
#[allow(async_fn_in_trait)]
pub trait Dialog {
    /// The new device has its device code. It is also written to the
    /// output, with a QR code for a terminal.
    fn device_code(&mut self, _code: &str) {}

    /// A check code that is confirmed on the other device, for display.
    fn check_code(&mut self, _check: Check, _code: &str) {}

    /// Asks whether `code` matches the six digits on the other device.
    async fn confirm(&mut self, check: Check, code: &str) -> bool;

    /// Waits before the relay is asked again.
    async fn pause(&mut self) {
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One device of the person, for display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeviceView {
    pub name: String,
    /// This device.
    pub this: bool,
}

/// Checks the name of a device: not empty, at most 32 bytes.
fn device_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(fail!("Der Gerätename darf nicht leer sein."));
    }
    if name.len() > MAX_DEVICE_NAME_LEN {
        return Err(fail!(
            "Der Gerätename ist zu lang (höchstens {MAX_DEVICE_NAME_LEN} Byte)."
        ));
    }
    Ok(name.to_owned())
}

/// A token this device may show to `relay` to store handover slots: the
/// access token of its relay account there, else a read token of an own
/// mailbox on it. Tokens never go to another relay than the one that
/// issued them.
fn slot_token(device: &Device, relay: &RelayUrl) -> Option<Token> {
    let access = device
        .relay
        .as_ref()
        .filter(|r| &r.url == relay)
        .and_then(|r| r.access_token);
    let group = device
        .group
        .as_ref()
        .filter(|g| &g.relay == relay)
        .map(|g| g.read_token);
    let contact = device
        .contacts
        .iter()
        .find(|c| !c.state.ended && &c.own_relay == relay)
        .map(|c| c.read_token);
    let invitation = device
        .invitations
        .iter()
        .find(|i| &i.pending.own.relay == relay)
        .map(|i| i.read_token);
    access.or(group).or(contact).or(invitation)
}

/// The mailboxes a new device is registered on: the sync mailbox and those
/// of the contacts, not those of open invitations.
fn mailboxes_for_new_device(device: &Device) -> Vec<(RelayUrl, MailboxId, Token)> {
    let mut list = Vec::new();
    if let Some(group) = &device.group {
        list.push((group.relay, group.mailbox, group.read_token));
    }
    for contact in device.contacts.iter().filter(|c| !c.state.ended) {
        list.push((
            contact.own_relay,
            contact.state.contact.own_mailbox,
            contact.read_token,
        ));
    }
    list
}

/// "Achtung: …" for situations where someone else interferes.
fn alarm(text: &str) -> Fail {
    fail!("Achtung: {text}")
}

impl<R: Relay> Ctx<R> {
    /// Sets up this device as a further device of a person: shows the
    /// device code, waits for the existing device and installs the state
    /// it hands over. `relay` is where the existing device stores the
    /// handover slots; it needs a relay account or a mailbox there.
    ///
    /// Refuses if this device has a state already. On any failure no state
    /// is left behind, and it starts over with a new code.
    pub async fn join_device(
        &mut self,
        relay: &str,
        name: &str,
        push_url: Option<&str>,
        dialog: &mut impl Dialog,
    ) -> Result<()> {
        if self.store.path().exists() {
            return Err(fail!(
                "Unter {} gibt es schon ein Gerät. Hinzufügen geht nur mit einem Gerät ohne \
                 Zustandsdatei.",
                self.store.path().display()
            ));
        }
        let name = device_name(name)?;
        if let Some(url) = push_url {
            crate::push::check(url)?;
        }
        let relay = relay_url(relay)?;
        let again = inline(self.hints, Hint::JoinAgain);

        let now = self.now();
        let mut new = NewDevice::create(relay, now, &mut OsRng);
        let code = new
            .code()
            .to_text(&mut [0; device_code::MAX_TEXT_LEN])
            .to_owned();
        self.say(&format!(
            "Gerätecode für ein vorhandenes Gerät, gültig bis {}:\n\n{code}\n",
            when(new.expires_at, now)
        ));
        self.say(&crate::qr::terminal(&code));
        let text = with_hints(
            "Gib ihn auf einem vorhandenen Gerät ein oder scanne ihn dort.".into(),
            self.hints,
            &[Hint::AddDevice],
        );
        self.say(&text);
        dialog.device_code(&code);

        // Step 3: the challenge, then reveal `n`.
        let challenge = self
            .poll_slot(
                &relay,
                &new.challenge_slot(),
                &new.fetch_token,
                new.expires_at,
                dialog,
            )
            .await
            .map_err(|error| join_failed(error, &again))?
            .ok_or_else(|| {
                fail!("Der Gerätecode ist abgelaufen, ohne dass ein Gerät ihn gelesen hat.{again}")
            })?;
        let reply = new
            .take_challenge(&challenge, self.now())
            .map_err(|error| match error {
                lz_core::Error::Expired => fail!("Der Gerätecode ist abgelaufen.{again}"),
                _ => alarm(&format!(
                    "Auf dem Relay lag für diesen Code etwas Unlesbares. Vermutlich kennt \
                     jemand anderes den Code.{again}"
                )),
            })?;
        self.relay
            .put_reveal(&relay, &new.reveal_slot(), &reply, &new.secret)
            .await
            .map_err(|error| join_failed(error, &again))?;
        let check = new.check_code().expect("the challenge was taken");
        self.say(&format!(
            "Prüfcode: {check}\nVergleiche ihn mit dem Prüfcode auf dem vorhandenen Gerät und \
             bestätige dort, wenn beide übereinstimmen."
        ));
        dialog.check_code(Check::Device, &check);

        // Step 6: the handover.
        let sealed = self
            .poll_slot(
                &relay,
                &new.handover_slot(),
                &new.fetch_token,
                new.expires_at,
                dialog,
            )
            .await
            .map_err(|error| join_failed(error, &again))?
            .ok_or_else(|| {
                fail!(
                    "Das vorhandene Gerät hat nichts übergeben, abgelehnt oder nicht rechtzeitig. \
                     Dieses Gerät wurde nicht eingerichtet.{again}"
                )
            })?;
        let handover = new.open_handover(&sealed).map_err(|error| match error {
            lz_core::Error::Mismatch => alarm(&format!(
                "Die Übergabe stammt nicht von dem Gerät, das deinen Code gelesen hat. Sie wurde \
                 verworfen; dieses Gerät wurde nicht eingerichtet.{again}"
            )),
            lz_core::Error::UnknownVersion(version) => fail!(
                "Die Übergabe hat die Version {version}, die dieses Gerät nicht kennt. Bring \
                 beide Geräte auf den neuesten Stand.{again}"
            ),
            _ => alarm(&format!(
                "Die Übergabe ist nicht lesbar und wurde verworfen. Vermutlich hat jemand anderes \
                 sie abgelegt.{again}"
            )),
        })?;
        let check = new.handover_check_code(&handover);
        self.say(&format!(
            "Übergabe erhalten. Prüfcode der Übergabe: {check}\nBestätige nur, wenn er mit dem \
             Prüfcode auf dem vorhandenen Gerät übereinstimmt."
        ));
        if !dialog.confirm(Check::Handover, &check).await {
            return Err(fail!(
                "Abgebrochen; dieses Gerät wurde nicht eingerichtet.{again}"
            ));
        }
        if self.now() > new.expires_at {
            return Err(fail!("Der Gerätecode ist abgelaufen.{again}"));
        }

        let contacts = handover.contacts.len();
        let mut device = new.install(handover, name.clone());
        device.push_url = push_url.map(str::to_owned);
        device.push_pending = push_url.is_some();
        device
            .announce_device(self.now(), &mut OsRng)
            .map_err(core)?;
        if let Some(parent) = self
            .store
            .path()
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        self.store
            .create(&device)
            .map_err(|error| match error.kind() {
                io::ErrorKind::AlreadyExists => {
                    fail!("{} gibt es schon.", self.store.path().display())
                }
                _ => error.into(),
            })?;

        // The new reader was registered without push address; a missing one
        // is sent again by the next fetch.
        let finished = async {
            self.retry_push_url().await?;
            if self.now() > new.expires_at {
                return Err(fail!("Der Gerätecode ist abgelaufen."));
            }
            match self.deliver_outbox().await? {
                None => Ok(()),
                Some(error) => Err(fail!(
                    "Die Anmeldung bei den eigenen Geräten ist nicht angekommen ({error})."
                )),
            }
        }
        .await;
        if let Err(error) = finished {
            self.delete_state()?;
            return Err(fail!(
                "{error} Dieses Gerät wurde nicht eingerichtet.{again}"
            ));
        }
        self.say(&format!(
            "Dieses Gerät ist eingerichtet als „{name}“, mit {contacts} Kontakt{}.",
            if contacts == 1 { "" } else { "en" }
        ));
        Ok(())
    }

    /// Adds the device that shows `code` to this one's person. `name` is
    /// the name of this device, shown on the other devices.
    pub async fn add_device(
        &mut self,
        code: &str,
        name: &str,
        dialog: &mut impl Dialog,
    ) -> Result<()> {
        let code =
            DeviceCode::from_text(code).map_err(|_| fail!("Das ist kein gültiger Gerätecode."))?;
        let name = device_name(name)?;
        let again = inline(self.hints, Hint::JoinSomeDevice);
        let device = self.load()?;
        if code.reader == device.reader_id || device.devices.iter().any(|d| d.reader == code.reader)
        {
            return Err(fail!("Dieses Gerät ist schon dabei."));
        }
        if device.devices.iter().any(|d| d.name == name) {
            return Err(fail!("Ein anderes eigenes Gerät heißt schon „{name}“."));
        }
        let mut adding = Adding::start(code, self.now(), &mut OsRng)
            .map_err(|_| fail!("Der Gerätecode ist abgelaufen.{again}"))?;
        let code = adding.code.clone();
        let relay = code.relay;
        if !device.knows_relay(&relay) {
            return Err(fail!(
                "Auf dem Relay {relay} aus dem Gerätecode hat dieses Gerät weder sein Konto noch \
                 ein Postfach. Gib beim neuen Gerät ein Relay an, das dieses Gerät kennt."
            ));
        }
        let auth = slot_token(&device, &relay).ok_or_else(|| {
            fail!(
                "Auf dem Relay {relay} hat dieses Gerät weder einen Zugangstoken noch ein \
                 eigenes Postfach."
            )
        })?;

        // Step 2: the challenge.
        let stored = self
            .relay
            .put_slot(
                &relay,
                &adding.challenge_slot(),
                &auth,
                &code.fetch_hash,
                Some(&adding.reply_hash()),
                &adding.challenge.to_bytes(),
            )
            .await;
        match stored {
            Ok(()) => {}
            Err(RelayError::HandoverExists) => {
                return Err(alarm(&format!(
                    "Für diesen Gerätecode liegt auf dem Relay schon etwas. Vermutlich kennt \
                     jemand anderes den Code. Nichts wurde übergeben.{again}"
                )));
            }
            Err(error) => return Err(busy_or(error)),
        }
        self.say("Warte auf das neue Gerät …");

        // Step 4: the revealed value.
        let revealed = self
            .poll_slot(
                &relay,
                &adding.reveal_slot(),
                &adding.challenge.reply_token,
                code.expires_at,
                dialog,
            )
            .await?
            .ok_or_else(|| {
                fail!("Das neue Gerät hat nicht rechtzeitig geantwortet. Nichts wurde übergeben.{again}")
            })?;
        let check = adding.check_reveal(&revealed).map_err(|_| {
            alarm(&format!(
                "Die Antwort passt nicht zum Gerätecode. Vermutlich kennt jemand anderes den \
                 Code. Nichts wurde übergeben.{again}"
            ))
        })?;
        self.say(&format!(
            "Prüfcode: {check}\nBestätige nur, wenn das neue Gerät denselben Prüfcode zeigt."
        ));
        if !dialog.confirm(Check::Device, &check).await {
            return Err(fail!("Abgebrochen; nichts wurde übergeben."));
        }
        if self.now() > code.expires_at {
            return Err(fail!("Der Gerätecode ist abgelaufen.{again}"));
        }

        // Step 5: group, readers, handover.
        let created = self.ensure_group(&name).await?;
        let handed_over = async {
            self.register_reader(&code.reader).await?;
            let (sealed, check) = adding
                .seal(&self.load()?, &mut OsRng)
                .map_err(|error| fail!("Übergabe nicht verschlüsselt: {error}"))?;
            match self
                .relay
                .put_slot(
                    &relay,
                    &adding.handover_slot(),
                    &auth,
                    &code.fetch_hash,
                    None,
                    &sealed,
                )
                .await
            {
                Ok(()) => Ok(check),
                Err(RelayError::HandoverExists) => Err(alarm(
                    "Jemand hat dem neuen Gerät schon eine Übergabe hingelegt; das kann nur, \
                     wer den Gerätecode kennt. Das neue Gerät wurde wieder ausgetragen; nichts \
                     wurde übergeben.",
                )),
                Err(error) => Err(busy_or(error)),
            }
        }
        .await;
        let check = match handed_over {
            Ok(check) => check,
            Err(error) => {
                self.undo_adding(&code.reader, created).await;
                return Err(fail!("{error}{again}"));
            }
        };
        self.say(&format!(
            "Übergeben. Prüfcode der Übergabe: {check}\nBestätige ihn auf dem neuen Gerät, wenn \
             er übereinstimmt."
        ));
        dialog.check_code(Check::Handover, &check);

        // Step 7: wait for `device` from the new reader.
        loop {
            let notices = self.fetch(Mode::Quiet).await?;
            if notices.contains(&Notice::Removed) {
                return Err(fail!("Dieses Gerät wurde entfernt."));
            }
            if self.load()?.devices.iter().any(|d| d.reader == code.reader) {
                self.say("Das neue Gerät ist eingerichtet.");
                return Ok(());
            }
            if self.now() > code.expires_at {
                break;
            }
            dialog.pause().await;
        }
        self.undo_adding(&code.reader, created).await;
        Err(fail!(
            "Das neue Gerät hat sich nicht rechtzeitig gemeldet und wurde wieder ausgetragen.{again}"
        ))
    }

    /// Looks at a handover slot until it has content or `until` has passed
    /// (`None`). An unreachable relay is tried again.
    async fn poll_slot(
        &mut self,
        relay: &RelayUrl,
        slot: &Hash,
        token: &Token,
        until: u64,
        dialog: &mut impl Dialog,
    ) -> std::result::Result<Option<Vec<u8>>, RelayError> {
        let mut warned = false;
        loop {
            match self.relay.get_slot(relay, slot, token).await {
                Ok(data) => return Ok(Some(data)),
                Err(RelayError::UnknownHandover) => {}
                Err(RelayError::Transport(detail)) => {
                    if !warned {
                        self.say(&format!(
                            "Relay nicht erreichbar ({detail}); neuer Versuch in Kürze …"
                        ));
                        warned = true;
                    }
                }
                Err(error) => return Err(error),
            }
            if self.now() > until {
                return Ok(None);
            }
            dialog.pause().await;
        }
    }

    /// Creates the group if there is none, with this device as reader of
    /// the sync mailbox, and sets the name of this device. Returns whether
    /// the group was created now.
    async fn ensure_group(&mut self, name: &str) -> Result<bool> {
        let device = self.load()?;
        if device.group.is_some() {
            if device.name.as_deref() != Some(name) {
                let now = self.now();
                self.store.update(|device| {
                    device.name = Some(name.to_owned());
                    device.announce_device(now, &mut OsRng).map_err(core)
                })?;
                self.flush_outbox().await?;
            }
            return Ok(false);
        }
        let read_token = random_token();
        let send_token = random_token();
        let (relay, mailbox) = self
            .create_sync_mailbox(&device, &read_token, &send_token)
            .await?;
        if let Err(error) = self
            .relay
            .put_reader(
                &relay,
                &mailbox,
                &read_token,
                &device.reader_id,
                device.push_url.as_deref(),
            )
            .await
        {
            self.delete_own(&relay, &mailbox, &read_token).await;
            return Err(fail!(
                "Postfach der eigenen Geräte nicht eingerichtet: {error}"
            ));
        }
        let group = Group::new(
            Address {
                relay,
                mailbox,
                send_token,
            },
            read_token,
            &mut OsRng,
        );
        let stored = self.store.update(|device| {
            if device.group.is_some() {
                return Err(fail!(
                    "Inzwischen gibt es schon eine Gruppe; bitte neu beginnen."
                ));
            }
            device.group = Some(group);
            device.name = Some(name.to_owned());
            Ok(())
        });
        if let Err(error) = stored {
            self.delete_own(&relay, &mailbox, &read_token).await;
            return Err(error);
        }
        Ok(true)
    }

    /// Creates the sync mailbox: with the access token of the relay
    /// account, or else derived from an own mailbox that was created with
    /// a voucher.
    async fn create_sync_mailbox(
        &mut self,
        device: &Device,
        read_token: &Token,
        send_token: &Token,
    ) -> Result<(RelayUrl, MailboxId)> {
        if let Some(RelayAccount {
            url,
            access_token: Some(access),
        }) = &device.relay
        {
            let mailbox = self
                .relay
                .create_mailbox(url, access, read_token, send_token)
                .await
                .map_err(|error| fail!("Postfach der eigenen Geräte nicht angelegt: {error}"))?;
            return Ok((*url, mailbox));
        }
        // Without an access token, the own mailboxes of contacts were
        // created with vouchers of their invitations; such a mailbox may
        // derive further ones.
        for contact in device.contacts.iter().filter(|c| !c.state.ended) {
            match self
                .relay
                .create_mailbox(
                    &contact.own_relay,
                    &contact.read_token,
                    read_token,
                    send_token,
                )
                .await
            {
                Ok(mailbox) => return Ok((contact.own_relay, mailbox)),
                Err(RelayError::Refused {
                    status: 401 | 429, ..
                }) => continue,
                Err(error) => {
                    return Err(fail!("Postfach der eigenen Geräte nicht angelegt: {error}"));
                }
            }
        }
        Err(fail!(
            "Mehrere Geräte brauchen einen Zugangstoken für ein eigenes Relay oder ein Postfach, \
             das mit dem Gutschein einer Einladung angelegt wurde und noch Platz hat. Lass dich \
             mit Gutschein einladen oder richte ein Relay mit Zugangstoken ein."
        ))
    }

    /// Registers the reader of a new device, without push address, on the
    /// sync mailbox and all mailboxes of contacts. On failure, the ones
    /// done are undone.
    async fn register_reader(&mut self, reader: &ReaderId) -> Result<()> {
        let device = self.load()?;
        let targets = mailboxes_for_new_device(&device);
        for (i, (relay, mailbox, token)) in targets.iter().enumerate() {
            if let Err(error) = self
                .relay
                .put_reader(relay, mailbox, token, reader, None)
                .await
            {
                for (relay, mailbox, token) in &targets[..i] {
                    let _ = self
                        .relay
                        .delete_reader(relay, mailbox, token, reader)
                        .await;
                }
                return Err(fail!(
                    "Das neue Gerät konnte nicht eingetragen werden: {error}."
                ));
            }
        }
        Ok(())
    }

    /// Unregisters a reader from all own mailboxes. Returns the first error
    /// other than an unknown reader or mailbox.
    async fn unregister_reader(&mut self, reader: &ReaderId) -> Result<Option<RelayError>> {
        let device = self.load()?;
        let mut first_error = None;
        for (relay, mailboxes) in own_mailboxes(&device) {
            for (mailbox, token) in mailboxes {
                match self
                    .relay
                    .delete_reader(&relay, &mailbox, &token, reader)
                    .await
                {
                    Ok(()) | Err(RelayError::UnknownMailbox) => {}
                    Err(RelayError::Refused { status: 404, .. }) => {}
                    Err(error) => {
                        first_error.get_or_insert(error);
                    }
                }
            }
        }
        Ok(first_error)
    }

    /// Takes back what adding a device did: the new reader, and the group
    /// if it was created for it. Best effort.
    async fn undo_adding(&mut self, reader: &ReaderId, group_created: bool) {
        if let Ok(Some(error)) = self.unregister_reader(reader).await {
            self.say(&format!(
                "Warnung: das neue Gerät ist nicht überall ausgetragen ({error})."
            ));
        }
        if !group_created {
            return;
        }
        let taken = self.store.update(|device| {
            device.outbox.clear();
            Ok::<_, Fail>(device.group.take())
        });
        if let Ok(Some(group)) = taken {
            self.delete_own(&group.relay, &group.mailbox, &group.read_token)
                .await;
        }
    }

    /// The devices of the person, this one first.
    pub fn devices(&self) -> Result<Vec<DeviceView>> {
        let device = self.load()?;
        let mut list = vec![DeviceView {
            name: device.name.clone().unwrap_or_default(),
            this: true,
        }];
        list.extend(device.devices.iter().map(|d| DeviceView {
            name: d.name.clone(),
            this: false,
        }));
        Ok(list)
    }

    /// Lists the devices of the person.
    pub fn list_devices(&mut self) -> Result<()> {
        let devices = self.devices()?;
        if devices.len() == 1 {
            let text = with_hints(
                "Bisher gibt es nur dieses Gerät.".into(),
                self.hints,
                &[Hint::JoinSomeDevice],
            );
            self.say(&text);
            return Ok(());
        }
        for device in devices {
            let line = match (device.this, device.name.is_empty()) {
                (true, false) => format!("„{}“ (dieses Gerät)", device.name),
                (true, true) => "dieses Gerät (ohne Namen)".to_owned(),
                (false, _) => format!("„{}“", device.name),
            };
            self.say(&line);
        }
        Ok(())
    }

    /// Removes another own device: unregisters it from all mailboxes and
    /// tells the remaining devices.
    pub async fn remove_device(&mut self, name: &str) -> Result<()> {
        let device = self.load()?;
        if device.name.as_deref() == Some(name) {
            return Err(fail!(
                "„{name}“ ist dieses Gerät. Entfernen geht nur von einem anderen Gerät aus."
            ));
        }
        let reader = device
            .devices
            .iter()
            .find(|d| d.name == name)
            .map(|d| d.reader)
            .ok_or_else(|| fail!("Kein eigenes Gerät namens „{name}“ gefunden."))?;
        if let Some(error) = self.unregister_reader(&reader).await? {
            return Err(fail!(
                "„{name}“ ist nicht überall ausgetragen ({error}); versuche es noch einmal."
            ));
        }
        self.tell_devices(|device, now| device.sync_device_removed(&reader, now, &mut OsRng))
            .await?;
        self.say(&format!(
            "„{name}“ entfernt. Es kennt aber weiterhin alle Schlüssel: Ist es verloren gegangen \
             oder gestohlen, kopple deine Kontakte neu."
        ));
        Ok(())
    }

    /// Puts a message for the other own devices into the outbox and sends
    /// it. Does nothing without other devices.
    pub(crate) async fn tell_devices(
        &mut self,
        queue: impl FnOnce(&mut Device, u64) -> std::result::Result<(), lz_core::Error>,
    ) -> Result<()> {
        if self.load()?.group.is_none() {
            return Ok(());
        }
        let now = self.now();
        self.store
            .update(|device| queue(device, now).map_err(core))?;
        self.flush_outbox().await
    }

    /// Sends the outbox to the sync mailbox. What the relay refuses stays
    /// for the next fetch; after 24 hours it is dropped.
    pub(crate) async fn flush_outbox(&mut self) -> Result<()> {
        if let Some(error) = self.deliver_outbox().await? {
            self.say(&format!(
                "Nachricht an die eigenen Geräte noch nicht zugestellt ({error}); neuer Versuch \
                 beim nächsten Abrufen."
            ));
        }
        Ok(())
    }

    /// Delivers the outbox in order, up to the first refusal, which it
    /// returns.
    async fn deliver_outbox(&mut self) -> Result<Option<RelayError>> {
        let device = self.store.load()?;
        if device.outbox.is_empty() {
            return Ok(None);
        }
        let Some(group) = device.group else {
            return Ok(None);
        };
        let now = self.now();
        let outbox = self.store.update(|device| {
            device.expire_sync(now);
            Ok::<_, Fail>(device.outbox.clone())
        })?;
        for outgoing in outbox {
            let delivered = self
                .relay
                .deliver(
                    &group.relay,
                    &group.mailbox,
                    &group.send_token,
                    &outgoing.envelope,
                )
                .await;
            if let Err(error) = delivered {
                return Ok(Some(error));
            }
            self.store.update(|device| {
                device.outbox.retain(|o| o.envelope != outgoing.envelope);
                Ok::<_, Fail>(())
            })?;
        }
        Ok(None)
    }

    /// Deletes the state file of this device and its lock file.
    pub(crate) fn delete_state(&self) -> Result<()> {
        let path = self.store.path();
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut lock = path.as_os_str().to_owned();
        lock.push(".lock");
        let _ = std::fs::remove_file(lock);
        Ok(())
    }
}

/// A failed relay call while joining.
fn join_failed(error: RelayError, again: &str) -> Fail {
    match error {
        RelayError::HandoverExists => alarm(&format!(
            "Die Antwort an das vorhandene Gerät war schon vergeben. Vermutlich kennt jemand \
             anderes den Code. Dieses Gerät wurde nicht eingerichtet.{again}"
        )),
        RelayError::Refused { status: 401, .. } => alarm(&format!(
            "Auf dem Relay liegt für diesen Code etwas, das nicht für dieses Gerät bestimmt ist. \
             Vermutlich kennt jemand anderes den Code.{again}"
        )),
        other => fail!("{other}. Dieses Gerät wurde nicht eingerichtet.{again}"),
    }
}

/// A refused handover slot, with the limits explained.
fn busy_or(error: RelayError) -> Fail {
    match error {
        RelayError::Refused { status: 429, code } if code == "handovers_quota" => fail!(
            "Auf dem Relay warten schon zwei Übergaben dieses Geräts; versuche es in 10 Minuten \
             noch einmal."
        ),
        RelayError::Refused { status: 503, .. } => fail!(
            "Auf dem Relay warten gerade zu viele Übergaben; versuche es in 10 Minuten noch \
             einmal."
        ),
        other => fail!("Übergabe nicht gespeichert: {other}."),
    }
}
