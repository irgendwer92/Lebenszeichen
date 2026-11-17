//! Several devices of one person (`docs/wire-format.md`, "Several devices"):
//! adding a device on both sides, listing and removing devices, and the
//! outbox of the sync mailbox.
//!
//! Adding runs as a sequence of steps on each device. The steps that need
//! the person go through a [`Dialog`]: the command line asks on its
//! terminal, the app shows buttons, tests answer directly.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use lz_core::handover::{Adding, NewDevice};
use lz_core::pairing::Address;
use lz_core::store::{Device, Group, RelayAccount};
use lz_core::wire::{
    DeviceCode, Hash, MAX_DEVICE_NAME_LEN, MailboxId, RelayUrl, Token, device_code,
};
use rand_core::OsRng;
use serde::Serialize;

use crate::ctx::{Ctx, Fail, Localize, Result, core, inline, random_token, relay_url};
use crate::i18n::Texts;
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
    /// Reader identifier in hex; names need not be unique.
    pub reader: String,
    /// This device.
    pub this: bool,
}

fn to_hex(reader: &ReaderId) -> String {
    reader.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str) -> Option<ReaderId> {
    let mut reader = ReaderId::default();
    if text.len() != 2 * reader.len() || !text.is_ascii() {
        return None;
    }
    for (i, byte) in reader.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(reader)
}

/// `name`, or `name 2`, `name 3` … if another device already has it. Cut
/// at a character boundary so that it stays within 32 bytes.
fn unique_name<'a>(name: &str, taken: impl Iterator<Item = &'a str> + Clone) -> String {
    let free = |candidate: &str| !taken.clone().any(|t| t == candidate);
    if free(name) {
        return name.to_owned();
    }
    (2..)
        .map(|n| {
            let suffix = format!(" {n}");
            let mut base = name.to_owned();
            while base.len() + suffix.len() > 32 {
                base.pop();
            }
            format!("{base}{suffix}")
        })
        .find(|candidate| free(candidate))
        .expect("there is always a free number")
}

/// Checks the name of a device: not empty, at most 32 bytes.
fn device_name(t: Texts, name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(fail!(t, "error-device-name-empty"));
    }
    if name.len() > MAX_DEVICE_NAME_LEN {
        return Err(fail!(
            t,
            "error-device-name-long",
            max = &MAX_DEVICE_NAME_LEN.to_string()
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
/// of the paired contacts, not those of open invitations or pairings in
/// progress, which the handover leaves out as well.
fn mailboxes_for_new_device(device: &Device) -> Vec<(RelayUrl, MailboxId, Token)> {
    let mut list = Vec::new();
    if let Some(group) = &device.group {
        list.push((group.relay, group.mailbox, group.read_token));
    }
    for contact in device
        .contacts
        .iter()
        .filter(|c| !c.state.ended && c.state.is_paired())
    {
        list.push((
            contact.own_relay,
            contact.state.contact.own_mailbox,
            contact.read_token,
        ));
    }
    list
}

/// "Achtung: …" for situations where someone else interferes.
fn alarm(t: Texts, text: &str) -> Fail {
    fail!(t, "error-alarm", text = text)
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
        let t = self.texts();
        if self.store.path().exists() {
            return Err(fail!(
                t,
                "error-join-has-device",
                path = &self.store.path().display().to_string()
            ));
        }
        let name = device_name(t, name)?;
        if let Some(url) = push_url {
            crate::push::check(t, url)?;
        }
        let relay = relay_url(t, relay)?;
        let again = inline(self.hints, t, Hint::JoinAgain);

        let now = self.now();
        let mut new = NewDevice::create(relay, now, &mut OsRng);
        let code = new
            .code()
            .to_text(&mut [0; device_code::MAX_TEXT_LEN])
            .to_owned();
        self.say(&format!(
            "{}\n",
            tr!(
                t,
                "out-device-code",
                until = &when(t, new.expires_at, now),
                code = &code
            )
        ));
        self.say(&crate::qr::terminal(&code));
        let text = with_hints(
            tr!(t, "out-device-code-enter"),
            self.hints,
            t,
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
            .map_err(|error| join_failed(t, error, &again))?
            .ok_or_else(|| fail!(t, "error-code-unread", again = &again))?;
        let reply = new
            .take_challenge(&challenge, self.now())
            .map_err(|error| match error {
                lz_core::Error::Expired => fail!(t, "error-code-expired", again = &again),
                _ => alarm(t, &tr!(t, "alarm-unreadable-challenge", again = &again)),
            })?;
        self.relay
            .put_reveal(&relay, &new.reveal_slot(), &reply, &new.secret)
            .await
            .map_err(|error| join_failed(t, error, &again))?;
        let check = new.check_code().expect("the challenge was taken");
        self.say(&tr!(t, "out-join-check", check = &check));
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
            .map_err(|error| join_failed(t, error, &again))?
            .ok_or_else(|| fail!(t, "error-nothing-handed-over", again = &again))?;
        let handover = new.open_handover(&sealed).map_err(|error| match error {
            lz_core::Error::Mismatch => {
                alarm(t, &tr!(t, "alarm-handover-mismatch", again = &again))
            }
            lz_core::Error::UnknownVersion(version) => fail!(
                t,
                "error-handover-version",
                version = &version.to_string(),
                again = &again
            ),
            _ => alarm(t, &tr!(t, "alarm-handover-unreadable", again = &again)),
        })?;
        let check = new.handover_check_code(&handover);
        self.say(&tr!(t, "out-join-handover-check", check = &check));
        if !dialog.confirm(Check::Handover, &check).await {
            return Err(fail!(t, "error-join-cancelled", again = &again));
        }
        if self.now() > new.expires_at {
            return Err(fail!(t, "error-code-expired", again = &again));
        }

        let contacts = handover.contacts.len();
        // Names need not be unique to work, but two devices with the same
        // name confuse people; the new one gets a number.
        let name = unique_name(&name, handover.devices.iter().map(|d| d.name.as_str()));
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
            std::fs::create_dir_all(parent).loc(t)?;
        }
        self.store
            .create(&device)
            .map_err(|error| match error.kind() {
                io::ErrorKind::AlreadyExists => fail!(
                    t,
                    "error-exists",
                    path = &self.store.path().display().to_string()
                ),
                _ => crate::ctx::io_fail(t, &error),
            })?;

        // The new reader was registered without push address; a missing one
        // is sent again by the next fetch.
        let finished = async {
            self.retry_push_url().await?;
            if self.now() > new.expires_at {
                return Err(fail!(t, "error-code-expired", again = ""));
            }
            match self.deliver_outbox().await? {
                None => Ok(()),
                Some(error) => Err(fail!(
                    t,
                    "error-announcement-not-arrived",
                    error = &error.text(t)
                )),
            }
        }
        .await;
        if let Err(error) = finished {
            self.delete_state()?;
            return Err(fail!(
                t,
                "error-join-failed",
                error = &error.0,
                again = &again
            ));
        }
        self.say(&tr!(t, "out-join-done", name = &name, contacts = contacts));
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
        let t = self.texts();
        let code =
            DeviceCode::from_text(code).map_err(|_| fail!(t, "error-invalid-device-code"))?;
        let name = device_name(t, name)?;
        let again = inline(self.hints, t, Hint::JoinSomeDevice);
        let device = self.load()?;
        if code.reader == device.reader_id || device.devices.iter().any(|d| d.reader == code.reader)
        {
            return Err(fail!(t, "error-device-already-added"));
        }
        if device.devices.iter().any(|d| d.name == name) {
            return Err(fail!(t, "error-device-name-taken", name = &name));
        }
        let mut adding = Adding::start(code, self.now(), &mut OsRng)
            .map_err(|_| fail!(t, "error-code-expired", again = &again))?;
        let code = adding.code.clone();
        let relay = code.relay;
        if !device.knows_relay(&relay) {
            return Err(fail!(
                t,
                "error-add-unknown-relay",
                relay = &relay.to_string()
            ));
        }
        let auth = slot_token(&device, &relay)
            .ok_or_else(|| fail!(t, "error-add-no-token", relay = &relay.to_string()))?;

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
                return Err(alarm(t, &tr!(t, "alarm-slot-taken", again = &again)));
            }
            Err(error) => return Err(busy_or(t, error)),
        }
        self.say(&tr!(t, "out-waiting-for-new-device"));

        // Step 4: the revealed value.
        let revealed = self
            .poll_slot(
                &relay,
                &adding.reveal_slot(),
                &adding.challenge.reply_token,
                code.expires_at,
                dialog,
            )
            .await
            .loc(t)?
            .ok_or_else(|| fail!(t, "error-new-device-silent", again = &again))?;
        let check = adding
            .check_reveal(&revealed)
            .map_err(|_| alarm(t, &tr!(t, "alarm-reveal-mismatch", again = &again)))?;
        self.say(&tr!(t, "out-add-check", check = &check));
        if !dialog.confirm(Check::Device, &check).await {
            return Err(fail!(t, "error-add-cancelled"));
        }
        if self.now() > code.expires_at {
            return Err(fail!(t, "error-code-expired", again = &again));
        }

        // Step 5: group, readers, handover.
        let created = self.ensure_group(&name).await?;
        let handed_over = async {
            self.register_reader(&code.reader).await?;
            let (sealed, check) = adding.seal(&self.load()?, &mut OsRng).map_err(|error| {
                fail!(
                    t,
                    "error-handover-not-encrypted",
                    error = &error.to_string()
                )
            })?;
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
                Err(RelayError::HandoverExists) => Err(alarm(t, &tr!(t, "alarm-handover-taken"))),
                Err(error) => Err(busy_or(t, error)),
            }
        }
        .await;
        let check = match handed_over {
            Ok(check) => check,
            Err(error) => {
                self.undo_adding(&code.reader, created).await;
                return Err(fail!(
                    t,
                    "error-with-again",
                    error = &error.0,
                    again = &again
                ));
            }
        };
        self.say(&tr!(t, "out-add-handed-over", check = &check));
        dialog.check_code(Check::Handover, &check);

        // Step 7: wait for `device` from the new reader.
        loop {
            let notices = self.fetch(Mode::Quiet).await?;
            if notices.contains(&Notice::Removed) {
                return Err(fail!(t, "error-device-removed"));
            }
            if self.load()?.devices.iter().any(|d| d.reader == code.reader) {
                self.say(&tr!(t, "out-new-device-done"));
                return Ok(());
            }
            if self.now() > code.expires_at {
                break;
            }
            dialog.pause().await;
        }
        self.undo_adding(&code.reader, created).await;
        Err(fail!(t, "error-new-device-late", again = &again))
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
                Err(error) if error.is_transport() => {
                    if !warned {
                        let detail = error.detail(self.texts());
                        let text = tr!(self.texts(), "out-relay-unreachable", detail = &detail);
                        self.say(&text);
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
        let t = self.texts();
        let device = self.load()?;
        if device.group.is_some() {
            if device.name.as_deref() != Some(name) {
                let now = self.now();
                self.update(|device| {
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
            return Err(fail!(t, "error-group-not-set-up", error = &error.text(t)));
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
        let stored = self.update(|device| {
            if device.group.is_some() {
                return Err(fail!(t, "error-group-exists"));
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
        let t = self.texts();
        if let Some(RelayAccount {
            url,
            access_token: Some(access),
        }) = &device.relay
        {
            let mailbox = self
                .relay
                .create_mailbox(url, access, read_token, send_token)
                .await
                .map_err(|error| fail!(t, "error-group-not-created", error = &error.text(t)))?;
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
                    return Err(fail!(t, "error-group-not-created", error = &error.text(t)));
                }
            }
        }
        Err(fail!(t, "error-group-needs-access"))
    }

    /// Registers the reader of a new device, without push address, on the
    /// sync mailbox and all mailboxes of contacts. On failure, the ones
    /// done are undone.
    async fn register_reader(&mut self, reader: &ReaderId) -> Result<()> {
        let t = self.texts();
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
                    t,
                    "error-reader-not-registered",
                    error = &error.text(t)
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
            let text = tr!(
                self.texts(),
                "warn-reader-not-removed",
                error = &error.text(self.texts())
            );
            self.say(&text);
        }
        if !group_created {
            return;
        }
        let taken = self.update(|device| {
            device.outbox.clear();
            Ok(device.group.take())
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
            reader: to_hex(&device.reader_id),
            this: true,
        }];
        list.extend(device.devices.iter().map(|d| DeviceView {
            name: d.name.clone(),
            reader: to_hex(&d.reader),
            this: false,
        }));
        Ok(list)
    }

    /// Lists the devices of the person.
    pub fn list_devices(&mut self) -> Result<()> {
        let t = self.texts();
        let devices = self.devices()?;
        if devices.len() == 1 {
            let text = with_hints(
                tr!(t, "out-only-this-device"),
                self.hints,
                t,
                &[Hint::JoinSomeDevice],
            );
            self.say(&text);
            return Ok(());
        }
        for device in devices {
            let line = match (device.this, device.name.is_empty()) {
                (true, false) => tr!(t, "out-device-this", name = &device.name),
                (true, true) => tr!(t, "out-device-this-unnamed"),
                (false, _) => tr!(t, "out-device-other", name = &device.name),
            };
            self.say(&line);
        }
        Ok(())
    }

    /// Removes another own device: unregisters it from all mailboxes and
    /// tells the remaining devices.
    pub async fn remove_device(&mut self, name: &str) -> Result<()> {
        let t = self.texts();
        let device = self.load()?;
        let matches: Vec<_> = device.devices.iter().filter(|d| d.name == name).collect();
        let reader = match matches.as_slice() {
            [one] => one.reader,
            [] if device.name.as_deref() == Some(name) => {
                return Err(fail!(t, "error-remove-this-device-named", name = name));
            }
            [] => return Err(fail!(t, "error-no-device-named", name = name)),
            _ => return Err(fail!(t, "error-several-devices-named", name = name)),
        };
        self.remove_reader(&reader, name).await
    }

    /// Removes the device with this reader identifier (hex), as the app
    /// does: names need not be unique.
    pub async fn remove_device_by_reader(&mut self, reader: &str) -> Result<()> {
        let t = self.texts();
        let device = self.load()?;
        let reader = from_hex(reader).ok_or_else(|| fail!(t, "error-unknown-device"))?;
        if reader == device.reader_id {
            return Err(fail!(t, "error-remove-this-device"));
        }
        let name = device
            .devices
            .iter()
            .find(|d| d.reader == reader)
            .map(|d| d.name.clone())
            .ok_or_else(|| fail!(t, "error-unknown-device"))?;
        self.remove_reader(&reader, &name).await
    }

    async fn remove_reader(&mut self, reader: &ReaderId, name: &str) -> Result<()> {
        let t = self.texts();
        let reader = *reader;
        if let Some(error) = self.unregister_reader(&reader).await? {
            return Err(fail!(
                t,
                "error-device-not-removed-everywhere",
                name = name,
                error = &error.text(t)
            ));
        }
        self.tell_devices(|device, now| device.sync_device_removed(&reader, now, &mut OsRng))
            .await?;
        self.say(&tr!(t, "out-device-removed", name = name));
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
        self.update(|device| queue(device, now).map_err(core))?;
        self.flush_outbox().await
    }

    /// Sends the outbox to the sync mailbox. What the relay refuses stays
    /// for the next fetch; after 24 hours it is dropped. A refusal is said
    /// once, not again at every fetch while the outbox stays blocked for the
    /// same reason.
    pub(crate) async fn flush_outbox(&mut self) -> Result<()> {
        let refused = self.deliver_outbox().await?.map(|error| {
            tr!(
                self.texts(),
                "warn-outbox-pending",
                error = &error.text(self.texts())
            )
        });
        if outbox_news(self.store.path(), refused.as_deref())
            && let Some(text) = refused
        {
            self.say(&text);
        }
        Ok(())
    }

    /// Delivers the outbox in order, up to the first refusal, which it
    /// returns.
    async fn deliver_outbox(&mut self) -> Result<Option<RelayError>> {
        let device = self.load()?;
        if device.outbox.is_empty() {
            return Ok(None);
        }
        let Some(group) = device.group else {
            return Ok(None);
        };
        let now = self.now();
        let outbox = self.update(|device| {
            device.expire_sync(now);
            Ok(device.outbox.clone())
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
            self.update(|device| {
                device.outbox.retain(|o| o.envelope != outgoing.envelope);
                Ok(())
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
            Err(error) => return Err(crate::ctx::io_fail(self.texts(), &error)),
        }
        let mut lock = path.as_os_str().to_owned();
        lock.push(".lock");
        let _ = std::fs::remove_file(lock);
        Ok(())
    }
}

/// A failed relay call while joining.
/// The refusal of the outbox last said, per state file. Kept for the life of
/// the process: the app fetches in one process, and `lz sync` fetches once
/// anyway.
static OUTBOX_SAID: Mutex<Vec<(PathBuf, String)>> = Mutex::new(Vec::new());

/// Notes the state of the outbox of a state file: refused with `text`, or
/// delivered (`None`). Whether a refusal is news and is to be said.
fn outbox_news(path: &Path, text: Option<&str>) -> bool {
    let mut said = OUTBOX_SAID
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let before = said.iter().position(|(p, _)| p == path);
    match (before, text) {
        (Some(i), Some(text)) if said[i].1 == text => false,
        (Some(i), Some(text)) => {
            said[i].1 = text.to_owned();
            true
        }
        (None, Some(text)) => {
            said.push((path.to_owned(), text.to_owned()));
            true
        }
        (Some(i), None) => {
            said.swap_remove(i);
            false
        }
        (None, None) => false,
    }
}

fn join_failed(t: Texts, error: RelayError, again: &str) -> Fail {
    match error {
        RelayError::HandoverExists => alarm(t, &tr!(t, "alarm-reply-taken", again = again)),
        RelayError::Refused { status: 401, .. } => {
            alarm(t, &tr!(t, "alarm-slot-not-for-us", again = again))
        }
        other => fail!(t, "error-join-relay", error = &other.text(t), again = again),
    }
}

/// A refused handover slot, with the limits explained.
fn busy_or(t: Texts, error: RelayError) -> Fail {
    match error {
        RelayError::Refused { status: 429, code } if code == "handovers_quota" => {
            fail!(t, "error-handovers-quota")
        }
        RelayError::Refused { status: 503, .. } => fail!(t, "error-handovers-busy"),
        other => fail!(t, "error-handover-not-stored", error = &other.text(t)),
    }
}

#[cfg(test)]
mod tests {
    use super::{from_hex, to_hex, unique_name};

    #[test]
    fn a_blocked_outbox_is_said_once() {
        use super::outbox_news;
        let (a, b) = (std::path::Path::new("/a"), std::path::Path::new("/b"));
        assert!(outbox_news(a, Some("full")));
        assert!(!outbox_news(a, Some("full")));
        // Another device, or another reason, is news.
        assert!(outbox_news(b, Some("full")));
        assert!(outbox_news(a, Some("rate limited")));
        // Once delivered, the next refusal is news again.
        assert!(!outbox_news(a, None));
        assert!(outbox_news(a, Some("rate limited")));
    }

    #[test]
    fn duplicate_names_get_a_number() {
        let taken = ["Laptop", "Handy", "Laptop 2"];
        assert_eq!(unique_name("Tablet", taken.iter().copied()), "Tablet");
        assert_eq!(unique_name("Laptop", taken.iter().copied()), "Laptop 3");
        assert_eq!(unique_name("Handy", taken.iter().copied()), "Handy 2");
        let long = "x".repeat(32);
        let named = unique_name(&long, [long.as_str()].into_iter());
        assert_eq!(named.len(), 32);
        assert!(named.ends_with(" 2"));
    }

    #[test]
    fn reader_ids_round_trip_as_hex() {
        let reader = [0xab; 16];
        assert_eq!(from_hex(&to_hex(&reader)), Some(reader));
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex(&"zz".repeat(16)), None);
    }
}
