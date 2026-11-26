//! Own reader tokens (`docs/m10-satellite.md`, part 1).
//!
//! On a relay with `reader_tokens` every device reads each of its own
//! mailboxes with a token only it knows. A device gets there in one of
//! three ways:
//!
//! - It creates the mailbox: the sync mailbox in the new form, with its own
//!   token from the start; a contact mailbox in the old form, with a shared
//!   token that is the enrolment token for the other own devices
//!   (`contact_part` carries one token for all of them), and registers
//!   itself with its own token in the same step.
//! - Another own device registers it with an enrolment token (the
//!   handover), or it registers itself with the shared token from
//!   `contact_part`: it replaces that token with its own at once.
//! - It was set up before M10 and holds shared tokens only: it takes its
//!   own token on every mailbox on the next fetch ([`Ctx::ensure_own_tokens`]).
//!
//! Taking an own token is `PUT readers/{own}` with `token_hash`, authorised
//! by the token held so far. The new token is stored as `new_token` before
//! it is sent, so a lost answer is resolved on the next try: the token held
//! so far is refused once the relay has the new one, which is then tried.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use lz_core::store::{Device, ReadAccess};
use lz_core::wire::{MailboxId, RelayUrl, Token};

use crate::ctx::{Ctx, Result, random_token};
use crate::relay::{ReaderChange, Relay, RelayError};

/// One own mailbox of this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Own {
    /// The sync mailbox.
    Group,
    /// The own mailbox of a contact, by its identifier.
    Contact(MailboxId),
    /// The mailbox of an open invitation.
    Invitation(MailboxId),
    /// The channel mailbox of a satellite (M10), by its identifier.
    Channel(MailboxId),
}

/// What the device holds for one own mailbox.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Held {
    pub relay: RelayUrl,
    pub mailbox: MailboxId,
    /// The token it reads and manages the mailbox with.
    pub token: Token,
    pub own_token: Option<Token>,
    pub new_token: Option<Token>,
}

/// What the device holds for `own`, if it still has that mailbox.
pub(crate) fn held(device: &Device, own: Own) -> Option<Held> {
    fn of(
        access: &impl ReadAccess,
        new_token: Option<Token>,
        relay: RelayUrl,
        mailbox: MailboxId,
    ) -> Held {
        Held {
            relay,
            mailbox,
            token: access.token(),
            own_token: access.own_token().copied(),
            new_token,
        }
    }
    match own {
        Own::Group => device
            .group
            .as_ref()
            .map(|g| of(g, g.new_token, g.relay, g.mailbox)),
        Own::Contact(mailbox) => device
            .contacts
            .iter()
            .find(|c| !c.state.ended && c.state.contact.own_mailbox == mailbox)
            .map(|c| of(c, c.new_token, c.own_relay, mailbox)),
        Own::Invitation(mailbox) => device
            .invitations
            .iter()
            .find(|i| i.pending.own.mailbox == mailbox)
            .map(|i| of(i, i.new_token, i.pending.own.relay, mailbox)),
        Own::Channel(mailbox) => device
            .satellite_by_mailbox(&mailbox)
            .map(|s| of(s, s.new_token, s.relay, mailbox)),
    }
}

/// The own and the pending token of `own`, to change them.
fn tokens_mut(device: &mut Device, own: Own) -> Option<(&mut Option<Token>, &mut Option<Token>)> {
    match own {
        Own::Group => device
            .group
            .as_mut()
            .map(|g| (&mut g.own_token, &mut g.new_token)),
        Own::Contact(mailbox) => device
            .contacts
            .iter_mut()
            .find(|c| !c.state.ended && c.state.contact.own_mailbox == mailbox)
            .map(|c| (&mut c.own_token, &mut c.new_token)),
        Own::Invitation(mailbox) => device
            .invitations
            .iter_mut()
            .find(|i| i.pending.own.mailbox == mailbox)
            .map(|i| (&mut i.own_token, &mut i.new_token)),
        Own::Channel(mailbox) => device
            .satellites
            .iter_mut()
            .find(|s| s.mailbox == mailbox)
            .map(|s| (&mut s.own_token, &mut s.new_token)),
    }
}

/// Every own mailbox of the device: the sync mailbox, those of contacts
/// that have not ended, those of open invitations, and the channels of the
/// satellites.
pub(crate) fn all_own(device: &Device) -> Vec<Own> {
    let group = device.group.iter().map(|_| Own::Group);
    let contacts = device
        .contacts
        .iter()
        .filter(|c| !c.state.ended)
        .map(|c| Own::Contact(c.state.contact.own_mailbox));
    let invitations = device
        .invitations
        .iter()
        .map(|i| Own::Invitation(i.pending.own.mailbox));
    let channels = device.satellites.iter().map(|s| Own::Channel(s.mailbox));
    group
        .chain(contacts)
        .chain(invitations)
        .chain(channels)
        .collect()
}

/// The own mailbox `mailbox` on `relay`, if it is one.
pub(crate) fn own_of(device: &Device, relay: &RelayUrl, mailbox: &MailboxId) -> Option<Own> {
    all_own(device)
        .into_iter()
        .find(|&own| held(device, own).is_some_and(|h| h.relay == *relay && h.mailbox == *mailbox))
}

/// How taking an own token ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Taken {
    /// The device has its own token on the mailbox now (or had it).
    Done,
    /// The relay has no `reader_tokens`; the device stays with the token
    /// it holds.
    Unsupported,
    /// The relay refused, or could not be reached; the error says which.
    /// After `401` neither the token held so far nor the new one is
    /// accepted: the access of this device was withdrawn.
    Failed(RelayError),
    /// The device no longer has the mailbox, or the relay no longer has it.
    Gone,
}

/// Own mailboxes on which taking an own token was refused with `401`, per
/// state file and for the life of the process: the access of this device
/// was withdrawn, and asking again at every fetch would change nothing.
/// Taking one explicitly (on `unauthorized`, or when removing) still asks.
static REFUSED: Mutex<Vec<(PathBuf, RelayUrl, MailboxId)>> = Mutex::new(Vec::new());

fn refused() -> std::sync::MutexGuard<'static, Vec<(PathBuf, RelayUrl, MailboxId)>> {
    REFUSED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl<R: Relay> Ctx<R> {
    /// Whether `relay` offers `reader_tokens`; an error if it could not be
    /// asked.
    pub(crate) async fn reader_tokens(
        &self,
        relay: &RelayUrl,
    ) -> std::result::Result<bool, RelayError> {
        self.relay.features(relay).await.map(|f| f.reader_tokens)
    }

    /// Gives this device its own token on `own` if it has none yet and the
    /// relay offers `reader_tokens`, with its push address in the same
    /// step (which also registers it if it was not yet a reader). See the
    /// module documentation.
    pub(crate) async fn take_own_token(&mut self, own: Own) -> Result<Taken> {
        let device = self.load()?;
        let Some(held) = held(&device, own) else {
            return Ok(Taken::Gone);
        };
        if held.own_token.is_some() && held.new_token.is_none() {
            return Ok(Taken::Done);
        }
        match self.reader_tokens(&held.relay).await {
            Ok(true) => {}
            Ok(false) => return Ok(Taken::Unsupported),
            Err(error) => return Ok(Taken::Failed(error)),
        }
        // Stored before it goes out, so that a lost answer is resolved by
        // the next try.
        let new = match held.new_token {
            Some(token) => token,
            None => {
                let token = random_token();
                let stored = self.update(|device| {
                    Ok(tokens_mut(device, own).map(|(_, new)| *new = Some(token)))
                })?;
                if stored.is_none() {
                    return Ok(Taken::Gone);
                }
                token
            }
        };
        let change = ReaderChange {
            push_url: Some(device.push_url.as_deref()),
            token: Some(&new),
            role: None,
        };
        let reader = device.reader_id;
        let mut result = self
            .relay
            .put_reader(&held.relay, &held.mailbox, &held.token, &reader, change)
            .await;
        if let Err(RelayError::Refused { status: 401, .. }) = result {
            // Perhaps the relay took the new token already and the answer
            // got lost: then the new token itself is accepted.
            result = self
                .relay
                .put_reader(&held.relay, &held.mailbox, &new, &reader, change)
                .await;
        }
        match result {
            Ok(()) => {
                self.update(|device| {
                    if let Some((own_token, new_token)) = tokens_mut(device, own)
                        && *new_token == Some(new)
                    {
                        *own_token = Some(new);
                        *new_token = None;
                    }
                    Ok(())
                })?;
                Ok(Taken::Done)
            }
            Err(RelayError::UnknownMailbox) => Ok(Taken::Gone),
            Err(error) => {
                // Refused for good: a fresh token next time. After a
                // transport error the relay may have it, so it stays.
                if !error.maybe_stored() {
                    self.update(|device| {
                        if let Some((_, new_token)) = tokens_mut(device, own)
                            && *new_token == Some(new)
                        {
                            *new_token = None;
                        }
                        Ok(())
                    })?;
                }
                Ok(Taken::Failed(error))
            }
        }
    }

    /// Takes an own token on every own mailbox that has none yet, on relays
    /// with `reader_tokens`; idempotent, and cheap once done. Called at the
    /// start of every fetch, so that devices set up before M10 migrate and
    /// a lost answer is resolved. Failures are not reported here: a
    /// mailbox this device can no longer read shows up when it subscribes.
    pub(crate) async fn ensure_own_tokens(&mut self) -> Result<()> {
        let device = self.load()?;
        let path = self.store.path().to_owned();
        let mut offers: HashMap<RelayUrl, bool> = HashMap::new();
        for own in all_own(&device) {
            let Some(held) = held(&device, own) else {
                continue;
            };
            if held.own_token.is_some() && held.new_token.is_none() {
                continue;
            }
            let key = (path.clone(), held.relay, held.mailbox);
            if refused().contains(&key) {
                continue;
            }
            let offered = match offers.get(&held.relay) {
                Some(&offered) => offered,
                None => {
                    let offered = self.reader_tokens(&held.relay).await.unwrap_or(false);
                    offers.insert(held.relay, offered);
                    offered
                }
            };
            if offered
                && let Taken::Failed(RelayError::Refused { status: 401, .. }) =
                    self.take_own_token(own).await?
            {
                refused().push(key);
            }
        }
        Ok(())
    }

    /// Registers this device as a reader of `own` with its push address:
    /// with an own token in the same step where the relay offers
    /// `reader_tokens`, else as before with the token it holds.
    pub(crate) async fn register_self(
        &mut self,
        own: Own,
    ) -> Result<std::result::Result<(), RelayError>> {
        match self.take_own_token(own).await? {
            Taken::Done | Taken::Gone => Ok(Ok(())),
            Taken::Failed(error) => Ok(Err(error)),
            Taken::Unsupported => {
                let device = self.load()?;
                let Some(held) = held(&device, own) else {
                    return Ok(Ok(()));
                };
                Ok(self
                    .relay
                    .put_reader(
                        &held.relay,
                        &held.mailbox,
                        &held.token,
                        &device.reader_id,
                        ReaderChange::push(device.push_url.as_deref()),
                    )
                    .await)
            }
        }
    }

    /// Registers this device on a mailbox it has just created in the old
    /// form with `shared`: with its own token where the relay offers
    /// `reader_tokens`, which it returns, else as a legacy reader.
    pub(crate) async fn join_new_mailbox(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        shared: &Token,
        device: &Device,
    ) -> std::result::Result<Option<Token>, RelayError> {
        let own = match self.reader_tokens(relay).await? {
            true => Some(random_token()),
            false => None,
        };
        let change = ReaderChange {
            push_url: Some(device.push_url.as_deref()),
            token: own.as_ref(),
            role: None,
        };
        self.relay
            .put_reader(relay, mailbox, shared, &device.reader_id, change)
            .await?;
        Ok(own)
    }
}
