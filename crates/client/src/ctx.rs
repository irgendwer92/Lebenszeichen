//! The commands of `lz`, as functions on [`Ctx`] so that tests can call them
//! with a fake relay and a manual clock.
//!
//! Every change of the device state goes through [`JsonFile::update`].
//! Messages to a contact are prepared and stored first and delivered
//! afterwards; if the first delivery fails, the change is rolled back, so the
//! command can simply be repeated.

use std::fmt;
use std::io::{self, Write};
use std::path::Path;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lz_core::contact::ContactState;
use lz_core::pairing::{Address, PairingState, PendingInvitation, accept};
use lz_core::store::{Device, JsonFile, RelayAccount, Store, StoredContact, StoredInvitation};
use lz_core::wire::invitation::MAX_TEXT_LEN;
use lz_core::wire::{
    ENVELOPE_LEN, Error as WireError, Invitation, MailboxId, Reason, RelayUrl, ResponseKind, Rules,
    Token,
};
use rand_core::{OsRng, RngCore};
use serde::Serialize;

use crate::relay::{Relay, RelayError, from_b64};
use crate::show::{self, Hint, Hints, when, with_hints};

/// A failed command, with a message for the person at the keyboard.
#[derive(Debug, PartialEq, Eq)]
pub struct Fail(pub String);

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<io::Error> for Fail {
    fn from(error: io::Error) -> Self {
        Self(format!("Zustandsdatei: {error}"))
    }
}

impl From<RelayError> for Fail {
    fn from(error: RelayError) -> Self {
        Self(error.to_string())
    }
}

macro_rules! fail {
    ($($arg:tt)*) => { $crate::ctx::Fail(format!($($arg)*)) };
}

pub type Result<T> = std::result::Result<T, Fail>;

/// Source of the current time.
#[derive(Clone)]
pub enum Clock {
    System,
    /// Set by tests.
    #[cfg(test)]
    Manual(Arc<AtomicU64>),
}

impl Clock {
    pub fn now(&self) -> u64 {
        match self {
            Self::System => system_now(),
            #[cfg(test)]
            Self::Manual(at) => at.load(Ordering::SeqCst),
        }
    }
}

pub fn system_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Everything a command needs.
pub struct Ctx<R> {
    pub store: JsonFile,
    pub relay: R,
    pub clock: Clock,
    pub out: Box<dyn Write>,
    /// Also show dropped messages.
    pub verbose: bool,
    /// How long `sync` waits for further messages before it ends.
    pub idle: Duration,
    /// Wording of hints such as "confirm with …"; see [`Hint`].
    pub hints: Hints,
}

/// Creates the state file with a new device.
pub fn init(
    store: &JsonFile,
    relay: Option<&str>,
    access_token: Option<&str>,
    out: &mut dyn Write,
) -> Result<()> {
    let mut device = Device::new(&mut OsRng);
    if let Some(url) = relay {
        let access_token = access_token
            .map(|text| {
                from_b64::<32>(text)
                    .ok_or_else(|| fail!("Der Zugangstoken muss b64url von 32 Byte sein."))
            })
            .transpose()?;
        device.relay = Some(RelayAccount {
            url: relay_url(url)?,
            access_token,
        });
    } else if access_token.is_some() {
        return Err(fail!("Ein Zugangstoken braucht auch --relay."));
    }
    if let Some(parent) = store.path().parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    store.create(&device).map_err(|error| match error.kind() {
        io::ErrorKind::AlreadyExists => fail!("{} gibt es schon.", store.path().display()),
        _ => error.into(),
    })?;
    say(
        out,
        &format!("Gerät angelegt in {}.", store.path().display()),
    );
    match &device.relay {
        Some(RelayAccount {
            access_token: Some(_),
            ..
        }) => {}
        Some(_) => say(
            out,
            "Ohne Zugangstoken kannst du nur Einladungen mit Gutschein annehmen.",
        ),
        None => say(
            out,
            "Ohne Relay kannst du nur Einladungen mit Gutschein annehmen.",
        ),
    }
    Ok(())
}

/// Checks and normalises the base address of a relay.
pub fn relay_url(text: &str) -> Result<RelayUrl> {
    let text = text.trim().trim_end_matches('/');
    let rest = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
        .ok_or_else(|| fail!("Die Relay-Adresse muss mit https:// oder http:// beginnen."))?;
    if rest.is_empty() {
        return Err(fail!("Die Relay-Adresse hat keinen Rechnernamen."));
    }
    RelayUrl::new(text).map_err(|_| fail!("Die Relay-Adresse ist zu lang."))
}

/// A hint to append to a sentence, with a leading space, or nothing.
pub(crate) fn inline(hints: Hints, hint: Hint<'_>) -> String {
    hints(&hint).map_or_else(String::new, |text| format!(" {text}"))
}

pub(crate) fn say(out: &mut dyn Write, text: &str) {
    // Output that cannot be written is not worth failing a command for.
    let _ = writeln!(out, "{text}");
}

pub(crate) fn random_token() -> Token {
    let mut token = [0; 32];
    OsRng.fill_bytes(&mut token);
    token
}

/// Compares two states by their stored form.
fn same(a: &ContactState, b: &ContactState) -> bool {
    serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
}

/// Fails if a contact was already made from this invitation, so that the
/// same code is not accepted twice by mistake.
fn already_accepted(device: &Device, invitation: &Invitation) -> Result<()> {
    let earlier = device.contacts.iter().find(|c| {
        let peer = &c.state.contact.peer;
        peer.relay == invitation.relay && peer.mailbox == invitation.mailbox
    });
    match earlier {
        Some(contact) => Err(fail!(
            "Diese Einladung hast du schon angenommen (als {}).",
            contact.name
        )),
        None => Ok(()),
    }
}

fn name_is_free(device: &Device, name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(fail!("Der Name darf nicht leer sein."));
    }
    let taken = device.contacts.iter().any(|c| c.name == name)
        || device.invitations.iter().any(|i| i.name == name);
    if taken {
        return Err(fail!("Den Namen {name} gibt es schon."));
    }
    Ok(())
}

pub(crate) fn contact_mut<'a>(device: &'a mut Device, name: &str) -> Result<&'a mut StoredContact> {
    device
        .contacts
        .iter_mut()
        .find(|c| c.name == name)
        .ok_or_else(|| fail!("Keinen Kontakt namens {name} gefunden."))
}

/// Fails unless requests and responses are possible with this contact.
fn check_active(name: &str, state: &ContactState) -> Result<()> {
    if state.ended {
        Err(fail!("Die Verbindung mit {name} ist beendet."))
    } else if !state.is_paired() {
        Err(fail!("{name} ist noch nicht gekoppelt."))
    } else {
        Ok(())
    }
}

pub(crate) fn core(error: lz_core::Error) -> Fail {
    fail!("{error}")
}

impl<R: Relay> Ctx<R> {
    pub fn now(&self) -> u64 {
        self.clock.now()
    }

    pub(crate) fn say(&mut self, text: &str) {
        say(&mut self.out, text);
    }

    pub(crate) fn load(&self) -> Result<Device> {
        self.store.load().map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => fail!(
                "Kein Gerät unter {}.{}",
                self.store.path().display(),
                inline(self.hints, Hint::Init)
            ),
            _ => error.into(),
        })
    }

    /// Creates an invitation and returns its code.
    pub async fn invite(&mut self, name: &str, voucher: bool) -> Result<String> {
        let device = self.load()?;
        name_is_free(&device, name)?;
        let Some(RelayAccount {
            url,
            access_token: Some(access),
        }) = device.relay
        else {
            return Err(fail!(
                "Einladen braucht ein eigenes Relay mit Zugangstoken.{}",
                inline(self.hints, Hint::InitWithAccessToken)
            ));
        };
        let read_token = random_token();
        let send_token = random_token();
        let mailbox = self
            .relay
            .create_mailbox(&url, &access, &read_token, &send_token)
            .await?;

        let prepared = async {
            self.relay
                .put_reader(
                    &url,
                    &mailbox,
                    &read_token,
                    &device.reader_id,
                    device.push_url.as_deref(),
                )
                .await?;
            let voucher = match voucher {
                true => Some(self.relay.create_voucher(&url, &access).await?),
                false => None,
            };
            let own = Address {
                relay: url,
                mailbox,
                send_token,
            };
            let (pending, invitation) =
                PendingInvitation::create(own, voucher, self.now(), &mut OsRng);
            self.store.update(|device| {
                name_is_free(device, name)?;
                device.invitations.push(StoredInvitation {
                    name: name.into(),
                    read_token,
                    pending,
                });
                Ok::<_, Fail>(())
            })?;
            Ok::<_, Fail>(invitation)
        }
        .await;
        let invitation = match prepared {
            Ok(invitation) => invitation,
            Err(error) => {
                let _ = self.relay.delete_mailbox(&url, &mailbox, &read_token).await;
                return Err(error);
            }
        };

        let mut buf = [0; MAX_TEXT_LEN];
        let code = invitation.to_text(&mut buf).to_owned();
        let now = self.now();
        self.say(&format!("Einladung für {name}:\n\n{code}\n"));
        self.say(&format!(
            "Gültig bis {} und nur einmal{}.{}",
            when(invitation.expires_at, now),
            if invitation.voucher.is_some() {
                ", mit Gutschein für ein Postfach auf deinem Relay"
            } else {
                ""
            },
            inline(self.hints, Hint::Fetch)
        ));
        Ok(code)
    }

    /// Accepts an invitation and sends `hello`. The verification code
    /// follows once the inviting side has answered with `reveal`, as a
    /// notice while fetching.
    pub async fn accept(&mut self, name: &str, code: &str) -> Result<()> {
        let invitation = Invitation::from_text(code).map_err(|error| match error {
            WireError::OldInvitation | WireError::UnsupportedVersion(..=1) => {
                fail!("{}", show::OLD_INVITATION)
            }
            WireError::UnsupportedVersion(_) => fail!(
                "Diese Einladung stammt aus einer neueren Version; bitte aktualisiere Lebenszeichen."
            ),
            _ => fail!("Das ist kein gültiger Einladungscode."),
        })?;
        if self.now() > invitation.expires_at {
            return Err(fail!("Die Einladung ist abgelaufen."));
        }
        let device = self.load()?;
        name_is_free(&device, name)?;
        already_accepted(&device, &invitation)?;
        let (own_relay, authorisation) = match (&device.relay, invitation.voucher) {
            (
                Some(RelayAccount {
                    url,
                    access_token: Some(access),
                }),
                _,
            ) => (*url, *access),
            (_, Some(voucher)) => (invitation.relay, voucher),
            _ => {
                return Err(fail!(
                    "Kein Zugangstoken für ein eigenes Relay und kein Gutschein in der Einladung. \
                     Die Gegenseite kann mit Gutschein einladen.{}",
                    inline(self.hints, Hint::InviteWithVoucher)
                ));
            }
        };

        let read_token = random_token();
        let send_token = random_token();
        let mailbox = self
            .relay
            .create_mailbox(&own_relay, &authorisation, &read_token, &send_token)
            .await?;
        let own = Address {
            relay: own_relay,
            mailbox,
            send_token,
        };
        let mut stored = false;
        let result = async {
            self.relay
                .put_reader(
                    &own_relay,
                    &mailbox,
                    &read_token,
                    &device.reader_id,
                    device.push_url.as_deref(),
                )
                .await?;
            // A new contact key for every acceptance, also when retrying.
            let (contact, hello) =
                accept(&invitation, own, self.now(), &mut OsRng).map_err(core)?;
            // Key, commitment and own mailbox are stored before `hello`
            // goes out, so that the `reveal` answering it can be checked.
            self.store.update(|device| {
                name_is_free(device, name)?;
                already_accepted(device, &invitation)?;
                device.contacts.push(StoredContact {
                    name: name.into(),
                    own_relay,
                    read_token,
                    state: ContactState::new(contact),
                });
                Ok::<_, Fail>(())
            })?;
            stored = true;
            self.relay
                .deliver(
                    &invitation.relay,
                    &invitation.mailbox,
                    &invitation.send_token,
                    &hello,
                )
                .await
                .map_err(|error| match error {
                    RelayError::UnknownMailbox => fail!("Die Einladung gilt nicht mehr."),
                    other => other.into(),
                })
        }
        .await;
        if let Err(error) = result {
            if stored {
                self.store.update(|device| {
                    device
                        .contacts
                        .retain(|c| !(c.name == name && c.state.contact.own_mailbox == mailbox));
                    Ok::<_, Fail>(())
                })?;
            }
            let _ = self
                .relay
                .delete_mailbox(&own_relay, &mailbox, &read_token)
                .await;
            return Err(error);
        }
        self.say("Einladung angenommen. Warte auf den Prüfcode …");
        let text = with_hints(
            format!(
                "Der Prüfcode kommt, sobald das Gerät von {name} die Annahme abgeholt hat. \
                 Gekoppelt seid ihr, sobald {name} bestätigt hat.{}",
                inline(self.hints, Hint::Fetch)
            ),
            self.hints,
            &[Hint::Reject(name)],
        );
        self.say(&text);
        Ok(())
    }

    /// Confirms a contact that accepted an own invitation.
    pub async fn confirm(&mut self, name: &str) -> Result<()> {
        let hints = self.hints;
        self.change_and_send(name, |state, now| {
            if state.ended {
                return Err(fail!("Die Verbindung mit {name} ist beendet."));
            }
            match state.contact.state {
                PairingState::AwaitingConfirmation => {}
                PairingState::AwaitingReveal | PairingState::AwaitingHelloOk => {
                    return Err(fail!(
                        "{name} muss bestätigen, nicht du.{}",
                        inline(hints, Hint::Fetch)
                    ));
                }
                PairingState::Paired => return Err(fail!("{name} ist schon gekoppelt.")),
            }
            let envelopes = state.confirm(now, &mut OsRng).map_err(core)?;
            Ok(((), envelopes.to_vec()))
        })
        .await?;
        self.say(&format!("{name} ist jetzt gekoppelt."));
        // `confirm` has announced the rules, so the record carries their
        // stamp.
        let mailbox = self.own_mailbox_of(name)?;
        self.tell_devices(|device, now| device.sync_contact_added(&mailbox, now, &mut OsRng))
            .await
    }

    /// Asks a contact for a sign of life.
    pub async fn request(&mut self, name: &str) -> Result<()> {
        let (id, at) = self
            .change_and_send(name, |state, now| {
                check_active(name, state)?;
                match state.request(now, &mut OsRng) {
                    Ok((id, envelope)) => Ok(((id, now), vec![envelope])),
                    Err(lz_core::Error::NotYet(at)) => Err(fail!(
                        "Anfrage an {name} erst wieder möglich ab {}.",
                        when(at, now)
                    )),
                    Err(error) => Err(core(error)),
                }
            })
            .await?;
        self.say(&format!("Anfrage an {name} gesendet."));
        let mailbox = self.own_mailbox_of(name)?;
        self.tell_devices(|device, now| device.sync_requested(&mailbox, id, at, now, &mut OsRng))
            .await
    }

    /// Answers the open request of a contact.
    pub async fn respond(&mut self, name: &str) -> Result<()> {
        let (id, at) = self
            .change_and_send(name, |state, now| {
                check_active(name, state)?;
                let id = state
                    .open_request(now)
                    .ok_or_else(|| fail!("Keine offene Anfrage von {name}."))?;
                let envelope = state.respond(&id, now, &mut OsRng).map_err(core)?;
                Ok(((id, now), vec![envelope]))
            })
            .await?;
        self.say(&format!("Lebenszeichen an {name} gesendet ✓"));
        let mailbox = self.own_mailbox_of(name)?;
        self.tell_devices(|device, now| {
            device.sync_responded(&mailbox, id, ResponseKind::Manual, at, now, &mut OsRng)
        })
        .await
    }

    /// Sends a reported state for the open request of a contact.
    pub async fn report(&mut self, name: &str, reason: &str) -> Result<()> {
        let reason = Reason::new(reason.trim())
            .map_err(|_| fail!("Der Grund ist zu lang (höchstens 64 Byte)."))?;
        if reason.is_empty() {
            return Err(fail!("Der Grund darf nicht leer sein."));
        }
        let (id, at) = self
            .change_and_send(name, |state, now| {
                check_active(name, state)?;
                let id = state
                    .open_request(now)
                    .ok_or_else(|| fail!("Keine offene Anfrage von {name}."))?;
                match state.report(&id, reason, now, &mut OsRng) {
                    Ok(envelope) => Ok(((id, now), vec![envelope])),
                    Err(lz_core::Error::Unexpected) => {
                        Err(fail!("Zu dieser Anfrage ist schon ein Zustand gemeldet."))
                    }
                    Err(error) => Err(core(error)),
                }
            })
            .await?;
        let text = with_hints(
            format!(
                "An {name} gemeldet: {}. Die Anfrage bleibt offen, bis du antwortest.",
                show::reported(reason)
            ),
            self.hints,
            &[Hint::Respond(name)],
        );
        self.say(&text);
        let mailbox = self.own_mailbox_of(name)?;
        self.tell_devices(|device, now| {
            let kind = ResponseKind::Reported(reason);
            device.sync_responded(&mailbox, id, kind, at, now, &mut OsRng)
        })
        .await
    }

    /// Shows or changes how often a contact may ask.
    pub async fn rules(
        &mut self,
        name: &str,
        interval_minutes: Option<u32>,
        expiry_minutes: Option<u32>,
        weekly_limit: Option<u16>,
    ) -> Result<()> {
        if interval_minutes.is_none() && expiry_minutes.is_none() && weekly_limit.is_none() {
            let mut device = self.load()?;
            let contact = contact_mut(&mut device, name)?;
            let (own, peer) = (contact.state.own_rules, contact.state.peer_rules);
            self.say(&format!("{name} darf dich fragen: {}", show::rules(&own)));
            self.say(&format!("Du darfst {name} fragen: {}", show::rules(&peer)));
            return Ok(());
        }
        if expiry_minutes == Some(0) {
            return Err(fail!("Die Ablaufzeit muss mindestens 1 Minute sein."));
        }
        let rules = self
            .change_and_send(name, |state, now| {
                check_active(name, state)?;
                let old = state.own_rules;
                let rules = Rules {
                    min_interval_minutes: interval_minutes.unwrap_or(old.min_interval_minutes),
                    expiry_minutes: expiry_minutes.unwrap_or(old.expiry_minutes),
                    weekly_limit: weekly_limit.unwrap_or(old.weekly_limit),
                };
                let envelope = state.set_rules(rules, now, &mut OsRng).map_err(core)?;
                Ok((rules, vec![envelope]))
            })
            .await?;
        self.say(&format!("Neue Regeln für {name}: {}", show::rules(&rules)));
        // Same stamp as the `rules` just sent to the contact.
        let mailbox = self.own_mailbox_of(name)?;
        self.tell_devices(|device, now| device.sync_rules_set(&mailbox, now, &mut OsRng))
            .await
    }

    /// Ends a contact or withdraws an invitation.
    pub async fn remove(&mut self, name: &str) -> Result<()> {
        let device = self.load()?;
        if let Some(invitation) = device.invitations.iter().find(|i| i.name == name) {
            // Forget the invitation first: a running `listen` then knows that
            // the mailbox is gone on purpose when the relay reports it.
            self.store.update(|device| {
                device.invitations.retain(|i| i.name != name);
                Ok::<_, Fail>(())
            })?;
            let own = &invitation.pending.own;
            if let Err(error) = self
                .relay
                .delete_mailbox(&own.relay, &own.mailbox, &invitation.read_token)
                .await
            {
                self.warn_unless_gone(&error);
            }
            self.say(&format!("Einladung für {name} zurückgezogen."));
            return Ok(());
        }
        self.end_contact(name).await?;
        self.say(&format!("{name} entfernt."));
        Ok(())
    }

    /// Rejects a contact whose pairing is not complete, typically because the
    /// verification codes did not match. The other side sees the rejection.
    pub async fn reject(&mut self, name: &str) -> Result<()> {
        let device = self.load()?;
        let contact = device
            .contacts
            .iter()
            .find(|c| c.name == name)
            .ok_or_else(|| fail!("Keinen Kontakt namens {name} gefunden."))?;
        let next = match contact.state.contact.state {
            PairingState::Paired => {
                return Err(fail!(
                    "{name} ist schon gekoppelt; ablehnen geht nur vorher.{}",
                    inline(self.hints, Hint::Remove(name))
                ));
            }
            PairingState::AwaitingConfirmation => with_hints(
                "Für einen neuen Versuch lade neu ein.".to_owned(),
                self.hints,
                &[Hint::Invite(name)],
            ),
            PairingState::AwaitingReveal | PairingState::AwaitingHelloOk => {
                format!("Bitte {name} um eine neue Einladung.")
            }
        };
        // Delivered means the relay took it; whether the device on the
        // other side can read it, this side cannot know.
        let delivered = if self.end_contact(name).await? {
            " Die Ablehnung wurde zugestellt."
        } else {
            ""
        };
        self.say(&format!("{name} abgelehnt.{delivered} {next}"));
        Ok(())
    }

    /// Sends `end` unless the contact has already ended, deletes the own
    /// mailbox and drops the contact. Returns whether `end` was delivered.
    async fn end_contact(&mut self, name: &str) -> Result<bool> {
        let device = self.load()?;
        let contact = device
            .contacts
            .iter()
            .find(|c| c.name == name)
            .ok_or_else(|| fail!("Keinen Kontakt namens {name} gefunden."))?;
        let (own_relay, own_mailbox, read_token) = (
            contact.own_relay,
            contact.state.contact.own_mailbox,
            contact.read_token,
        );
        // Only paired contacts are known to the other own devices.
        let known_elsewhere = contact.state.is_paired();
        let mut delivered = false;
        if !contact.state.ended {
            let now = self.now();
            let (envelope, peer) = self.store.update(|device| {
                let contact = contact_mut(device, name)?;
                let envelope = contact.state.end(now, &mut OsRng).map_err(core)?;
                Ok::<_, Fail>((envelope, contact.state.contact.peer.clone()))
            })?;
            match self
                .relay
                .deliver(&peer.relay, &peer.mailbox, &peer.send_token, &envelope)
                .await
            {
                Ok(()) => delivered = true,
                Err(RelayError::UnknownMailbox) => {}
                Err(error) => self.say(&format!(
                    "Warnung: {name} hat die Nachricht über das Ende nicht bekommen ({error}). \
                     Das Entfernen bemerkt {name} trotzdem bei der nächsten Anfrage."
                )),
            }
        }
        if let Err(error) = self
            .relay
            .delete_mailbox(&own_relay, &own_mailbox, &read_token)
            .await
        {
            self.warn_unless_gone(&error);
        }
        self.store.update(|device| {
            device.contacts.retain(|c| c.name != name);
            Ok::<_, Fail>(())
        })?;
        if known_elsewhere {
            self.tell_devices(|device, now| {
                device.sync_contact_removed(&own_mailbox, now, &mut OsRng)
            })
            .await?;
        }
        Ok(delivered)
    }

    /// Identifier of the own mailbox for a contact, the contact identifier
    /// between own devices.
    fn own_mailbox_of(&self, name: &str) -> Result<MailboxId> {
        let device = self.load()?;
        let contact = device
            .contacts
            .iter()
            .find(|c| c.name == name)
            .ok_or_else(|| fail!("Keinen Kontakt namens {name} gefunden."))?;
        Ok(contact.state.contact.own_mailbox)
    }

    pub(crate) fn warn_unless_gone(&mut self, error: &RelayError) {
        if *error != RelayError::UnknownMailbox {
            self.say(&format!(
                "Warnung: eigenes Postfach nicht gelöscht ({error}). Das Relay räumt es nach 90 Tagen auf."
            ));
        }
    }

    /// The state of all contacts and open invitations, for display.
    pub fn overview(&self) -> Result<Overview> {
        let device = self.load()?;
        let now = self.now();
        let contacts = device
            .contacts
            .iter()
            .map(|contact| contact_view(contact, now))
            .collect();
        let invitations = device
            .invitations
            .iter()
            .map(|invitation| InvitationView {
                name: invitation.name.clone(),
                expires_at: invitation.pending.expires_at,
                expired: invitation.pending.expires_at < now,
            })
            .collect();
        Ok(Overview {
            now,
            contacts,
            invitations,
        })
    }

    /// Lists contacts and open invitations.
    pub fn contacts(&mut self) -> Result<()> {
        let overview = self.overview()?;
        let now = overview.now;
        let mut lines = Vec::new();
        for contact in &overview.contacts {
            let name = &contact.name;
            let status = match &contact.status {
                Status::Ended => format!("beendet.{}", inline(self.hints, Hint::Remove(name))),
                Status::AwaitingReveal => format!(
                    "Einladung angenommen, warte auf den Prüfcode …{}{}",
                    inline(self.hints, Hint::Fetch),
                    inline(self.hints, Hint::Reject(name))
                ),
                Status::AwaitingHelloOk { code } => format!(
                    "wartet auf die Bestätigung durch {name}, Prüfcode {code}. {}{}",
                    show::COMPARE_HINT,
                    inline(self.hints, Hint::Reject(name))
                ),
                Status::AwaitingConfirmation { code } => format!(
                    "hat angenommen, Prüfcode {code}. {}{}{}",
                    show::COMPARE_HINT,
                    inline(self.hints, Hint::Confirm(name)),
                    inline(self.hints, Hint::Reject(name))
                ),
                Status::Paired => "gekoppelt".to_owned(),
            };
            lines.push(format!("{name}: {status}"));
            if contact.status != Status::Paired {
                continue;
            }
            if let Some(open) = &contact.incoming {
                let mut line = format!(
                    "  offene Anfrage seit {}, läuft ab {}",
                    when(open.since, now),
                    when(open.expires_at, now)
                );
                if let Some(reason) = &open.reported {
                    line.push_str(&format!("; gemeldet: {}", show::reported(reason)));
                }
                lines.push(line);
            }
            if let Some(since) = contact.outgoing_since {
                lines.push(format!("  eigene Anfrage offen seit {}", when(since, now)));
            }
            if contact.next_request_at <= now {
                lines.push("  Anfrage möglich".to_owned());
            } else {
                lines.push(format!(
                    "  Anfrage wieder möglich ab {}",
                    when(contact.next_request_at, now)
                ));
            }
            if let Some(last) = &contact.last_response {
                let text = match last {
                    LastResponse::Manual { at } => format!("✓ {}", when(*at, now)),
                    LastResponse::Reported { reason } => show::reported(reason),
                };
                lines.push(format!("  letzte Antwort: {text}"));
            }
        }
        for invitation in &overview.invitations {
            let status = if invitation.expired {
                "Einladung abgelaufen; eine rechtzeitige Annahme wird noch abgeholt".to_owned()
            } else {
                format!("Einladung offen bis {}", when(invitation.expires_at, now))
            };
            lines.push(format!("{}: {status}", invitation.name));
        }
        if lines.is_empty() {
            lines.push(format!(
                "Noch keine Kontakte.{}",
                inline(self.hints, Hint::InviteSomeone)
            ));
        }
        for line in lines {
            self.say(&line);
        }
        Ok(())
    }

    /// Changes the state of a contact, stores it and delivers the resulting
    /// envelopes in order.
    ///
    /// If the first delivery fails, the change is rolled back unless
    /// something else changed the contact in between.
    async fn change_and_send<T>(
        &mut self,
        name: &str,
        change: impl FnOnce(&mut ContactState, u64) -> Result<(T, Vec<[u8; ENVELOPE_LEN]>)>,
    ) -> Result<T> {
        let now = self.now();
        let (value, envelopes, before, after) = self.store.update(|device| {
            let contact = contact_mut(device, name)?;
            let before = contact.state.clone();
            let (value, envelopes) = change(&mut contact.state, now)?;
            Ok::<_, Fail>((value, envelopes, before, contact.state.clone()))
        })?;
        let peer = after.contact.peer.clone();
        for (i, envelope) in envelopes.iter().enumerate() {
            match self
                .relay
                .deliver(&peer.relay, &peer.mailbox, &peer.send_token, envelope)
                .await
            {
                Ok(()) => {}
                Err(RelayError::UnknownMailbox) => {
                    self.gone(name).await?;
                    return Err(fail!(
                        "{name} hat die Verbindung beendet (das Postfach gibt es nicht mehr)."
                    ));
                }
                Err(error) if i == 0 => {
                    self.store.update(|device| {
                        let contact = contact_mut(device, name)?;
                        if same(&contact.state, &after) {
                            contact.state = before;
                        }
                        Ok::<_, Fail>(())
                    })?;
                    return Err(fail!("Nicht zugestellt, nichts geändert: {error}"));
                }
                Err(error) => self.say(&format!("Warnung: nicht alles zugestellt: {error}")),
            }
        }
        Ok(value)
    }

    /// The mailbox of the contact is gone: the contact removed this side.
    /// Marks the contact as ended and deletes the own mailbox.
    pub(crate) async fn gone(&mut self, name: &str) -> Result<()> {
        let (relay, mailbox, token) = self.store.update(|device| {
            let contact = contact_mut(device, name)?;
            contact.state.ended = true;
            Ok::<_, Fail>((
                contact.own_relay,
                contact.state.contact.own_mailbox,
                contact.read_token,
            ))
        })?;
        self.delete_own(&relay, &mailbox, &token).await;
        Ok(())
    }

    /// Deletes an own mailbox, best effort.
    pub(crate) async fn delete_own(
        &mut self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        token: &Token,
    ) {
        if let Err(error) = self.relay.delete_mailbox(relay, mailbox, token).await {
            self.warn_unless_gone(&error);
        }
    }
}

/// What [`Ctx::overview`] returns. Times are Unix seconds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Overview {
    pub now: u64,
    pub contacts: Vec<ContactView>,
    pub invitations: Vec<InvitationView>,
}

/// One contact as shown to the person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ContactView {
    pub name: String,
    pub status: Status,
    /// The open request from this contact, if any.
    pub incoming: Option<IncomingView>,
    /// Sending time of the own open request, if any.
    pub outgoing_since: Option<u64>,
    /// Earliest time of the next own request; at most `now` means now.
    pub next_request_at: u64,
    /// The latest response to an own request.
    pub last_response: Option<LastResponse>,
    /// How often this contact may ask; set by the own side.
    pub own_rules: Rules,
    /// How often the own side may ask this contact; set by the contact.
    pub peer_rules: Rules,
}

/// Where a contact stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Status {
    /// Accepted an invitation; waiting for the inviting side's `reveal`.
    /// There is no code to show yet.
    AwaitingReveal,
    /// Accepted an invitation; waiting for the other side to confirm.
    AwaitingHelloOk {
        code: String,
    },
    /// The other side accepted the own invitation; compare and confirm.
    AwaitingConfirmation {
        code: String,
    },
    Paired,
    /// One side ended the contact.
    Ended,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IncomingView {
    pub since: u64,
    pub expires_at: u64,
    /// Reason of the reported state already sent for it.
    pub reported: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LastResponse {
    Manual { at: u64 },
    Reported { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InvitationView {
    pub name: String,
    pub expires_at: u64,
    /// Expired, but an acceptance sent in time may still arrive.
    pub expired: bool,
}

fn contact_view(contact: &StoredContact, now: u64) -> ContactView {
    let state = &contact.state;
    let code = state.contact.verification_code();
    let status = if state.ended {
        Status::Ended
    } else {
        // Without a code (a pairing begun with version 1) the pairing waits
        // to be ended as failed by the next fetch.
        match (state.contact.state, code) {
            (PairingState::Paired, _) => Status::Paired,
            (PairingState::AwaitingHelloOk, Some(code)) => Status::AwaitingHelloOk { code },
            (PairingState::AwaitingConfirmation, Some(code)) => {
                Status::AwaitingConfirmation { code }
            }
            _ => Status::AwaitingReveal,
        }
    };
    let incoming = state.open_request(now).and_then(|id| {
        let entry = state.incoming.entries.iter().find(|e| e.id == id)?;
        Some(IncomingView {
            since: entry.at,
            expires_at: entry.at + u64::from(state.own_rules.expiry_minutes) * 60,
            reported: entry.reported.map(|r| r.as_str().to_owned()),
        })
    });
    let last_response =
        state
            .outgoing
            .entries
            .iter()
            .rev()
            .find_map(|e| match (e.answered_at, &e.reported) {
                (Some(at), _) => Some(LastResponse::Manual { at }),
                (None, Some(reason)) => Some(LastResponse::Reported {
                    reason: reason.as_str().to_owned(),
                }),
                (None, None) => None,
            });
    ContactView {
        name: contact.name.clone(),
        status,
        incoming,
        outgoing_since: state.outgoing.open(&state.peer_rules, now).map(|e| e.at),
        next_request_at: state.next_request_at(now),
        last_response,
        own_rules: state.own_rules,
        peer_rules: state.peer_rules,
    }
}

/// Path of the state file if none is given: `$XDG_DATA_HOME/lebenszeichen/device.json`,
/// falling back to `~/.local/share/lebenszeichen/device.json`.
pub fn default_state_path() -> Result<std::path::PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| Path::new(&home).join(".local").join("share"))
        })
        .ok_or_else(|| fail!("Weder XDG_DATA_HOME noch HOME gesetzt; bitte --state angeben."))?;
    Ok(data.join("lebenszeichen").join("device.json"))
}
