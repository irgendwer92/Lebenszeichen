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
use lz_core::pairing::Address;
use lz_core::store::{Device, Store, StoredContact};
use lz_core::wire::{ENVELOPE_LEN, MailboxId, RelayUrl, Token};
use rand_core::OsRng;
use tokio::sync::mpsc;

use crate::app::{Ctx, Fail, Result};
use crate::relay::{Connection, Delivery, Relay, RelayError, ServerFrame};
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
    None,
    /// Deliver an envelope to the contact.
    Send {
        name: String,
        peer: Address,
        envelope: Box<[u8; ENVELOPE_LEN]>,
    },
    /// The contact ended; delete the own mailbox.
    DeleteOwn {
        relay: RelayUrl,
        mailbox: MailboxId,
        token: Token,
    },
}

/// Own mailboxes grouped by relay, with their read tokens.
fn own_mailboxes(device: &Device) -> HashMap<RelayUrl, Vec<(MailboxId, Token)>> {
    let mut map: HashMap<RelayUrl, Vec<(MailboxId, Token)>> = HashMap::new();
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
        self.fetch(false).await
    }

    /// Stays connected and shows events as they come. Returns only on
    /// errors with the state file.
    pub async fn listen(&mut self) -> Result<()> {
        self.fetch(true).await.map(drop)
    }

    async fn fetch(&mut self, listen: bool) -> Result<Vec<Notice>> {
        let (deliveries, mut incoming) = mpsc::unbounded_channel();
        let mut live: HashMap<RelayUrl, Live> = HashMap::new();
        let mut notices = Vec::new();
        self.reconcile(&mut live, &deliveries, &mut notices).await?;
        if live.is_empty() && !listen {
            self.clean_invitations().await?;
            if notices.is_empty() {
                self.say("Keine Postfächer zum Abrufen.");
            }
            return Ok(notices);
        }
        if listen {
            self.say("Warte auf Nachrichten (Abbruch mit Strg+C) …");
        }
        let mut tick = tokio::time::interval(LISTEN_TICK);
        tick.tick().await;

        loop {
            let delivery = if listen {
                tokio::select! {
                    delivery = incoming.recv() => delivery,
                    _ = tick.tick() => {
                        self.reconcile(&mut live, &deliveries, &mut notices).await?;
                        self.clean_invitations().await?;
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
                    let device = self.store.load()?;
                    // The own mailbox of an ended contact was deleted on purpose.
                    let deleted_on_purpose = code == "unknown_mailbox"
                        && mailbox.is_some_and(|m| {
                            device.contacts.iter().any(|c| {
                                c.state.ended
                                    && c.own_relay == relay
                                    && c.state.contact.own_mailbox == m
                            })
                        });
                    if deleted_on_purpose {
                        continue;
                    }
                    let whose = mailbox
                        .and_then(|m| name_of(&device, &relay, &m))
                        .map_or_else(String::new, |name| format!(" (Postfach für {name})"));
                    let notice =
                        Notice::Problem(format!("Fehler vom Relay {relay}{whose}: {code}"));
                    self.show(&notice);
                    notices.push(notice);
                }
                ServerFrame::Message { mailbox, seq, data } => {
                    let (notice, follow_up) = self.receive(&relay, &mailbox, &data)?;
                    if let Some(l) = live.get_mut(&relay) {
                        // A lost acknowledgement only means the message comes again.
                        let _ = l.connection.ack(&mailbox, seq);
                    }
                    self.show(&notice);
                    notices.push(notice);
                    if let Some(notice) = self.follow_up(follow_up).await? {
                        self.show(&notice);
                        notices.push(notice);
                    }
                }
                ServerFrame::Closed => {
                    live.remove(&relay);
                    if listen {
                        self.say(&format!(
                            "Verbindung zu {relay} getrennt; neuer Versuch in Kürze."
                        ));
                    } else if live.is_empty() {
                        break;
                    }
                }
            }
        }
        self.clean_invitations().await?;
        if !listen && notices.iter().all(Notice::is_dropped) {
            self.say("Keine neuen Nachrichten.");
        }
        Ok(notices)
    }

    fn show(&mut self, notice: &Notice) {
        if self.verbose || !notice.is_dropped() {
            let text = notice.text(self.now());
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
        let device = self.store.load()?;
        for (relay, mailboxes) in own_mailboxes(&device) {
            let l = match live.entry(relay) {
                Entry::Occupied(slot) => slot.into_mut(),
                Entry::Vacant(slot) => match self.relay.connect(&relay, deliveries.clone()).await {
                    Ok(connection) => slot.insert(Live {
                        connection,
                        subscribed: HashSet::new(),
                        awaiting: HashSet::new(),
                    }),
                    Err(error) => {
                        let notice = Notice::Problem(format!("{relay}: {error}"));
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
    ) -> Result<(Notice, FollowUp)> {
        let now = self.now();
        self.store.update(|device| {
            if let Some(contact) = device
                .contacts
                .iter_mut()
                .find(|c| c.own_relay == *relay && c.state.contact.own_mailbox == *mailbox)
            {
                return Ok(receive_for_contact(contact, data, now));
            }
            let Some(index) = device
                .invitations
                .iter()
                .position(|i| i.pending.own.relay == *relay && i.pending.own.mailbox == *mailbox)
            else {
                return Ok((
                    Notice::Dropped {
                        name: None,
                        why: "unbekanntes Postfach".into(),
                    },
                    FollowUp::None,
                ));
            };
            let invitation = &device.invitations[index];
            let name = invitation.name.clone();
            match invitation.pending.receive_hello(data, now) {
                Ok(contact) => {
                    let invitation = device.invitations.remove(index);
                    let code = contact.verification_code();
                    device.contacts.push(StoredContact {
                        name: name.clone(),
                        own_relay: invitation.pending.own.relay,
                        read_token: invitation.read_token,
                        state: ContactState::new(contact),
                    });
                    Ok::<_, Fail>((Notice::Accepted { name, code }, FollowUp::None))
                }
                Err(error) => Ok((
                    Notice::Dropped {
                        name: Some(name),
                        why: error.to_string(),
                    },
                    FollowUp::None,
                )),
            }
        })
    }

    /// Network work after a message was handled. Returns a notice if the
    /// contact turned out to be gone.
    async fn follow_up(&mut self, follow_up: FollowUp) -> Result<Option<Notice>> {
        match follow_up {
            FollowUp::None => Ok(None),
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
            } => match self
                .relay
                .deliver(&peer.relay, &peer.mailbox, &peer.send_token, &envelope[..])
                .await
            {
                Ok(()) => Ok(None),
                Err(RelayError::UnknownMailbox) => {
                    self.gone(&name).await?;
                    Ok(Some(Notice::Ended { name }))
                }
                Err(error) => Ok(Some(Notice::Problem(format!(
                    "Regeln an {name} nicht zugestellt: {error}"
                )))),
            },
        }
    }

    /// Withdraws expired invitations and deletes their mailboxes.
    async fn clean_invitations(&mut self) -> Result<()> {
        let now = self.now();
        let expired = self.store.update(|device| {
            let (expired, open) = std::mem::take(&mut device.invitations)
                .into_iter()
                .partition::<Vec<_>, _>(|i| i.pending.keep_until() < now);
            device.invitations = open;
            Ok::<_, Fail>(expired)
        })?;
        for invitation in expired {
            let own = &invitation.pending.own;
            self.delete_own(&own.relay, &own.mailbox, &invitation.read_token)
                .await;
            self.say(&format!(
                "Einladung für {} ist abgelaufen und wurde entfernt.",
                invitation.name
            ));
        }
        Ok(())
    }
}

fn receive_for_contact(contact: &mut StoredContact, data: &[u8], now: u64) -> (Notice, FollowUp) {
    let name = contact.name.clone();
    let event = match contact.state.receive(data, now) {
        Ok(event) => event,
        Err(error) => {
            return (
                Notice::Dropped {
                    name: Some(name),
                    why: error.to_string(),
                },
                FollowUp::None,
            );
        }
    };
    match event {
        Event::Paired => match contact.state.announce_rules(now, &mut OsRng) {
            Ok(envelope) => (
                Notice::Paired { name: name.clone() },
                FollowUp::Send {
                    peer: contact.state.contact.peer.clone(),
                    name,
                    envelope: Box::new(envelope),
                },
            ),
            Err(error) => (
                Notice::Problem(format!("Regeln für {name} nicht verschlüsselt: {error}")),
                FollowUp::None,
            ),
        },
        Event::Rules(rules) => (Notice::Rules { name, rules }, FollowUp::None),
        Event::Request(_) => (Notice::Request { name }, FollowUp::None),
        Event::Response { kind, at, .. } => (Notice::Response { name, kind, at }, FollowUp::None),
        Event::Ended => (
            Notice::Ended { name },
            FollowUp::DeleteOwn {
                relay: contact.own_relay,
                mailbox: contact.state.contact.own_mailbox,
                token: contact.read_token,
            },
        ),
        Event::Rejected => (
            Notice::Rejected { name },
            FollowUp::DeleteOwn {
                relay: contact.own_relay,
                mailbox: contact.state.contact.own_mailbox,
                token: contact.read_token,
            },
        ),
        Event::SecondAcceptance => (
            Notice::SecondAcceptance {
                name,
                paired: contact.state.is_paired(),
            },
            FollowUp::None,
        ),
        Event::Ignored(why) => (
            Notice::Dropped {
                name: Some(name),
                why: format!("{why:?}"),
            },
            FollowUp::None,
        ),
    }
}
