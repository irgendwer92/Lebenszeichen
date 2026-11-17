//! Fetching messages: `lz sync` and `lz listen`.
//!
//! All own mailboxes (of contacts and of open invitations) are subscribed,
//! one connection per relay. Each message changes the stored state first and
//! is acknowledged only afterwards, so a crash in between at worst delivers
//! it again, where the core drops it as a duplicate.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use lz_core::contact::{ContactState, Event};
use lz_core::group::{SyncEvent, SyncIgnored};
use lz_core::pairing::Address;
use lz_core::store::{Device, StoredContact};
use lz_core::wire::{ENVELOPE_LEN, MailboxId, RelayUrl, Token};
use rand_core::OsRng;
use tokio::sync::mpsc;

use crate::ctx::{Ctx, Fail, Result};
use crate::i18n::Texts;
use crate::relay::{Connection, Delivery, Relay, RelayError, ServerFrame};
use crate::reports::Failed;
use crate::show::Notice;

/// How long to wait for the relay to confirm subscriptions.
const SUBSCRIBE_WAIT: Duration = Duration::from_secs(10);

/// How often `listen` reconnects lost connections, subscribes new mailboxes
/// and cleans up expired invitations.
const LISTEN_TICK: Duration = Duration::from_secs(10);

/// An open connection with what it has subscribed.
struct Live {
    connection: Box<dyn Connection>,
    subscribed: HashSet<MailboxId>,
    /// Subscribed, but neither `subscribed` nor `error` came back yet.
    awaiting: HashSet<MailboxId>,
}

/// Something to do after a message was stored and acknowledged.
enum FollowUp {
    /// Deliver an envelope to the contact.
    Send {
        name: String,
        peer: Address,
        envelope: Box<[u8; ENVELOPE_LEN]>,
        what: Sent,
    },
    /// The contact ended; delete the own mailbox.
    DeleteOwn {
        relay: RelayUrl,
        mailbox: MailboxId,
        token: Token,
    },
    /// A contact arrived from another own device: register this device as
    /// a reader of its own mailbox.
    Register {
        relay: RelayUrl,
        mailbox: MailboxId,
        token: Token,
    },
    /// This device was removed.
    Removed,
}

/// What a [`FollowUp::Send`] delivers, for the text if it fails.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sent {
    Rules,
    Reveal,
    /// `end` after a failed pairing; a missing mailbox is no news then.
    End,
}

/// How [`Ctx::fetch`] runs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Fetch what is waiting, then end.
    Sync,
    /// Stay connected.
    Listen,
    /// Like `Sync`, without saying that nothing came. Used while waiting
    /// for a new device.
    Quiet,
}

/// Own mailboxes grouped by relay, with their read tokens: the sync mailbox
/// first, then those of contacts and open invitations.
pub(crate) fn own_mailboxes(device: &Device) -> HashMap<RelayUrl, Vec<(MailboxId, Token)>> {
    let mut map: HashMap<RelayUrl, Vec<(MailboxId, Token)>> = HashMap::new();
    if let Some(group) = &device.group {
        map.entry(group.relay)
            .or_default()
            .push((group.mailbox, group.read_token));
    }
    for contact in device.contacts.iter().filter(|c| !c.state.ended) {
        map.entry(contact.own_relay)
            .or_default()
            .push((contact.state.contact.own_mailbox, contact.read_token));
    }
    for invitation in &device.invitations {
        let own = &invitation.pending.own;
        map.entry(own.relay)
            .or_default()
            .push((own.mailbox, invitation.read_token));
    }
    map
}

fn name_of(device: &Device, relay: &RelayUrl, mailbox: &MailboxId) -> Option<String> {
    device
        .contacts
        .iter()
        .find(|c| c.own_relay == *relay && c.state.contact.own_mailbox == *mailbox)
        .map(|c| c.name.clone())
        .or_else(|| {
            device
                .invitations
                .iter()
                .find(|i| i.pending.own.relay == *relay && i.pending.own.mailbox == *mailbox)
                .map(|i| i.name.clone())
        })
}

impl<R: Relay> Ctx<R> {
    /// Fetches everything waiting and returns what happened.
    pub async fn sync(&mut self) -> Result<Vec<Notice>> {
        self.fetch(Mode::Sync).await
    }

    /// Stays connected and shows events as they come. Returns only on
    /// errors with the state file, or when this device was removed.
    pub async fn listen(&mut self) -> Result<()> {
        self.fetch(Mode::Listen).await.map(drop)
    }

    pub(crate) async fn fetch(&mut self, mode: Mode) -> Result<Vec<Notice>> {
        let listen = mode == Mode::Listen;
        let (deliveries, mut incoming) = mpsc::unbounded_channel();
        let mut live: HashMap<RelayUrl, Live> = HashMap::new();
        let mut notices = Vec::new();
        self.retry_push_url().await?;
        self.flush_outbox().await?;
        self.check_pairings(&mut notices).await?;
        self.reconcile(&mut live, &deliveries, &mut notices).await?;
        let mut failed_reports = Failed::new();
        if live.is_empty() && !listen {
            self.send_due_reports(&mut notices, &mut failed_reports)
                .await?;
            self.clean_invitations().await?;
            if notices.is_empty() && mode == Mode::Sync {
                self.say(&tr!(self.texts(), "out-no-mailboxes"));
            }
            return Ok(notices);
        }
        if listen {
            let t = self.texts();
            let text = tr!(
                t,
                "out-listening",
                hint = &crate::show::with_hints(
                    String::new(),
                    self.hints,
                    t,
                    &[crate::show::Hint::StopListening]
                )
            );
            self.say(&text);
        }
        let mut tick = tokio::time::interval(LISTEN_TICK);
        tick.tick().await;

        loop {
            let delivery = if listen {
                tokio::select! {
                    delivery = incoming.recv() => delivery,
                    _ = tick.tick() => {
                        self.flush_outbox().await?;
                        self.check_pairings(&mut notices).await?;
                        self.reconcile(&mut live, &deliveries, &mut notices).await?;
                        self.clean_invitations().await?;
                        self.send_due_reports(&mut notices, &mut failed_reports).await?;
                        continue;
                    }
                }
            } else {
                let waiting = live.values().any(|l| !l.awaiting.is_empty());
                let wait = if waiting { SUBSCRIBE_WAIT } else { self.idle };
                match tokio::time::timeout(wait, incoming.recv()).await {
                    Ok(delivery) => delivery,
                    Err(_) => break,
                }
            };
            // The sender half lives in this function, so the channel never ends.
            let Some(Delivery { relay, frame }) = delivery else {
                break;
            };
            match frame {
                ServerFrame::Subscribed { mailbox } => {
                    if let Some(l) = live.get_mut(&relay) {
                        l.awaiting.remove(&mailbox);
                    }
                }
                ServerFrame::Error { mailbox, code } => {
                    if let (Some(l), Some(mailbox)) = (live.get_mut(&relay), mailbox) {
                        l.awaiting.remove(&mailbox);
                    }
                    let device = self.load()?;
                    let sync_mailbox = device
                        .group
                        .as_ref()
                        .is_some_and(|g| g.relay == relay && Some(g.mailbox) == mailbox);
                    if sync_mailbox && code == "unknown_reader" {
                        self.removed(&mut notices)?;
                        return Ok(notices);
                    }
                    // Mailboxes of ended contacts and of withdrawn invitations
                    // are deleted on purpose, possibly by another command.
                    let deleted_on_purpose = code == "unknown_mailbox"
                        && !sync_mailbox
                        && mailbox.is_some_and(|m| {
                            name_of(&device, &relay, &m).is_none()
                                || device.contacts.iter().any(|c| {
                                    c.state.ended
                                        && c.own_relay == relay
                                        && c.state.contact.own_mailbox == m
                                })
                        });
                    if deleted_on_purpose {
                        continue;
                    }
                    let t = self.texts();
                    let whose = if sync_mailbox {
                        tr!(t, "problem-whose-group")
                    } else {
                        mailbox
                            .and_then(|m| name_of(&device, &relay, &m))
                            .map_or_else(String::new, |name| {
                                tr!(t, "problem-whose-contact", name = &name)
                            })
                    };
                    let notice = Notice::Problem(tr!(
                        t,
                        "problem-relay-error",
                        relay = &relay.to_string(),
                        whose = &whose,
                        code = &code
                    ));
                    self.show(&notice);
                    notices.push(notice);
                }
                ServerFrame::Message { mailbox, seq, data } => {
                    let (received, follow_ups, queued) = self.receive(&relay, &mailbox, &data)?;
                    if let Some(l) = live.get_mut(&relay) {
                        // A lost acknowledgement only means the message comes again.
                        let _ = l.connection.ack(&mailbox, seq);
                    }
                    for notice in received {
                        self.show(&notice);
                        notices.push(notice);
                    }
                    let mut subscribe = false;
                    for follow_up in follow_ups {
                        match follow_up {
                            FollowUp::Removed => {
                                self.removed(&mut notices)?;
                                return Ok(notices);
                            }
                            FollowUp::Register { .. } => subscribe = true,
                            _ => {}
                        }
                        if let Some(notice) = self.follow_up(follow_up).await? {
                            self.show(&notice);
                            notices.push(notice);
                        }
                    }
                    if queued {
                        self.flush_outbox().await?;
                    }
                    if subscribe {
                        self.reconcile(&mut live, &deliveries, &mut notices).await?;
                    }
                }
                ServerFrame::Closed => {
                    live.remove(&relay);
                    if listen {
                        let text = tr!(
                            self.texts(),
                            "out-connection-lost",
                            relay = &relay.to_string()
                        );
                        self.say(&text);
                    } else if live.is_empty() {
                        break;
                    }
                }
            }
        }
        self.send_due_reports(&mut notices, &mut failed_reports)
            .await?;
        self.clean_invitations().await?;
        if mode == Mode::Sync && notices.iter().all(Notice::is_dropped) {
            self.say(&tr!(self.texts(), "out-no-new-messages"));
        }
        Ok(notices)
    }

    /// This device was removed by another one: says so and deletes the
    /// state.
    fn removed(&mut self, notices: &mut Vec<Notice>) -> Result<()> {
        let notice = Notice::Removed;
        self.show(&notice);
        notices.push(notice);
        self.delete_state()
    }

    pub(crate) fn show(&mut self, notice: &Notice) {
        if let Some(on_notice) = &mut self.on_notice {
            on_notice(notice);
        }
        if self.verbose || !notice.is_dropped() {
            let text = notice.text(self.texts(), self.now(), self.hints);
            self.say(&text);
        }
    }

    /// Connects to relays without a connection and subscribes mailboxes that
    /// are not subscribed yet.
    async fn reconcile(
        &mut self,
        live: &mut HashMap<RelayUrl, Live>,
        deliveries: &mpsc::UnboundedSender<Delivery>,
        notices: &mut Vec<Notice>,
    ) -> Result<()> {
        let device = self.load()?;
        let mut relays: Vec<_> = own_mailboxes(&device).into_iter().collect();
        // The sync mailbox first, so that what another own device did
        // tends to arrive before the messages of contacts about it.
        let group_relay = device.group.as_ref().map(|g| g.relay);
        relays.sort_by_key(|(relay, _)| Some(*relay) != group_relay);
        for (relay, mailboxes) in relays {
            let l = match live.entry(relay) {
                Entry::Occupied(slot) => slot.into_mut(),
                Entry::Vacant(slot) => match self.relay.connect(&relay, deliveries.clone()).await {
                    Ok(connection) => slot.insert(Live {
                        connection,
                        subscribed: HashSet::new(),
                        awaiting: HashSet::new(),
                    }),
                    Err(error) => {
                        let notice = Notice::Problem(tr!(
                            self.texts(),
                            "problem-relay",
                            relay = &relay.to_string(),
                            error = &error.text(self.texts())
                        ));
                        self.show(&notice);
                        notices.push(notice);
                        continue;
                    }
                },
            };
            for (mailbox, token) in mailboxes {
                if l.subscribed.contains(&mailbox) {
                    continue;
                }
                if l.connection
                    .subscribe(&mailbox, &token, &device.reader_id)
                    .is_ok()
                {
                    l.subscribed.insert(mailbox);
                    l.awaiting.insert(mailbox);
                }
            }
        }
        Ok(())
    }

    /// Applies one message to the stored state.
    fn receive(
        &mut self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        data: &[u8],
    ) -> Result<(Vec<Notice>, Vec<FollowUp>, bool)> {
        let now = self.now();
        let t = self.texts();
        self.update(|device| {
            let queued = device.outbox.len();
            let (notices, follow_ups) = receive_into(t, device, relay, mailbox, data, now)?;
            // The waiting time for a reported answer starts on arrival.
            device.refresh_arrivals(now);
            Ok((notices, follow_ups, device.outbox.len() > queued))
        })
    }

    /// Network work after a message was handled. Returns a notice if the
    /// contact turned out to be gone.
    async fn follow_up(&mut self, follow_up: FollowUp) -> Result<Option<Notice>> {
        match follow_up {
            FollowUp::Removed => Ok(None),
            FollowUp::Register {
                relay,
                mailbox,
                token,
            } => {
                let device = self.load()?;
                match self
                    .relay
                    .put_reader(
                        &relay,
                        &mailbox,
                        &token,
                        &device.reader_id,
                        device.push_url.as_deref(),
                    )
                    .await
                {
                    Ok(()) => Ok(None),
                    Err(error) => {
                        let name = name_of(&device, &relay, &mailbox).unwrap_or_default();
                        let t = self.texts();
                        Ok(Some(Notice::Problem(tr!(
                            t,
                            "problem-not-registered",
                            name = &name,
                            error = &error.text(t)
                        ))))
                    }
                }
            }
            FollowUp::DeleteOwn {
                relay,
                mailbox,
                token,
            } => {
                self.delete_own(&relay, &mailbox, &token).await;
                Ok(None)
            }
            FollowUp::Send {
                name,
                peer,
                envelope,
                what,
            } => match self
                .relay
                .deliver(&peer.relay, &peer.mailbox, &peer.send_token, &envelope[..])
                .await
            {
                Ok(()) => Ok(None),
                Err(RelayError::UnknownMailbox) if what == Sent::End => Ok(None),
                Err(RelayError::UnknownMailbox) => {
                    self.gone(&name).await?;
                    Ok(Some(Notice::Ended { name }))
                }
                Err(error) => {
                    let t = self.texts();
                    let what = tr!(
                        t,
                        match what {
                            Sent::Rules => "problem-what-rules",
                            // Comes again when the other side repeats `hello`.
                            Sent::Reveal => "problem-what-reveal",
                            Sent::End => "problem-what-end",
                        }
                    );
                    Ok(Some(Notice::Problem(tr!(
                        t,
                        "problem-not-delivered",
                        what = &what,
                        name = &name,
                        error = &error.text(t)
                    ))))
                }
            },
        }
    }

    /// Pairings this device accepted and that still wait for `reveal`:
    /// sends `hello` again from time to time, in case `reveal` got lost,
    /// and ends those that can no longer complete.
    async fn check_pairings(&mut self, notices: &mut Vec<Notice>) -> Result<()> {
        let now = self.now();
        let (repeat, failed) = self.update(|device| {
            let mut repeat = Vec::new();
            let mut failed = Vec::new();
            for contact in device.contacts.iter_mut().filter(|c| !c.state.ended) {
                if contact.state.pairing_failed(now) {
                    let envelope = contact
                        .state
                        .end(now, &mut OsRng)
                        .map_err(crate::ctx::core)?;
                    failed.push((
                        contact.name.clone(),
                        contact.state.contact.peer.clone(),
                        envelope,
                        contact.own_relay,
                        contact.state.contact.own_mailbox,
                        contact.read_token,
                    ));
                } else if let Some(hello) = contact.state.contact.hello_to_repeat(now) {
                    repeat.push((
                        contact.name.clone(),
                        contact.state.contact.peer.clone(),
                        hello,
                    ));
                }
            }
            Ok((repeat, failed))
        })?;
        for (name, peer, envelope, relay, mailbox, token) in failed {
            // The other side may be gone already; nothing to tell then.
            let _ = self
                .relay
                .deliver(&peer.relay, &peer.mailbox, &peer.send_token, &envelope)
                .await;
            self.delete_own(&relay, &mailbox, &token).await;
            let notice = Notice::PairingFailed { name };
            self.show(&notice);
            notices.push(notice);
        }
        for (name, peer, hello) in repeat {
            match self
                .relay
                .deliver(&peer.relay, &peer.mailbox, &peer.send_token, &hello)
                .await
            {
                Ok(()) => {}
                Err(RelayError::UnknownMailbox) => {
                    // The invitation is gone on the other side.
                    self.gone(&name).await?;
                    let notice = Notice::PairingFailed { name };
                    self.show(&notice);
                    notices.push(notice);
                }
                // Tried again later.
                Err(_) => {}
            }
        }
        Ok(())
    }

    /// Withdraws expired invitations and those of version 1, and deletes
    /// their mailboxes.
    async fn clean_invitations(&mut self) -> Result<()> {
        let now = self.now();
        let expired = self.update(|device| {
            let (expired, open) = std::mem::take(&mut device.invitations)
                .into_iter()
                .partition::<Vec<_>, _>(|i| {
                    i.pending.keep_until() < now || i.pending.is_outdated()
                });
            device.invitations = open;
            Ok(expired)
        })?;
        for invitation in expired {
            let own = &invitation.pending.own;
            self.delete_own(&own.relay, &own.mailbox, &invitation.read_token)
                .await;
            let name = &invitation.name;
            let id = if invitation.pending.is_outdated() {
                "out-invitation-outdated-removed"
            } else {
                "out-invitation-expired-removed"
            };
            self.say(&tr!(self.texts(), id, name = name));
        }
        Ok(())
    }
}

/// Applies one message to the state of the device.
fn receive_into(
    t: Texts,
    device: &mut Device,
    relay: &RelayUrl,
    mailbox: &MailboxId,
    data: &[u8],
    now: u64,
) -> Result<(Vec<Notice>, Vec<FollowUp>)> {
    if device
        .group
        .as_ref()
        .is_some_and(|g| g.relay == *relay && g.mailbox == *mailbox)
    {
        return Ok(receive_sync(t, device, data, now));
    }
    if let Some(contact) = device
        .contacts
        .iter_mut()
        .find(|c| c.own_relay == *relay && c.state.contact.own_mailbox == *mailbox)
    {
        let own_mailbox = contact.state.contact.own_mailbox;
        let name = contact.name.clone();
        let (mut notices, follow_ups, paired) = receive_for_contact(t, contact, data, now);
        if paired {
            // `receive_for_contact` has announced the rules, so the
            // record carries their stamp.
            if let Err(error) = device.sync_contact_added(&own_mailbox, now, &mut OsRng) {
                notices.push(Notice::Problem(tr!(
                    t,
                    "problem-not-handed-over",
                    name = &name,
                    error = &error.to_string()
                )));
            }
        }
        return Ok((notices, follow_ups));
    }
    let Some(index) = device
        .invitations
        .iter()
        .position(|i| i.pending.own.relay == *relay && i.pending.own.mailbox == *mailbox)
    else {
        return Ok((
            vec![Notice::Dropped {
                name: None,
                why: tr!(t, "dropped-unknown-mailbox"),
            }],
            vec![],
        ));
    };
    let invitation = &device.invitations[index];
    let name = invitation.name.clone();
    match invitation.pending.receive_hello(data, now) {
        Ok(contact) => {
            // Contact, secret value and `reveal` are stored in this one
            // step, before `reveal` goes out; a second `hello` then finds
            // the contact and gets none.
            let reveal = contact.reveal(now, &mut OsRng).map_err(crate::ctx::core)?;
            let code = contact
                .verification_code()
                .ok_or_else(|| fail!(t, "error-no-code", name = &name))?;
            let peer = contact.peer.clone();
            let invitation = device.invitations.remove(index);
            device.contacts.push(StoredContact {
                name: name.clone(),
                own_relay: invitation.pending.own.relay,
                read_token: invitation.read_token,
                state: ContactState::new(contact),
            });
            Ok::<_, Fail>((
                vec![Notice::Accepted {
                    name: name.clone(),
                    code,
                }],
                vec![FollowUp::Send {
                    name,
                    peer,
                    envelope: Box::new(reveal),
                    what: Sent::Reveal,
                }],
            ))
        }
        Err(error) => Ok((
            vec![Notice::Dropped {
                name: Some(name),
                why: error.to_string(),
            }],
            vec![],
        )),
    }
}

/// Applies a message from the sync mailbox.
fn receive_sync(
    t: Texts,
    device: &mut Device,
    data: &[u8],
    now: u64,
) -> (Vec<Notice>, Vec<FollowUp>) {
    let before = device.devices.clone();
    // Sync messages do not say which device sent them; with only one other
    // device it is that one.
    let other = match before.as_slice() {
        [only] => Some(only.name.clone()),
        _ => None,
    };
    let events = match device.receive_sync(data, now) {
        Ok(events) => events,
        Err(error) => {
            return (
                vec![Notice::Dropped {
                    name: None,
                    why: tr!(t, "dropped-own-devices", why = &error.to_string()),
                }],
                vec![],
            );
        }
    };
    let name = |device: &Device, contact: &MailboxId| {
        device
            .contact_by_mailbox(contact)
            .map(|c| c.name.clone())
            .unwrap_or_default()
    };
    let mut notices = Vec::new();
    let mut follow_ups = Vec::new();
    for event in events {
        let notice = match event {
            SyncEvent::DeviceAdded { name, .. } => Notice::DeviceAdded { name },
            SyncEvent::DeviceRemoved { reader } => Notice::DeviceRemoved {
                name: before
                    .iter()
                    .find(|d| d.reader == reader)
                    .map(|d| d.name.clone()),
            },
            SyncEvent::Removed => {
                follow_ups.push(FollowUp::Removed);
                continue;
            }
            SyncEvent::Requested { contact, .. } => Notice::OwnRequested {
                name: name(device, &contact),
                device: other.clone(),
            },
            SyncEvent::Responded { contact, kind, .. } => Notice::OwnResponded {
                name: name(device, &contact),
                device: other.clone(),
                kind,
            },
            SyncEvent::RulesSet { contact, rules } => Notice::OwnRules {
                name: name(device, &contact),
                device: other.clone(),
                rules,
            },
            SyncEvent::ContactAdded {
                contact,
                relay,
                read_token,
            } => {
                follow_ups.push(FollowUp::Register {
                    relay,
                    mailbox: contact,
                    token: read_token,
                });
                Notice::OwnContactAdded {
                    name: name(device, &contact),
                    device: other.clone(),
                }
            }
            SyncEvent::ContactRemoved { name, .. } => Notice::OwnContactRemoved {
                name,
                device: other.clone(),
            },
            SyncEvent::StateHanded { reason } => Notice::OwnState {
                reason,
                device: other.clone(),
            },
            SyncEvent::PartStored | SyncEvent::Parked => continue,
            SyncEvent::Ignored(why) => Notice::Dropped {
                name: None,
                why: tr!(t, "dropped-own-devices", why = &ignored(t, why)),
            },
        };
        notices.push(notice);
    }
    (notices, follow_ups)
}

fn ignored(t: Texts, why: SyncIgnored) -> String {
    tr!(
        t,
        match why {
            SyncIgnored::Duplicate => "dropped-sync-duplicate",
            SyncIgnored::NotExpected => "dropped-sync-not-expected",
            SyncIgnored::UnknownType => "dropped-sync-unknown-type",
            SyncIgnored::Outdated => "dropped-sync-outdated",
            SyncIgnored::Known => "dropped-sync-known",
            SyncIgnored::Discarded => "dropped-sync-discarded",
        }
    )
}

/// Applies a message from a contact. Returns the notices, the network work
/// to do afterwards, and whether the pairing has just completed.
fn receive_for_contact(
    t: Texts,
    contact: &mut StoredContact,
    data: &[u8],
    now: u64,
) -> (Vec<Notice>, Vec<FollowUp>, bool) {
    let name = contact.name.clone();
    let event = match contact.state.receive(data, now) {
        Ok(event) => event,
        Err(error) => {
            return (
                vec![Notice::Dropped {
                    name: Some(name),
                    why: error.to_string(),
                }],
                vec![],
                false,
            );
        }
    };
    let delete_own = FollowUp::DeleteOwn {
        relay: contact.own_relay,
        mailbox: contact.state.contact.own_mailbox,
        token: contact.read_token,
    };
    match event {
        Event::Paired => {
            let (notices, follow_ups) = announce_after_pairing(t, contact, vec![], now);
            (notices, follow_ups, true)
        }
        Event::Revealed { code, paired } => {
            let revealed = Notice::Revealed {
                name: name.clone(),
                code,
                paired,
            };
            if paired {
                let (notices, follow_ups) = announce_after_pairing(t, contact, vec![revealed], now);
                (notices, follow_ups, true)
            } else {
                (vec![revealed], vec![], false)
            }
        }
        Event::RevealMismatch => {
            let mut follow_ups = Vec::new();
            match contact.state.end(now, &mut OsRng) {
                Ok(envelope) => follow_ups.push(FollowUp::Send {
                    name: name.clone(),
                    peer: contact.state.contact.peer.clone(),
                    envelope: Box::new(envelope),
                    what: Sent::End,
                }),
                // Cannot happen with valid keys; the contact is ended
                // locally all the same.
                Err(_) => contact.state.ended = true,
            }
            follow_ups.push(delete_own);
            (vec![Notice::RevealMismatch { name }], follow_ups, false)
        }
        Event::HelloOkKept => (
            vec![Notice::Dropped {
                name: Some(name),
                why: tr!(t, "dropped-confirmation-early"),
            }],
            vec![],
            false,
        ),
        Event::HelloAgain => match contact.state.contact.reveal(now, &mut OsRng) {
            Ok(envelope) => (
                vec![],
                vec![FollowUp::Send {
                    peer: contact.state.contact.peer.clone(),
                    name,
                    envelope: Box::new(envelope),
                    what: Sent::Reveal,
                }],
                false,
            ),
            Err(error) => (
                vec![Notice::Problem(tr!(
                    t,
                    "problem-reveal-not-encrypted",
                    name = &name,
                    error = &error.to_string()
                ))],
                vec![],
                false,
            ),
        },
        Event::Rules(rules) => (vec![Notice::Rules { name, rules }], vec![], false),
        Event::Request(_) => (vec![Notice::Request { name }], vec![], false),
        Event::Response { kind, at, .. } => {
            (vec![Notice::Response { name, kind, at }], vec![], false)
        }
        Event::Ended => (vec![Notice::Ended { name }], vec![delete_own], false),
        Event::Rejected => (vec![Notice::Rejected { name }], vec![delete_own], false),
        Event::SecondAcceptance => (
            vec![Notice::SecondAcceptance {
                name,
                paired: contact.state.is_paired(),
            }],
            vec![],
            false,
        ),
        Event::Ignored(why) => (
            vec![Notice::Dropped {
                name: Some(name),
                why: format!("{why:?}"),
            }],
            vec![],
            false,
        ),
    }
}

/// The pairing completed on the accepting side: the own rules go to the
/// contact.
fn announce_after_pairing(
    t: Texts,
    contact: &mut StoredContact,
    mut notices: Vec<Notice>,
    now: u64,
) -> (Vec<Notice>, Vec<FollowUp>) {
    let name = contact.name.clone();
    match contact.state.announce_rules(now, &mut OsRng) {
        Ok(envelope) => {
            notices.push(Notice::Paired { name: name.clone() });
            let send = FollowUp::Send {
                peer: contact.state.contact.peer.clone(),
                name,
                envelope: Box::new(envelope),
                what: Sent::Rules,
            };
            (notices, vec![send])
        }
        Err(error) => {
            notices.push(Notice::Problem(tr!(
                t,
                "problem-rules-not-encrypted",
                name = &name,
                error = &error.to_string()
            )));
            (notices, vec![])
        }
    }
}
