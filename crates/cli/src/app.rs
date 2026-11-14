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
use lz_core::wire::{ENVELOPE_LEN, Invitation, MailboxId, Reason, RelayUrl, Rules, Token};
use rand_core::{OsRng, RngCore};

use crate::relay::{Relay, RelayError, from_b64};
use crate::show::{self, when};

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
    ($($arg:tt)*) => { $crate::app::Fail(format!($($arg)*)) };
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

pub(crate) fn say(out: &mut dyn Write, text: &str) {
    // Output that cannot be written is not worth failing a command for.
    let _ = writeln!(out, "{text}");
}

fn random_token() -> Token {
    let mut token = [0; 32];
    OsRng.fill_bytes(&mut token);
    token
}

/// Compares two states by their stored form.
fn same(a: &ContactState, b: &ContactState) -> bool {
    serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
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

fn core(error: lz_core::Error) -> Fail {
    fail!("{error}")
}

impl<R: Relay> Ctx<R> {
    pub fn now(&self) -> u64 {
        self.clock.now()
    }

    pub(crate) fn say(&mut self, text: &str) {
        say(&mut self.out, text);
    }

    fn load(&self) -> Result<Device> {
        self.store.load().map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => fail!(
                "Kein Gerät unter {}. Zuerst: lz init",
                self.store.path().display()
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
                "Einladen braucht ein eigenes Relay mit Zugangstoken (lz init --relay … --access-token …)."
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
                .put_reader(&url, &mailbox, &read_token, &device.reader_id)
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
            "Gültig bis {} und nur einmal{}. Danach mit lz sync oder lz listen abholen.",
            when(invitation.expires_at, now),
            if invitation.voucher.is_some() {
                ", mit Gutschein für ein Postfach auf deinem Relay"
            } else {
                ""
            }
        ));
        Ok(code)
    }

    /// Accepts an invitation, sends `hello` and returns the verification
    /// code.
    pub async fn accept(&mut self, name: &str, code: &str) -> Result<String> {
        let invitation = Invitation::from_text(code)
            .map_err(|_| fail!("Das ist kein gültiger Einladungscode."))?;
        if self.now() > invitation.expires_at {
            return Err(fail!("Die Einladung ist abgelaufen."));
        }
        let device = self.load()?;
        name_is_free(&device, name)?;
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
                     Die Gegenseite kann mit lz invite --voucher einladen."
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
        let result = async {
            self.relay
                .put_reader(&own_relay, &mailbox, &read_token, &device.reader_id)
                .await?;
            let (contact, hello) =
                accept(&invitation, own, self.now(), &mut OsRng).map_err(core)?;
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
                })?;
            let code = contact.verification_code();
            self.store.update(|device| {
                name_is_free(device, name)?;
                device.contacts.push(StoredContact {
                    name: name.into(),
                    own_relay,
                    read_token,
                    state: ContactState::new(contact),
                });
                Ok::<_, Fail>(())
            })?;
            Ok::<_, Fail>(code)
        }
        .await;
        let code = match result {
            Ok(code) => code,
            Err(error) => {
                let _ = self
                    .relay
                    .delete_mailbox(&own_relay, &mailbox, &read_token)
                    .await;
                return Err(error);
            }
        };
        self.say(&format!("Einladung angenommen. Prüfcode: {code}"));
        self.say(&format!(
            "Vergleiche ihn mit dem Prüfcode bei {name}. Gekoppelt seid ihr, sobald {name} bestätigt hat (lz sync).\n\
             Stimmt er nicht überein: lz reject {name}"
        ));
        Ok(code)
    }

    /// Confirms a contact that accepted an own invitation.
    pub async fn confirm(&mut self, name: &str) -> Result<()> {
        self.change_and_send(name, |state, now| {
            if state.ended {
                return Err(fail!("Die Verbindung mit {name} ist beendet."));
            }
            match state.contact.state {
                PairingState::AwaitingConfirmation => {}
                PairingState::AwaitingHelloOk => {
                    return Err(fail!(
                        "{name} muss bestätigen, nicht du. Danach bringt lz sync die Bestätigung."
                    ));
                }
                PairingState::Paired => return Err(fail!("{name} ist schon gekoppelt.")),
            }
            let envelopes = state.confirm(now, &mut OsRng).map_err(core)?;
            Ok(((), envelopes.to_vec()))
        })
        .await?;
        self.say(&format!("{name} ist jetzt gekoppelt."));
        Ok(())
    }

    /// Asks a contact for a sign of life.
    pub async fn request(&mut self, name: &str) -> Result<()> {
        self.change_and_send(name, |state, now| {
            check_active(name, state)?;
            match state.request(now, &mut OsRng) {
                Ok((_, envelope)) => Ok(((), vec![envelope])),
                Err(lz_core::Error::NotYet(at)) => Err(fail!(
                    "Anfrage an {name} erst wieder möglich ab {}.",
                    when(at, now)
                )),
                Err(error) => Err(core(error)),
            }
        })
        .await?;
        self.say(&format!("Anfrage an {name} gesendet."));
        Ok(())
    }

    /// Answers the open request of a contact.
    pub async fn respond(&mut self, name: &str) -> Result<()> {
        self.change_and_send(name, |state, now| {
            check_active(name, state)?;
            let id = state
                .open_request(now)
                .ok_or_else(|| fail!("Keine offene Anfrage von {name}."))?;
            let envelope = state.respond(&id, now, &mut OsRng).map_err(core)?;
            Ok(((), vec![envelope]))
        })
        .await?;
        self.say(&format!("Lebenszeichen an {name} gesendet ✓"));
        Ok(())
    }

    /// Sends a reported state for the open request of a contact.
    pub async fn report(&mut self, name: &str, reason: &str) -> Result<()> {
        let reason = Reason::new(reason.trim())
            .map_err(|_| fail!("Der Grund ist zu lang (höchstens 64 Byte)."))?;
        if reason.is_empty() {
            return Err(fail!("Der Grund darf nicht leer sein."));
        }
        self.change_and_send(name, |state, now| {
            check_active(name, state)?;
            let id = state
                .open_request(now)
                .ok_or_else(|| fail!("Keine offene Anfrage von {name}."))?;
            match state.report(&id, reason, now, &mut OsRng) {
                Ok(envelope) => Ok(((), vec![envelope])),
                Err(lz_core::Error::Unexpected) => {
                    Err(fail!("Zu dieser Anfrage ist schon ein Zustand gemeldet."))
                }
                Err(error) => Err(core(error)),
            }
        })
        .await?;
        self.say(&format!(
            "An {name} gemeldet: {}. Die Anfrage bleibt offen, bis du mit lz respond {name} antwortest.",
            show::reported(&reason)
        ));
        Ok(())
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
        Ok(())
    }

    /// Ends a contact or withdraws an invitation.
    pub async fn remove(&mut self, name: &str) -> Result<()> {
        let device = self.load()?;
        if let Some(invitation) = device.invitations.iter().find(|i| i.name == name) {
            let own = &invitation.pending.own;
            if let Err(error) = self
                .relay
                .delete_mailbox(&own.relay, &own.mailbox, &invitation.read_token)
                .await
            {
                self.warn_unless_gone(&error);
            }
            self.store.update(|device| {
                device.invitations.retain(|i| i.name != name);
                Ok::<_, Fail>(())
            })?;
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
        let hint = match contact.state.contact.state {
            PairingState::Paired => {
                return Err(fail!(
                    "{name} ist schon gekoppelt. Entfernen mit: lz remove {name}"
                ));
            }
            PairingState::AwaitingConfirmation => format!("Neu einladen mit: lz invite {name}"),
            PairingState::AwaitingHelloOk => format!("Bitte {name} um eine neue Einladung."),
        };
        let seen = if self.end_contact(name).await? {
            "; die Gegenseite sieht das"
        } else {
            ""
        };
        self.say(&format!("{name} abgelehnt{seen}. {hint}"));
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
        Ok(delivered)
    }

    fn warn_unless_gone(&mut self, error: &RelayError) {
        if *error != RelayError::UnknownMailbox {
            self.say(&format!(
                "Warnung: eigenes Postfach nicht gelöscht ({error}). Das Relay räumt es nach 90 Tagen auf."
            ));
        }
    }

    /// Lists contacts and open invitations.
    pub fn contacts(&mut self) -> Result<()> {
        let device = self.load()?;
        let now = self.now();
        let mut lines = Vec::new();
        for contact in &device.contacts {
            let name = &contact.name;
            let state = &contact.state;
            let status = if state.ended {
                "beendet; entfernen mit: lz remove".to_owned()
            } else {
                match state.contact.state {
                    PairingState::AwaitingHelloOk => format!(
                        "wartet auf die Bestätigung durch {name}, Prüfcode {}; \
                         stimmt er nicht: lz reject {name}",
                        state.contact.verification_code()
                    ),
                    PairingState::AwaitingConfirmation => format!(
                        "hat angenommen, Prüfcode {}; bestätigen mit: lz confirm {name}, \
                         ablehnen mit: lz reject {name}",
                        state.contact.verification_code()
                    ),
                    PairingState::Paired => "gekoppelt".to_owned(),
                }
            };
            lines.push(format!("{name}: {status}"));
            if state.ended || !state.is_paired() {
                continue;
            }
            if let Some(id) = state.open_request(now) {
                let entry = state.incoming.entries.iter().find(|e| e.id == id);
                if let Some(entry) = entry {
                    let expiry = entry.at + u64::from(state.own_rules.expiry_minutes) * 60;
                    let mut line = format!(
                        "  offene Anfrage seit {}, läuft ab {}",
                        when(entry.at, now),
                        when(expiry, now)
                    );
                    if let Some(reason) = &entry.reported {
                        line.push_str(&format!("; gemeldet: {}", show::reported(reason)));
                    }
                    lines.push(line);
                }
            }
            if let Some(open) = state.outgoing.open(&state.peer_rules, now) {
                lines.push(format!(
                    "  eigene Anfrage offen seit {}",
                    when(open.at, now)
                ));
            }
            let next = state.next_request_at(now);
            if next <= now {
                lines.push("  Anfrage möglich".to_owned());
            } else {
                lines.push(format!("  Anfrage wieder möglich ab {}", when(next, now)));
            }
            let last = state
                .outgoing
                .entries
                .iter()
                .rev()
                .find(|e| e.answered_at.is_some() || e.reported.is_some());
            if let Some(entry) = last {
                let text = match (entry.answered_at, &entry.reported) {
                    (Some(at), _) => format!("✓ {}", when(at, now)),
                    (None, Some(reason)) => show::reported(reason),
                    (None, None) => unreachable!("filtered above"),
                };
                lines.push(format!("  letzte Antwort: {text}"));
            }
        }
        for invitation in &device.invitations {
            let expires = invitation.pending.expires_at;
            let status = if expires < now {
                "Einladung abgelaufen; eine rechtzeitige Annahme wird noch abgeholt".to_owned()
            } else {
                format!("Einladung offen bis {}", when(expires, now))
            };
            lines.push(format!("{}: {status}", invitation.name));
        }
        if lines.is_empty() {
            lines.push("Noch keine Kontakte. Einladen mit: lz invite <name>".to_owned());
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
