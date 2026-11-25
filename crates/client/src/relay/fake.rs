//! Relays in memory, following the documented semantics, for tests.
//!
//! Time comes from a manual clock shared with the tests: vouchers and
//! handover slots expire after 10 minutes, messages after 24 hours.
//! Not covered: the rate limit per hour, the 90-day rule and wake signals.
//! Push addresses are only stored and roughly checked.
//!
//! A relay offers `reader_tokens` (M10) unless [`FakeRelay::set_old`] makes
//! it behave as a relay before M10: no features, no own tokens (unknown
//! fields ignored), no new calls, a missing push address removes it, and
//! `subscribe` checks the token before the reader. A handle from
//! [`FakeRelay::old_client`] reaches the same relays but is told that none
//! offers `reader_tokens`, so that a client on it behaves as one before M10.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lz_core::wire::{Hash, MailboxId, RelayUrl, Token};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use super::{
    Connection, Delivery, Features, MailboxRead, ReaderChange, ReaderId, Relay, RelayError, Role,
    ServerFrame, token_hash,
};

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
/// Most readers of one mailbox.
const MAX_READERS: usize = 16;

/// Any number of fake relays, reachable by their base address.
#[derive(Clone, Default)]
pub struct FakeRelay {
    servers: Arc<Mutex<HashMap<String, Server>>>,
    next_connection: Arc<AtomicU64>,
    clock: Arc<AtomicU64>,
    /// This handle is told that no relay offers features, as a client
    /// before M10 would not ask.
    old_client: bool,
}

#[derive(Default)]
struct Server {
    /// Unreachable, as if the network were down.
    down: bool,
    /// Behaves as a relay before M10.
    old: bool,
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
    /// Calls of `features`.
    health_calls: u64,
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
    /// Hash of the shared token; `None` once dropped, or for a mailbox of
    /// the new form.
    shared_hash: Option<String>,
    send_hash: String,
    origin: Origin,
    readers: HashMap<ReaderId, Reader>,
    /// Sequence number, arrival time, envelope.
    messages: Vec<(u64, u64, Vec<u8>)>,
    last_seq: u64,
}

/// A registered reader.
struct Reader {
    /// Last acknowledged sequence number.
    acked: u64,
    push_url: Option<String>,
    /// Hash of its own token; `None` for a legacy reader.
    token_hash: Option<String>,
    role: Role,
}

/// Who a token belongs to on one mailbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Principal {
    Shared,
    Reader(ReaderId, Role),
}

impl Principal {
    /// May register, change and revoke other readers and delete the
    /// mailbox.
    fn manages(self) -> bool {
        !matches!(self, Self::Reader(_, Role::Satellite))
    }

    fn is_full_reader(self) -> bool {
        matches!(self, Self::Reader(_, Role::Full))
    }
}

/// Outcome of the checks of a subscription, as the relay reports them.
enum ReadCheck {
    Ok(u64),
    UnknownMailbox,
    UnknownReader,
    Unauthorized,
}

impl ReadCheck {
    fn code(&self) -> &'static str {
        match self {
            Self::Ok(_) => "",
            Self::UnknownMailbox => "unknown_mailbox",
            Self::UnknownReader => "unknown_reader",
            Self::Unauthorized => "unauthorized",
        }
    }
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
    /// Hash of the token it was opened with.
    token_hash: String,
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

    /// Makes the relay behave as one before M10, or as an updated one
    /// again. Stored own tokens stay, as after a downgrade, but are not
    /// used.
    pub fn set_old(&self, url: &str, old: bool) {
        self.servers.lock().unwrap().get_mut(url).unwrap().old = old;
    }

    /// A handle to the same relays for a client before M10: it is told
    /// that no relay offers `reader_tokens`.
    pub fn old_client(&self) -> Self {
        Self {
            old_client: true,
            ..self.clone()
        }
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

    /// The readers registered on the mailbox.
    pub fn readers(&self, url: &str, mailbox: &MailboxId) -> Vec<ReaderId> {
        self.servers.lock().unwrap()[url].mailboxes[mailbox]
            .readers
            .keys()
            .copied()
            .collect()
    }

    /// Whether the reader has a token of its own on the mailbox.
    pub fn has_own_token(&self, url: &str, mailbox: &MailboxId, reader: &ReaderId) -> bool {
        self.servers.lock().unwrap()[url].mailboxes[mailbox]
            .readers
            .get(reader)
            .is_some_and(|r| r.token_hash.is_some())
    }

    /// Whether the mailbox still has a shared token.
    pub fn has_shared_token(&self, url: &str, mailbox: &MailboxId) -> bool {
        self.servers.lock().unwrap()[url].mailboxes[mailbox]
            .shared_hash
            .is_some()
    }

    /// Whether the reader has a live subscription of the mailbox.
    pub fn subscribed(&self, url: &str, mailbox: &MailboxId, reader: &ReaderId) -> bool {
        self.servers.lock().unwrap()[url]
            .subscriptions
            .iter()
            .any(|s| s.mailbox == *mailbox && s.reader == *reader)
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
            .readers
            .get(reader)
            .and_then(|r| r.push_url.clone())
    }

    /// How often `put_reader` succeeded on the relay.
    pub fn reader_puts(&self, url: &str) -> u64 {
        self.servers.lock().unwrap()[url].reader_puts
    }

    /// How often the relay was asked for its features.
    pub fn health_calls(&self, url: &str) -> u64 {
        self.servers.lock().unwrap()[url].health_calls
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
    /// The mailbox and who the token belongs to there. An unknown mailbox
    /// comes before a wrong token.
    fn principal(
        &mut self,
        id: &MailboxId,
        token: &Token,
    ) -> Result<(&mut Mailbox, Principal), RelayError> {
        let old = self.old;
        let mailbox = self
            .mailboxes
            .get_mut(id)
            .ok_or(RelayError::UnknownMailbox)?;
        let principal = mailbox
            .principal(&token_hash(token), old)
            .ok_or_else(|| refused(401, "unauthorized"))?;
        Ok((mailbox, principal))
    }

    /// The mailbox, if `token` is its send token.
    fn for_sending(&mut self, id: &MailboxId, token: &Token) -> Result<&mut Mailbox, RelayError> {
        let mailbox = self
            .mailboxes
            .get_mut(id)
            .ok_or(RelayError::UnknownMailbox)?;
        if mailbox.send_hash != token_hash(token) {
            return Err(refused(401, "unauthorized"));
        }
        Ok(mailbox)
    }

    /// Fails as an unknown call does on a relay before M10.
    fn new_call(&self) -> Result<(), RelayError> {
        match self.old {
            true => Err(refused(404, "")),
            false => Ok(()),
        }
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
                s.end(code);
            }
            !hit
        });
    }

    /// Checks every live subscription of the mailbox again, as the relay
    /// does when tokens change, and ends those no longer valid.
    fn recheck(&mut self, id: &MailboxId) {
        let old = self.old;
        let mailbox = self.mailboxes.get(id);
        self.subscriptions.retain(|s| {
            if s.mailbox != *id {
                return true;
            }
            let check = match mailbox {
                None => ReadCheck::UnknownMailbox,
                Some(m) => m.check(&s.reader, &s.token_hash, old),
            };
            match check {
                ReadCheck::Ok(_) => true,
                denied => {
                    s.end(denied.code());
                    false
                }
            }
        });
    }
}

impl Subscription {
    fn end(&self, code: &str) {
        let _ = self.deliveries.send(Delivery {
            relay: self.relay,
            frame: ServerFrame::Error {
                mailbox: Some(self.mailbox),
                code: code.into(),
            },
        });
    }
}

impl Mailbox {
    /// Who the token with this hash belongs to: a reader with this own
    /// token, else the shared token. A relay before M10 knows only the
    /// shared token.
    fn principal(&self, hash: &str, old: bool) -> Option<Principal> {
        let own = self
            .readers
            .iter()
            .filter(|_| !old)
            .find(|(_, r)| r.token_hash.as_deref() == Some(hash))
            .map(|(id, r)| Principal::Reader(*id, r.role));
        own.or_else(|| (self.shared_hash.as_deref() == Some(hash)).then_some(Principal::Shared))
    }

    /// Whether `reader` may read with the token with this hash: a reader
    /// with a token of its own only with it, a legacy one only with the
    /// shared token. Checked in the order mailbox, reader, token; a relay
    /// before M10 checks the token before the reader.
    fn check(&self, reader: &ReaderId, hash: &str, old: bool) -> ReadCheck {
        let shared = self.shared_hash.as_deref() == Some(hash);
        if old {
            return match (shared, self.readers.get(reader)) {
                (false, _) => ReadCheck::Unauthorized,
                (true, None) => ReadCheck::UnknownReader,
                (true, Some(r)) => ReadCheck::Ok(r.acked),
            };
        }
        let Some(entry) = self.readers.get(reader) else {
            return ReadCheck::UnknownReader;
        };
        let matches = match &entry.token_hash {
            Some(own) => own == hash,
            None => shared,
        };
        match matches {
            true => ReadCheck::Ok(entry.acked),
            false => ReadCheck::Unauthorized,
        }
    }

    /// Whether the shared token or a reader other than `reader` has this
    /// token hash already.
    fn hash_taken(&self, hash: &str, reader: &ReaderId) -> bool {
        self.shared_hash.as_deref() == Some(hash)
            || self
                .readers
                .iter()
                .any(|(id, r)| id != reader && r.token_hash.as_deref() == Some(hash))
    }

    /// Drops messages that every reader has acknowledged.
    fn clean(&mut self) {
        if let Some(min) = self.readers.values().map(|r| r.acked).min() {
            self.messages.retain(|(seq, _, _)| *seq > min);
        }
    }

    fn expire(&mut self, now: u64) {
        self.messages
            .retain(|(_, at, _)| now < at.saturating_add(MESSAGE_TTL));
    }
}

impl Relay for FakeRelay {
    async fn features(&self, relay: &RelayUrl) -> Result<Features, RelayError> {
        let old_client = self.old_client;
        self.with(relay, |server, _| {
            server.health_calls += 1;
            Ok(Features {
                reader_tokens: !server.old && !old_client,
            })
        })
    }

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
        read: MailboxRead<'_>,
        send_token: &Token,
    ) -> Result<MailboxId, RelayError> {
        self.with(relay, |server, _| {
            // A relay before M10 cannot read the new form.
            if server.old && matches!(read, MailboxRead::Reader(..)) {
                return Err(refused(400, "bad_request"));
            }
            let hash = token_hash(authorisation);
            let old = server.old;
            let origin = if *authorisation == server.access_token {
                Origin::AccessToken
            } else if server.vouchers.remove(&hash).is_some() {
                Origin::Voucher
            } else {
                let parent = server
                    .mailboxes
                    .iter()
                    .find(|(_, m)| {
                        m.origin == Origin::Voucher
                            && m.principal(&hash, old).is_some_and(Principal::manages)
                    })
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
            let (shared_hash, readers) = match read {
                MailboxRead::Shared(token) => (Some(token_hash(token)), HashMap::new()),
                MailboxRead::Reader(reader, token) => {
                    let first = Reader {
                        acked: 0,
                        push_url: None,
                        token_hash: Some(token_hash(token)),
                        role: Role::Full,
                    };
                    (None, HashMap::from([(*reader, first)]))
                }
            };
            server.mailboxes.insert(
                id,
                Mailbox {
                    shared_hash,
                    send_hash: token_hash(send_token),
                    origin,
                    readers,
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
            let (_, principal) = server.principal(mailbox, read_token)?;
            if !principal.manages() {
                return Err(refused(401, "unauthorized"));
            }
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
        change: ReaderChange<'_>,
    ) -> Result<(), RelayError> {
        self.with(relay, |server, _| {
            let id = *mailbox;
            let old = server.old;
            let (entry, principal) = server.principal(&id, read_token)?;
            // A relay before M10 ignores what it does not know, and a
            // missing push address means none.
            let change = match old {
                true => ReaderChange::push(change.push_url.flatten()),
                false => change,
            };
            // The real relay also refuses internal addresses.
            if change.push_url.flatten().is_some_and(|url| {
                !url.starts_with("https://") || url.starts_with("https://localhost")
            }) {
                return Err(refused(400, "bad_request"));
            }
            if let Principal::Reader(own, Role::Satellite) = principal
                && (own != *reader || change.role.is_some())
            {
                return Err(refused(401, "unauthorized"));
            }
            let new_hash = change.token.map(token_hash);
            if let Some(hash) = &new_hash
                && entry.hash_taken(hash, reader)
            {
                return Err(refused(400, "bad_request"));
            }
            let token_changed = match entry.readers.get_mut(reader) {
                Some(existing) => {
                    if let Some(push_url) = change.push_url {
                        existing.push_url = push_url.map(str::to_owned);
                    }
                    if let Some(role) = change.role {
                        existing.role = role;
                    }
                    match new_hash {
                        Some(hash) if existing.token_hash.as_ref() != Some(&hash) => {
                            existing.token_hash = Some(hash);
                            true
                        }
                        _ => false,
                    }
                }
                None => {
                    if entry.shared_hash.is_none() && new_hash.is_none() {
                        return Err(refused(400, "bad_request"));
                    }
                    if entry.readers.len() >= MAX_READERS {
                        return Err(refused(400, "bad_request"));
                    }
                    // A new reader starts at 0 and receives all waiting
                    // messages.
                    entry.readers.insert(
                        *reader,
                        Reader {
                            acked: 0,
                            push_url: change.push_url.flatten().map(str::to_owned),
                            token_hash: new_hash,
                            role: change.role.unwrap_or(Role::Full),
                        },
                    );
                    false
                }
            };
            if token_changed {
                server.recheck(&id);
            }
            server.reader_puts += 1;
            Ok(())
        })
    }

    async fn drop_read_token(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        token: &Token,
    ) -> Result<(), RelayError> {
        self.with(relay, |server, _| {
            server.new_call()?;
            let (entry, principal) = server.principal(mailbox, token)?;
            if !principal.is_full_reader() {
                return Err(refused(401, "unauthorized"));
            }
            entry.shared_hash = None;
            server.recheck(mailbox);
            Ok(())
        })
    }

    async fn put_send_token(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        token: &Token,
        send_token: &Token,
    ) -> Result<(), RelayError> {
        self.with(relay, |server, _| {
            server.new_call()?;
            let (entry, principal) = server.principal(mailbox, token)?;
            if !principal.is_full_reader() {
                return Err(refused(401, "unauthorized"));
            }
            entry.send_hash = token_hash(send_token);
            Ok(())
        })
    }

    async fn revoke_readers(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        token: &Token,
        keep: &[ReaderId],
        drop_shared_token: bool,
    ) -> Result<Vec<ReaderId>, RelayError> {
        self.with(relay, |server, _| {
            server.new_call()?;
            let (entry, principal) = server.principal(mailbox, token)?;
            let Principal::Reader(caller, Role::Full) = principal else {
                return Err(refused(401, "unauthorized"));
            };
            if keep.len() > MAX_READERS {
                return Err(refused(400, "bad_request"));
            }
            let revoked: Vec<ReaderId> = entry
                .readers
                .keys()
                .filter(|id| **id != caller && !keep.contains(id))
                .copied()
                .collect();
            for id in &revoked {
                entry.readers.remove(id);
            }
            if drop_shared_token {
                entry.shared_hash = None;
            }
            entry.clean();
            for id in &revoked {
                server.end_subscriptions(mailbox, Some(id), "unknown_reader");
            }
            server.recheck(mailbox);
            Ok(revoked)
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
            let (entry, principal) = server.principal(mailbox, read_token)?;
            if let Principal::Reader(own, Role::Satellite) = principal
                && own != *reader
            {
                return Err(refused(401, "unauthorized"));
            }
            if entry.readers.remove(reader).is_none() {
                return Err(refused(404, "unknown_reader"));
            }
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
            let old = server.old;
            if *authorisation != server.access_token
                && !server
                    .mailboxes
                    .values()
                    .any(|m| m.principal(&auth_hash, old).is_some_and(Principal::manages))
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
            let mailbox = server.for_sending(&id, send_token)?;
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
        let hash = token_hash(read_token);
        let result = self.relay.with(&self.url, |server, _| {
            // A new subscription of the mailbox on this connection
            // replaces the one before, also when it fails.
            let connection = self.id;
            server
                .subscriptions
                .retain(|s| !(s.connection == connection && s.mailbox == id));
            let old = server.old;
            let check = match server.mailboxes.get(&id) {
                None => ReadCheck::UnknownMailbox,
                Some(m) => m.check(reader, &hash, old),
            };
            let acked = match check {
                ReadCheck::Ok(acked) => acked,
                denied => return Ok(Err(denied.code())),
            };
            let mailbox = &server.mailboxes[&id];
            let waiting: Vec<_> = mailbox
                .messages
                .iter()
                .filter(|(seq, _, _)| *seq > acked)
                .map(|(seq, _, data)| (*seq, data.clone()))
                .collect();
            server.subscriptions.push(Subscription {
                connection,
                mailbox: id,
                reader: *reader,
                token_hash: hash.clone(),
                relay: self.url,
                deliveries: self.deliveries.clone(),
            });
            Ok(Ok(waiting))
        });
        match result {
            Ok(Ok(waiting)) => {
                self.send(ServerFrame::Subscribed { mailbox: id });
                for (seq, data) in waiting {
                    self.send(ServerFrame::Message {
                        mailbox: id,
                        seq,
                        data,
                    });
                }
            }
            Ok(Err(code)) => self.send(ServerFrame::Error {
                mailbox: Some(id),
                code: code.into(),
            }),
            Err(error) => self.send(ServerFrame::Error {
                mailbox: Some(id),
                code: error.to_string(),
            }),
        }
        Ok(())
    }

    fn ack(&mut self, mailbox: &MailboxId, seq: u64) -> Result<(), RelayError> {
        let connection = self.id;
        self.relay.with(&self.url, |server, _| {
            let old = server.old;
            let subscriptions: Vec<(ReaderId, String)> = server
                .subscriptions
                .iter()
                .filter(|s| s.connection == connection && s.mailbox == *mailbox)
                .map(|s| (s.reader, s.token_hash.clone()))
                .collect();
            if let Some(mailbox) = server.mailboxes.get_mut(mailbox) {
                for (reader, hash) in subscriptions {
                    // Every acknowledgement checks the token again.
                    if !matches!(mailbox.check(&reader, &hash, old), ReadCheck::Ok(_)) {
                        continue;
                    }
                    if let Some(entry) = mailbox.readers.get_mut(&reader) {
                        entry.acked = entry.acked.max(seq);
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

#[cfg(test)]
mod tests {
    //! The fake follows the real relay where the client relies on it
    //! (`crates/relay/tests/relay.rs` checks the real one).

    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    use lz_core::wire::RelayUrl;
    use tokio::sync::mpsc;

    use super::FakeRelay;
    use crate::relay::{MailboxRead, ReaderChange, Relay, RelayError, Role, ServerFrame};

    const URL: &str = "https://relay.example";
    const ACCESS: [u8; 32] = [1; 32];
    const SHARED: [u8; 32] = [2; 32];
    const SEND: [u8; 32] = [3; 32];
    const A: [u8; 16] = [0xA; 16];
    const B: [u8; 16] = [0xB; 16];
    const S: [u8; 16] = [0x5; 16];

    fn relay() -> (FakeRelay, RelayUrl) {
        let relay = FakeRelay::new(Arc::new(AtomicU64::new(1_794_600_000)));
        relay.add(URL, ACCESS);
        (relay, RelayUrl::new(URL).unwrap())
    }

    fn unauthorized<T: std::fmt::Debug>(result: Result<T, RelayError>) {
        assert!(
            matches!(result, Err(RelayError::Refused { status: 401, .. })),
            "{result:?}"
        );
    }

    fn token(n: u8) -> ReaderChange<'static> {
        // Leaked once per test value; tests only.
        let token: &'static [u8; 32] = Box::leak(Box::new([n; 32]));
        ReaderChange {
            token: Some(token),
            ..ReaderChange::default()
        }
    }

    #[tokio::test]
    async fn own_tokens_rights_and_revoking() {
        let (relay, url) = relay();
        assert!(relay.features(&url).await.unwrap().reader_tokens);
        assert!(
            !relay
                .old_client()
                .features(&url)
                .await
                .unwrap()
                .reader_tokens
        );
        let m = relay
            .create_mailbox(&url, &ACCESS, MailboxRead::Shared(&SHARED), &SEND)
            .await
            .unwrap();
        // A takes its own token; the shared token no longer reads as A.
        relay
            .put_reader(&url, &m, &SHARED, &A, token(10))
            .await
            .unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut c = relay.connect(&url, tx).await.unwrap();
        c.subscribe(&m, &SHARED, &A).unwrap();
        let frame = rx.recv().await.unwrap().frame;
        assert!(matches!(frame, ServerFrame::Error { code, .. } if code == "unauthorized"));
        c.subscribe(&m, &[10; 32], &A).unwrap();
        assert!(matches!(
            rx.recv().await.unwrap().frame,
            ServerFrame::Subscribed { .. }
        ));
        // A token already taken is refused.
        let taken = relay.put_reader(&url, &m, &SHARED, &B, token(10)).await;
        assert!(matches!(
            taken,
            Err(RelayError::Refused { status: 400, .. })
        ));
        // A satellite may change only itself, and no role.
        let satellite = ReaderChange {
            role: Some(Role::Satellite),
            ..token(20)
        };
        relay
            .put_reader(&url, &m, &[10; 32], &S, satellite)
            .await
            .unwrap();
        unauthorized(relay.put_reader(&url, &m, &[20; 32], &B, token(21)).await);
        let own_role = ReaderChange {
            role: Some(Role::Full),
            ..ReaderChange::default()
        };
        unauthorized(relay.put_reader(&url, &m, &[20; 32], &S, own_role).await);
        unauthorized(relay.delete_mailbox(&url, &m, &[20; 32]).await);
        unauthorized(relay.revoke_readers(&url, &m, &[20; 32], &[], true).await);
        unauthorized(relay.drop_read_token(&url, &m, &[20; 32]).await);
        // The shared token may not revoke or drop itself.
        unauthorized(relay.revoke_readers(&url, &m, &SHARED, &[], true).await);
        // B plants itself with the shared token; A revokes all but S.
        relay
            .put_reader(&url, &m, &SHARED, &B, ReaderChange::push(None))
            .await
            .unwrap();
        let revoked = relay.revoke_readers(&url, &m, &[10; 32], &[S], true).await;
        assert_eq!(revoked, Ok(vec![B]));
        assert!(!relay.has_shared_token(URL, &m));
        unauthorized(
            relay
                .put_reader(&url, &m, &SHARED, &B, ReaderChange::push(None))
                .await,
        );
        // A revoked reader learns that it is gone, whatever its token.
        c.subscribe(&m, &SHARED, &B).unwrap();
        let frame = rx.recv().await.unwrap().frame;
        assert!(matches!(frame, ServerFrame::Error { code, .. } if code == "unknown_reader"));
    }

    #[tokio::test]
    async fn a_token_change_ends_the_subscription_with_the_old_one() {
        let (relay, url) = relay();
        let m = relay
            .create_mailbox(&url, &ACCESS, MailboxRead::Reader(&A, &[10; 32]), &SEND)
            .await
            .unwrap();
        // Without a shared token a new reader needs its own token.
        let none = relay
            .put_reader(&url, &m, &[10; 32], &B, ReaderChange::push(None))
            .await;
        assert!(matches!(none, Err(RelayError::Refused { status: 400, .. })));
        relay
            .put_reader(&url, &m, &[10; 32], &B, token(11))
            .await
            .unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut c = relay.connect(&url, tx).await.unwrap();
        c.subscribe(&m, &[11; 32], &B).unwrap();
        assert!(matches!(
            rx.recv().await.unwrap().frame,
            ServerFrame::Subscribed { .. }
        ));
        // B replaces its enrolment token: the subscription ends.
        relay
            .put_reader(&url, &m, &[11; 32], &B, token(12))
            .await
            .unwrap();
        let frame = rx.recv().await.unwrap().frame;
        assert!(matches!(frame, ServerFrame::Error { code, .. } if code == "unauthorized"));
        // A missing push address stays as it is.
        let push = ReaderChange::push(Some("https://push.example/up"));
        relay
            .put_reader(&url, &m, &[12; 32], &B, push)
            .await
            .unwrap();
        relay
            .put_reader(&url, &m, &[12; 32], &B, token(12))
            .await
            .unwrap();
        assert!(relay.push_url(URL, &m, &B).is_some());
        // Leaving with the own token.
        relay.delete_reader(&url, &m, &[12; 32], &B).await.unwrap();
        assert!(!relay.has_reader(URL, &m, &B));
    }

    #[tokio::test]
    async fn an_old_relay_ignores_own_tokens() {
        let (relay, url) = relay();
        relay.set_old(URL, true);
        assert!(!relay.features(&url).await.unwrap().reader_tokens);
        let new_form = relay
            .create_mailbox(&url, &ACCESS, MailboxRead::Reader(&A, &[10; 32]), &SEND)
            .await;
        assert!(matches!(
            new_form,
            Err(RelayError::Refused { status: 400, .. })
        ));
        let m = relay
            .create_mailbox(&url, &ACCESS, MailboxRead::Shared(&SHARED), &SEND)
            .await
            .unwrap();
        relay
            .put_reader(&url, &m, &SHARED, &A, token(10))
            .await
            .unwrap();
        assert!(!relay.has_own_token(URL, &m, &A));
        unauthorized(relay.delete_mailbox(&url, &m, &[10; 32]).await);
        let call = relay.revoke_readers(&url, &m, &SHARED, &[], true).await;
        assert!(matches!(call, Err(RelayError::Refused { status: 404, .. })));
        // The token is checked before the reader.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut c = relay.connect(&url, tx).await.unwrap();
        c.subscribe(&m, &[9; 32], &B).unwrap();
        let frame = rx.recv().await.unwrap().frame;
        assert!(matches!(frame, ServerFrame::Error { code, .. } if code == "unauthorized"));
    }
}
