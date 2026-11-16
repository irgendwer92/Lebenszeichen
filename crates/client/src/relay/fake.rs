//! Relays in memory, following the documented semantics, for tests.
//!
//! Not covered: expiry of vouchers and messages, the rate limit per hour and
//! wake signals. Push addresses are only stored and roughly checked.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lz_core::wire::{MailboxId, RelayUrl, Token};
use rand_core::{OsRng, RngCore};
use tokio::sync::mpsc;

use super::{Connection, Delivery, ReaderId, Relay, RelayError, ServerFrame, token_hash};

/// Most messages waiting in one mailbox.
const MAX_WAITING: usize = 32;

/// Any number of fake relays, reachable by their base address.
#[derive(Clone, Default)]
pub struct FakeRelay {
    servers: Arc<Mutex<HashMap<String, Server>>>,
    next_connection: Arc<AtomicU64>,
}

#[derive(Default)]
struct Server {
    /// Unreachable, as if the network were down.
    down: bool,
    access_token: Token,
    /// Hashes of open vouchers.
    vouchers: HashSet<String>,
    mailboxes: HashMap<MailboxId, Mailbox>,
    subscriptions: Vec<Subscription>,
    /// Successful `put_reader` calls, for tests that expect none.
    reader_puts: u64,
}

struct Mailbox {
    read_hash: String,
    send_hash: String,
    /// Registered readers with their last acknowledged sequence number.
    readers: HashMap<ReaderId, u64>,
    /// Push address of each reader that has one.
    push_urls: HashMap<ReaderId, String>,
    messages: Vec<(u64, Vec<u8>)>,
    last_seq: u64,
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

impl FakeRelay {
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

    /// Whether the mailbox exists.
    pub fn has_mailbox(&self, url: &str, mailbox: &MailboxId) -> bool {
        self.servers.lock().unwrap()[url]
            .mailboxes
            .contains_key(mailbox)
    }

    /// Messages waiting in the mailbox, not yet acknowledged by every reader.
    pub fn waiting(&self, url: &str, mailbox: &MailboxId) -> usize {
        self.servers.lock().unwrap()[url].mailboxes[mailbox]
            .messages
            .len()
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

    fn with<R>(
        &self,
        relay: &RelayUrl,
        f: impl FnOnce(&mut Server) -> Result<R, RelayError>,
    ) -> Result<R, RelayError> {
        let mut servers = self.servers.lock().unwrap();
        let server = servers
            .get_mut(relay.as_str())
            .filter(|server| !server.down)
            .ok_or_else(|| RelayError::Transport(format!("{relay} nicht erreichbar")))?;
        f(server)
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
}

impl Mailbox {
    /// Drops messages that every reader has acknowledged.
    fn clean(&mut self) {
        if let Some(&min) = self.readers.values().min() {
            self.messages.retain(|(seq, _)| *seq > min);
        }
    }
}

impl Relay for FakeRelay {
    async fn create_voucher(
        &self,
        relay: &RelayUrl,
        access_token: &Token,
    ) -> Result<Token, RelayError> {
        self.with(relay, |server| {
            if *access_token != server.access_token {
                return Err(refused(401, "unauthorized"));
            }
            let mut voucher = [0; 32];
            OsRng.fill_bytes(&mut voucher);
            server.vouchers.insert(token_hash(&voucher));
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
        self.with(relay, |server| {
            if *authorisation != server.access_token
                && !server.vouchers.remove(&token_hash(authorisation))
            {
                return Err(refused(401, "unauthorized"));
            }
            let mut id = [0; 32];
            OsRng.fill_bytes(&mut id);
            server.mailboxes.insert(
                id,
                Mailbox {
                    read_hash: token_hash(read_token),
                    send_hash: token_hash(send_token),
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
        self.with(relay, |server| {
            server.mailbox(mailbox, read_token, true)?;
            server.mailboxes.remove(mailbox);
            server.subscriptions.retain(|s| s.mailbox != *mailbox);
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
        self.with(relay, |server| {
            // The real relay also refuses internal addresses.
            if push_url.is_some_and(|url| {
                !url.starts_with("https://") || url.starts_with("https://localhost")
            }) {
                return Err(refused(400, "bad_request"));
            }
            let mailbox = server.mailbox(mailbox, read_token, true)?;
            let last = mailbox.last_seq;
            mailbox.readers.entry(*reader).or_insert(last);
            match push_url {
                Some(url) => mailbox.push_urls.insert(*reader, url.to_owned()),
                None => mailbox.push_urls.remove(reader),
            };
            server.reader_puts += 1;
            Ok(())
        })
    }

    async fn deliver(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        send_token: &Token,
        envelope: &[u8],
    ) -> Result<(), RelayError> {
        self.with(relay, |server| {
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
            mailbox.messages.push((seq, envelope.to_vec()));
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
            Ok(())
        })
    }

    async fn connect(
        &self,
        relay: &RelayUrl,
        deliveries: mpsc::UnboundedSender<Delivery>,
    ) -> Result<Box<dyn Connection>, RelayError> {
        self.with(relay, |_| Ok(()))?;
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
        let result = self.relay.with(&self.url, |server| {
            let mailbox = server.mailbox(&id, read_token, true)?;
            let Some(&acked) = mailbox.readers.get(reader) else {
                return Err(refused(404, "unknown_reader"));
            };
            let waiting: Vec<_> = mailbox
                .messages
                .iter()
                .filter(|(seq, _)| *seq > acked)
                .cloned()
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
                    RelayError::Transport(detail) => detail,
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
        self.relay.with(&self.url, |server| {
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
        let _ = self.relay.with(&self.url, |server| {
            server.subscriptions.retain(|s| s.connection != id);
            Ok(())
        });
    }
}
