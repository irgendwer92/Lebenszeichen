//! In-memory registry of open subscriptions, for live delivery and to decide
//! which readers need a wake signal.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc::UnboundedSender;

use crate::ids::{MailboxId, ReaderId};

/// Something happened to a mailbox that subscribed connections must learn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A new message is waiting; the connection fetches it from the store.
    Message,
    /// The mailbox was deleted.
    MailboxDeleted,
    /// This reader was removed from the mailbox.
    ReaderRemoved(ReaderId),
    /// A token of the mailbox changed: the shared token was dropped or a
    /// reader got a new token. Connections check their subscription again.
    AccessChanged,
}

/// Identifies one WebSocket connection.
pub type ConnId = u64;

/// Channel into one connection. Unbounded is acceptable because the rate
/// limit per mailbox bounds the number of events.
pub type EventSender = UnboundedSender<(MailboxId, Event)>;

struct Subscription {
    conn: ConnId,
    reader: ReaderId,
    tx: EventSender,
}

/// All open subscriptions, by mailbox.
#[derive(Default)]
pub struct Hub {
    subscriptions: Mutex<HashMap<MailboxId, Vec<Subscription>>>,
    next_conn: AtomicU64,
}

impl Hub {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<MailboxId, Vec<Subscription>>> {
        self.subscriptions.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A fresh connection identifier.
    pub fn next_conn_id(&self) -> ConnId {
        self.next_conn.fetch_add(1, Ordering::Relaxed)
    }

    /// Subscribes `conn` to `mailbox` as `reader`, replacing an earlier
    /// subscription of the same connection to the same mailbox.
    pub fn subscribe(&self, mailbox: MailboxId, conn: ConnId, reader: ReaderId, tx: EventSender) {
        let mut subs = self.lock();
        let list = subs.entry(mailbox).or_default();
        list.retain(|s| s.conn != conn);
        list.push(Subscription { conn, reader, tx });
    }

    /// Removes the subscription of `conn` to `mailbox`.
    pub fn unsubscribe(&self, mailbox: &MailboxId, conn: ConnId) {
        let mut subs = self.lock();
        if let Some(list) = subs.get_mut(mailbox) {
            list.retain(|s| s.conn != conn);
            if list.is_empty() {
                subs.remove(mailbox);
            }
        }
    }

    /// Removes every subscription of a closed connection.
    pub fn remove_connection(&self, conn: ConnId) {
        self.lock().retain(|_, list| {
            list.retain(|s| s.conn != conn);
            !list.is_empty()
        });
    }

    /// Passes an event to every connection subscribed to `mailbox`. After
    /// [`Event::MailboxDeleted`] the subscriptions are dropped.
    pub fn notify(&self, mailbox: &MailboxId, event: Event) {
        let mut subs = self.lock();
        if let Some(list) = subs.get(mailbox) {
            for sub in list {
                // A closed connection removes itself; ignore it until then.
                let _ = sub.tx.send((*mailbox, event));
            }
        }
        if event == Event::MailboxDeleted {
            subs.remove(mailbox);
        }
    }

    /// Readers of `mailbox` that currently have an open subscription.
    pub fn subscribed_readers(&self, mailbox: &MailboxId) -> HashSet<ReaderId> {
        self.lock()
            .get(mailbox)
            .map(|list| list.iter().map(|s| s.reader).collect())
            .unwrap_or_default()
    }
}
