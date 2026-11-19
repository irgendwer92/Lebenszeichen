//! Relays in memory, following the documented semantics, for tests.
//!
//! Time comes from a manual clock shared with the tests: vouchers and
//! handover slots expire after 10 minutes, messages after 24 hours.
//! Not covered: the rate limit per hour, the limit of 16 readers, the
//! 90-day rule and wake signals. Push addresses are only stored and roughly
//! checked.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lz_core::wire::{Hash, MailboxId, RelayUrl, Token};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use super::{Connection, Delivery, ReaderId, Relay, RelayError, ServerFrame, token_hash};

/// Most messages waiting in one mailbox.
const MAX_WAITING: usize = 32;
/// Lifetime of vouchers and handover slots.
const SLOT_TTL: u64 = 10 * 60;
/// Lifetime of a message.
const MESSAGE_TTL: u64 = 24 * 60 * 60;
/// Most mailboxes derived from one voucher mailbox.
const MAX_DERIVED: usize = 10;
/// Most handover slots on one relay, reserved reveal slots included.
const MAX_SLOTS: usize = 64;
/// Most handover slots waiting per authorising token.
const MAX_SLOTS_PER_TOKEN: usize = 2;
/// Largest handover slot.
const MAX_SLOT_LEN: usize = 64 * 1024;

/// Any number of fake relays, reachable by their base address.
#[derive(Clone, Default)]
pub struct FakeRelay {
    servers: Arc<Mutex<HashMap<String, Server>>>,
    next_connection: Arc<AtomicU64>,
    clock: Arc<AtomicU64>,
}

#[derive(Default)]
struct Server {
    /// Unreachable, as if the network were down.
    down: bool,
    /// The next this many `deliver` calls store the message but answer
    /// with a transport error, as if the answer got lost.
    lose_answers: u32,
    access_token: Token,
    /// Hashes of open vouchers with their expiry.
    vouchers: HashMap<String, u64>,
    mailboxes: HashMap<MailboxId, Mailbox>,
    slots: HashMap<Hash, Slot>,
    /// Everything ever stored in a slot, as an operator could keep it.
    history: HashMap<Hash, Vec<u8>>,
    subscriptions: Vec<Subscription>,
    /// Successful `put_reader` calls, for tests that expect none.
    reader_puts: u64,
}

/// How a mailbox was created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    AccessToken,
    Voucher,
    /// Derived from the read token of this voucher mailbox.
    Derived(MailboxId),
}

struct Mailbox {
    read_hash: String,
    send_hash: String,
    origin: Origin,
    /// Registered readers with their last acknowledged sequence number.
    readers: HashMap<ReaderId, u64>,
    /// Push address of each reader that has one.
    push_urls: HashMap<ReaderId, String>,
    /// Sequence number, arrival time, envelope.
    messages: Vec<(u64, u64, Vec<u8>)>,
    last_seq: u64,
}

/// A handover slot, or a reserved reveal slot.
struct Slot {
    /// `None` for a reveal slot not filled yet.
    data: Option<Vec<u8>>,
    /// Hash of the fetch token, for a reveal slot of the reply token.
    fetch_hash: String,
    /// Hash of the authorising token; `None` for reveal slots.
    auth_hash: Option<String>,
    expires_at: u64,
    reveal: bool,
}

struct Subscription {
    connection: u64,
    mailbox: MailboxId,
    reader: ReaderId,
    relay: RelayUrl,
    deliveries: mpsc::UnboundedSender<Delivery>,
}

fn refused(status: u16, code: &str) -> RelayError {
    RelayError::Refused {
        status,
        code: code.into(),
    }
}

/// `SHA-256("lebenszeichen-v1-reveal" || slot)`.
fn reveal_of(slot: &Hash) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update(b"lebenszeichen-v1-reveal");
    hasher.update(slot);
    hasher.finalize().into()
}

impl FakeRelay {
    /// Relays that read the time from `clock`.
    pub fn new(clock: Arc<AtomicU64>) -> Self {
        Self {
            clock,
            ..Self::default()
        }
    }

    /// Starts a relay at `url` that accepts `access_token`.
    pub fn add(&self, url: &str, access_token: Token) {
        self.servers.lock().unwrap().insert(
            url.into(),
            Server {
                access_token,
                ..Server::default()
            },
        );
    }

    /// Makes the relay unreachable or reachable again.
    pub fn set_down(&self, url: &str, down: bool) {
        self.servers.lock().unwrap().get_mut(url).unwrap().down = down;
    }

    /// Replaces the access token of the relay, as its operator would.
    pub fn set_access_token(&self, url: &str, token: Token) {
        self.servers
            .lock()
            .unwrap()
            .get_mut(url)
            .unwrap()
            .access_token = token;
    }

    /// Makes the next `count` `deliver` calls on the relay store their
    /// message, but fail towards the caller, as if the answer got lost.
    pub fn lose_answers(&self, url: &str, count: u32) {
        self.servers
            .lock()
            .unwrap()
            .get_mut(url)
            .unwrap()
            .lose_answers = count;
    }

    /// Whether the mailbox exists.
    pub fn has_mailbox(&self, url: &str, mailbox: &MailboxId) -> bool {
        self.servers.lock().unwrap()[url]
            .mailboxes
            .contains_key(mailbox)
    }

    /// How the mailbox was created.
    pub fn origin(&self, url: &str, mailbox: &MailboxId) -> Origin {
        self.servers.lock().unwrap()[url].mailboxes[mailbox].origin
    }

    /// Whether the reader is registered on the mailbox.
    pub fn has_reader(&self, url: &str, mailbox: &MailboxId, reader: &ReaderId) -> bool {
        self.servers.lock().unwrap()[url]
            .mailboxes
            .get(mailbox)
            .is_some_and(|m| m.readers.contains_key(reader))
    }

    /// Messages waiting in the mailbox, not yet acknowledged by every reader.
    pub fn waiting(&self, url: &str, mailbox: &MailboxId) -> usize {
        let now = self.now();
        let mut servers = self.servers.lock().unwrap();
        let mailbox = servers
            .get_mut(url)
            .unwrap()
            .mailboxes
            .get_mut(mailbox)
            .unwrap();
        mailbox.expire(now);
        mailbox.messages.len()
    }

    /// The push address the reader has for the mailbox.
    pub fn push_url(&self, url: &str, mailbox: &MailboxId, reader: &ReaderId) -> Option<String> {
        self.servers.lock().unwrap()[url].mailboxes[mailbox]
            .push_urls
            .get(reader)
            .cloned()
    }

    /// How often `put_reader` succeeded on the relay.
    pub fn reader_puts(&self, url: &str) -> u64 {
        self.servers.lock().unwrap()[url].reader_puts
    }

    /// What was last stored in a slot, even if fetched since, as the
    /// operator of the relay can see it.
    pub fn seen_slot(&self, url: &str, slot: &Hash) -> Option<Vec<u8>> {
        self.servers.lock().unwrap()[url].history.get(slot).cloned()
    }

    /// Whether a slot is stored or reserved.
    pub fn has_slot(&self, url: &str, slot: &Hash) -> bool {
        let now = self.now();
        let mut servers = self.servers.lock().unwrap();
        let server = servers.get_mut(url).unwrap();
        server.purge_slots(now);
        server.slots.contains_key(slot)
    }

    fn now(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }

    fn with<R>(
        &self,
        relay: &RelayUrl,
        f: impl FnOnce(&mut Server, u64) -> Result<R, RelayError>,
    ) -> Result<R, RelayError> {
        let now = self.now();
        let mut servers = self.servers.lock().unwrap();
        let server = servers
            .get_mut(relay.as_str())
            .filter(|server| !server.down)
            .ok_or_else(|| RelayError::Unreachable(format!("{relay} is down")))?;
        server.purge_slots(now);
        for mailbox in server.mailboxes.values_mut() {
            mailbox.expire(now);
        }
        f(server, now)
    }
}

impl Server {
    fn mailbox(
        &mut self,
        id: &MailboxId,
        token: &Token,
        read: bool,
    ) -> Result<&mut Mailbox, RelayError> {
        let mailbox = self
            .mailboxes
            .get_mut(id)
            .ok_or(RelayError::UnknownMailbox)?;
        let expected = if read {
            &mailbox.read_hash
        } else {
            &mailbox.send_hash
        };
        if *expected != token_hash(token) {
            return Err(refused(401, "unauthorized"));
        }
        Ok(mailbox)
    }

    fn purge_slots(&mut self, now: u64) {
        self.slots.retain(|_, slot| now < slot.expires_at);
        self.vouchers.retain(|_, expires_at| now < *expires_at);
    }

    /// Ends the subscriptions of `reader` (or all, with `None`) on the
    /// mailbox and tells them why.
    fn end_subscriptions(&mut self, mailbox: &MailboxId, reader: Option<&ReaderId>, code: &str) {
        self.subscriptions.retain(|s| {
            let hit = s.mailbox == *mailbox && reader.is_none_or(|r| *r == s.reader);
            if hit {
                let _ = s.deliveries.send(Delivery {
                    relay: s.relay,
                    frame: ServerFrame::Error {
                        mailbox: Some(*mailbox),
                        code: code.into(),
                    },
                });
            }
            !hit
        });
    }
}

impl Mailbox {
    /// Drops messages that every reader has acknowledged.
    fn clean(&mut self) {
        if let Some(&min) = self.readers.values().min() {
            self.messages.retain(|(seq, _, _)| *seq > min);
        }
    }

    fn expire(&mut self, now: u64) {
        self.messages
            .retain(|(_, at, _)| now < at.saturating_add(MESSAGE_TTL));
    }
}

impl Relay for FakeRelay {
    async fn create_voucher(
        &self,
        relay: &RelayUrl,
        access_token: &Token,
    ) -> Result<Token, RelayError> {
        self.with(relay, |server, now| {
            if *access_token != server.access_token {
                return Err(refused(401, "unauthorized"));
            }
            let mut voucher = [0; 32];
            OsRng.fill_bytes(&mut voucher);
            server.vouchers.insert(token_hash(&voucher), now + SLOT_TTL);
            Ok(voucher)
        })
    }

    async fn create_mailbox(
        &self,
        relay: &RelayUrl,
        authorisation: &Token,
        read_token: &Token,
        send_token: &Token,
    ) -> Result<MailboxId, RelayError> {
        self.with(relay, |server, _| {
            let hash = token_hash(authorisation);
            let origin = if *authorisation == server.access_token {
                Origin::AccessToken
            } else if server.vouchers.remove(&hash).is_some() {
                Origin::Voucher
            } else {
                let parent = server
                    .mailboxes
                    .iter()
                    .find(|(_, m)| m.read_hash == hash && m.origin == Origin::Voucher)
                    .map(|(id, _)| *id)
                    .ok_or_else(|| refused(401, "unauthorized"))?;
                let derived = server
                    .mailboxes
                    .values()
                    .filter(|m| m.origin == Origin::Derived(parent))
                    .count();
                if derived >= MAX_DERIVED {
                    return Err(refused(429, "mailbox_quota"));
                }
                Origin::Derived(parent)
            };
            let mut id = [0; 32];
            OsRng.fill_bytes(&mut id);
            server.mailboxes.insert(
                id,
                Mailbox {
                    read_hash: token_hash(read_token),
                    send_hash: token_hash(send_token),
                    origin,
                    readers: HashMap::new(),
                    push_urls: HashMap::new(),
                    messages: Vec::new(),
                    last_seq: 0,
                },
            );
            Ok(id)
        })
    }

    async fn delete_mailbox(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        read_token: &Token,
    ) -> Result<(), RelayError> {
        self.with(relay, |server, _| {
            server.mailbox(mailbox, read_token, true)?;
            server.mailboxes.remove(mailbox);
            server.end_subscriptions(mailbox, None, "unknown_mailbox");
            Ok(())
        })
    }

    async fn put_reader(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        read_token: &Token,
        reader: &ReaderId,
        push_url: Option<&str>,
    ) -> Result<(), RelayError> {
        self.with(relay, |server, _| {
            // The real relay also refuses internal addresses.
            if push_url.is_some_and(|url| {
                !url.starts_with("https://") || url.starts_with("https://localhost")
            }) {
                return Err(refused(400, "bad_request"));
            }
            let mailbox = server.mailbox(mailbox, read_token, true)?;
            // A new reader starts at 0 and receives all waiting messages.
            mailbox.readers.entry(*reader).or_insert(0);
            match push_url {
                Some(url) => mailbox.push_urls.insert(*reader, url.to_owned()),
                None => mailbox.push_urls.remove(reader),
            };
            server.reader_puts += 1;
            Ok(())
        })
    }

    async fn delete_reader(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        read_token: &Token,
        reader: &ReaderId,
    ) -> Result<(), RelayError> {
        self.with(relay, |server, _| {
            let entry = server.mailbox(mailbox, read_token, true)?;
            if entry.readers.remove(reader).is_none() {
                return Err(refused(404, "unknown_reader"));
            }
            entry.push_urls.remove(reader);
            entry.clean();
            server.end_subscriptions(mailbox, Some(reader), "unknown_reader");
            Ok(())
        })
    }

    async fn put_slot(
        &self,
        relay: &RelayUrl,
        slot: &Hash,
        authorisation: &Token,
        fetch_hash: &Hash,
        reply_hash: Option<&Hash>,
        data: &[u8],
    ) -> Result<(), RelayError> {
        self.with(relay, |server, now| {
            let auth_hash = token_hash(authorisation);
            if *authorisation != server.access_token
                && !server.mailboxes.values().any(|m| m.read_hash == auth_hash)
            {
                return Err(refused(401, "unauthorized"));
            }
            if data.len() > MAX_SLOT_LEN {
                return Err(refused(413, "too_large"));
            }
            if data.is_empty() {
                return Err(refused(400, "bad_request"));
            }
            let reveal = reply_hash.map(|hash| (reveal_of(slot), hash));
            if server.slots.contains_key(slot)
                || reveal
                    .as_ref()
                    .is_some_and(|(id, _)| server.slots.contains_key(id))
            {
                return Err(RelayError::HandoverExists);
            }
            let per_token = server
                .slots
                .values()
                .filter(|s| s.auth_hash.as_ref() == Some(&auth_hash))
                .count();
            if per_token >= MAX_SLOTS_PER_TOKEN {
                return Err(refused(429, "handovers_quota"));
            }
            let needed = 1 + usize::from(reveal.is_some());
            if server.slots.len() + needed > MAX_SLOTS {
                return Err(refused(503, "handovers_full"));
            }
            let expires_at = now + SLOT_TTL;
            server.history.insert(*slot, data.to_vec());
            server.slots.insert(
                *slot,
                Slot {
                    data: Some(data.to_vec()),
                    fetch_hash: super::hex(fetch_hash),
                    auth_hash: Some(auth_hash),
                    expires_at,
                    reveal: false,
                },
            );
            if let Some((id, reply_hash)) = reveal {
                server.slots.insert(
                    id,
                    Slot {
                        data: None,
                        fetch_hash: super::hex(reply_hash),
                        auth_hash: None,
                        expires_at,
                        reveal: true,
                    },
                );
            }
            Ok(())
        })
    }

    async fn put_reveal(
        &self,
        relay: &RelayUrl,
        slot: &Hash,
        reply_token: &Token,
        value: &[u8; 32],
    ) -> Result<(), RelayError> {
        self.with(relay, |server, _| {
            let entry = server
                .slots
                .get_mut(slot)
                .ok_or(RelayError::UnknownHandover)?;
            if entry.data.is_some() || !entry.reveal {
                return Err(RelayError::HandoverExists);
            }
            if entry.fetch_hash != token_hash(reply_token) {
                return Err(refused(401, "unauthorized"));
            }
            entry.data = Some(value.to_vec());
            server.history.insert(*slot, value.to_vec());
            Ok(())
        })
    }

    async fn get_slot(
        &self,
        relay: &RelayUrl,
        slot: &Hash,
        token: &Token,
    ) -> Result<Vec<u8>, RelayError> {
        self.with(relay, |server, _| {
            let entry = server
                .slots
                .get(slot)
                .filter(|s| s.data.is_some())
                .ok_or(RelayError::UnknownHandover)?;
            if entry.fetch_hash != token_hash(token) {
                return Err(refused(401, "unauthorized"));
            }
            let data = entry.data.clone().expect("filtered");
            server.slots.remove(slot);
            Ok(data)
        })
    }

    async fn deliver(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        send_token: &Token,
        envelope: &[u8],
    ) -> Result<(), RelayError> {
        self.with(relay, |server, now| {
            let id = *mailbox;
            let mailbox = server.mailbox(&id, send_token, false)?;
            if envelope.len() > lz_core::wire::MAX_MESSAGE_BYTES {
                return Err(refused(413, "too_large"));
            }
            if mailbox.messages.len() >= MAX_WAITING {
                return Err(refused(429, "mailbox_full"));
            }
            mailbox.last_seq += 1;
            let seq = mailbox.last_seq;
            mailbox.messages.push((seq, now, envelope.to_vec()));
            for subscription in server.subscriptions.iter().filter(|s| s.mailbox == id) {
                let _ = subscription.deliveries.send(Delivery {
                    relay: subscription.relay,
                    frame: ServerFrame::Message {
                        mailbox: id,
                        seq,
                        data: envelope.to_vec(),
                    },
                });
            }
            if server.lose_answers > 0 {
                server.lose_answers -= 1;
                return Err(RelayError::Transport("answer lost".into()));
            }
            Ok(())
        })
    }

    async fn connect(
        &self,
        relay: &RelayUrl,
        deliveries: mpsc::UnboundedSender<Delivery>,
    ) -> Result<Box<dyn Connection>, RelayError> {
        self.with(relay, |_, _| Ok(()))?;
        Ok(Box::new(FakeConnection {
            relay: self.clone(),
            url: *relay,
            id: self.next_connection.fetch_add(1, Ordering::Relaxed),
            deliveries,
        }))
    }
}

struct FakeConnection {
    relay: FakeRelay,
    url: RelayUrl,
    id: u64,
    deliveries: mpsc::UnboundedSender<Delivery>,
}

impl FakeConnection {
    fn send(&self, frame: ServerFrame) {
        let _ = self.deliveries.send(Delivery {
            relay: self.url,
            frame,
        });
    }
}

impl Connection for FakeConnection {
    fn subscribe(
        &mut self,
        mailbox: &MailboxId,
        read_token: &Token,
        reader: &ReaderId,
    ) -> Result<(), RelayError> {
        let id = *mailbox;
        let result = self.relay.with(&self.url, |server, _| {
            let mailbox = server.mailbox(&id, read_token, true)?;
            let Some(&acked) = mailbox.readers.get(reader) else {
                return Err(refused(404, "unknown_reader"));
            };
            let waiting: Vec<_> = mailbox
                .messages
                .iter()
                .filter(|(seq, _, _)| *seq > acked)
                .map(|(seq, _, data)| (*seq, data.clone()))
                .collect();
            server.subscriptions.push(Subscription {
                connection: self.id,
                mailbox: id,
                reader: *reader,
                relay: self.url,
                deliveries: self.deliveries.clone(),
            });
            Ok(waiting)
        });
        match result {
            Ok(waiting) => {
                self.send(ServerFrame::Subscribed { mailbox: id });
                for (seq, data) in waiting {
                    self.send(ServerFrame::Message {
                        mailbox: id,
                        seq,
                        data,
                    });
                }
            }
            Err(error) => {
                let code = match error {
                    RelayError::UnknownMailbox => "unknown_mailbox".into(),
                    RelayError::Refused { code, .. } => code,
                    other => other.to_string(),
                };
                self.send(ServerFrame::Error {
                    mailbox: Some(id),
                    code,
                });
            }
        }
        Ok(())
    }

    fn ack(&mut self, mailbox: &MailboxId, seq: u64) -> Result<(), RelayError> {
        let connection = self.id;
        self.relay.with(&self.url, |server, _| {
            let readers: Vec<ReaderId> = server
                .subscriptions
                .iter()
                .filter(|s| s.connection == connection && s.mailbox == *mailbox)
                .map(|s| s.reader)
                .collect();
            if let Some(mailbox) = server.mailboxes.get_mut(mailbox) {
                for reader in readers {
                    if let Some(acked) = mailbox.readers.get_mut(&reader) {
                        *acked = (*acked).max(seq);
                    }
                }
                mailbox.clean();
            }
            Ok(())
        })
    }
}

impl Drop for FakeConnection {
    fn drop(&mut self) {
        let id = self.id;
        let _ = self.relay.with(&self.url, |server, _| {
            server.subscriptions.retain(|s| s.connection != id);
            Ok(())
        });
    }
}
