//! SQLite storage of the relay.
//!
//! Stored are mailboxes with token hashes, readers with push URL and last
//! acknowledgement, waiting messages with their arrival time and open
//! vouchers (hash only). Nothing else: no history, no addresses, no names.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

use crate::ids::{Hash, MailboxId, ReaderId};
use crate::limits::{
    MAILBOX_IDLE_DAYS, MAX_READERS_PER_MAILBOX, MAX_WAITING_MESSAGES, MESSAGE_TTL_SECS,
};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS mailboxes (
    id              BLOB PRIMARY KEY,
    read_token_hash BLOB NOT NULL,
    send_token_hash BLOB NOT NULL,
    next_seq        INTEGER NOT NULL,
    last_used_day   INTEGER NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS readers (
    mailbox  BLOB NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
    id       BLOB NOT NULL,
    push_url TEXT,
    last_ack INTEGER NOT NULL,
    PRIMARY KEY (mailbox, id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS messages (
    mailbox     BLOB NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,
    received_at INTEGER NOT NULL,
    data        BLOB NOT NULL,
    PRIMARY KEY (mailbox, seq)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS messages_received_at ON messages(received_at);
CREATE TABLE IF NOT EXISTS vouchers (
    hash       BLOB PRIMARY KEY,
    expires_at INTEGER NOT NULL
) WITHOUT ROWID;
";

/// Day number used for "day of last use": whole days since the Unix epoch.
pub fn day_of(now: i64) -> i64 {
    now.div_euclid(86_400)
}

/// Token hashes of a mailbox.
#[derive(Debug, Clone, Copy)]
pub struct MailboxAuth {
    /// `SHA-256` of the read token.
    pub read_token_hash: Hash,
    /// `SHA-256` of the send token.
    pub send_token_hash: Hash,
}

/// Result of registering or updating a reader.
#[derive(Debug, PartialEq, Eq)]
pub enum PutReader {
    /// The reader is registered.
    Ok,
    /// The mailbox does not exist.
    UnknownMailbox,
    /// The mailbox already has the maximum number of readers.
    TooManyReaders,
}

/// Result of storing a message.
#[derive(Debug, PartialEq, Eq)]
pub enum PostMessage {
    /// Stored under this sequence number.
    Stored(i64),
    /// The mailbox does not exist.
    UnknownMailbox,
    /// The mailbox already holds the maximum number of waiting messages.
    Full,
}

/// Result of an acknowledgement.
#[derive(Debug, PartialEq, Eq)]
pub enum Ack {
    /// Recorded.
    Ok,
    /// The reader (or its mailbox) is gone.
    UnknownReader,
}

/// What [`Store::cleanup`] removed.
#[derive(Debug, Default)]
pub struct Cleanup {
    /// Mailboxes deleted because they were not used for too long.
    pub mailboxes: Vec<MailboxId>,
    /// Number of expired messages deleted.
    pub messages: usize,
    /// Number of expired vouchers deleted.
    pub vouchers: usize,
}

/// The relay's database. All methods are blocking; call them from
/// `spawn_blocking`.
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Opens or creates the database file at `path`.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::init(Connection::open(path)?)
    }

    /// Opens a fresh database that lives only in memory, for tests.
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        // `secure_delete` overwrites deleted content, so delivered messages
        // do not linger in free pages of the file.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "secure_delete", "ON")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn with<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        // A panic while holding the lock leaves the connection usable; SQLite
        // rolls back an unfinished transaction on its own.
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut conn)
    }

    /// Stores a new voucher.
    pub fn insert_voucher(&self, hash: &Hash, expires_at: i64) -> rusqlite::Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO vouchers (hash, expires_at) VALUES (?1, ?2)",
                params![&hash[..], expires_at],
            )?;
            Ok(())
        })
    }

    /// Creates a mailbox under `id`. With `voucher`, the voucher is redeemed in
    /// the same transaction; returns `false` if it is unknown or expired.
    pub fn create_mailbox(
        &self,
        id: &MailboxId,
        auth: &MailboxAuth,
        voucher: Option<&Hash>,
        now: i64,
    ) -> rusqlite::Result<bool> {
        self.with(|c| {
            let tx = c.transaction()?;
            if let Some(voucher) = voucher {
                let redeemed = tx.execute(
                    "DELETE FROM vouchers WHERE hash = ?1 AND expires_at > ?2",
                    params![&voucher[..], now],
                )?;
                if redeemed != 1 {
                    return Ok(false);
                }
            }
            tx.execute(
                "INSERT INTO mailboxes (id, read_token_hash, send_token_hash, next_seq, last_used_day)
                 VALUES (?1, ?2, ?3, 1, ?4)",
                params![
                    &id[..],
                    &auth.read_token_hash[..],
                    &auth.send_token_hash[..],
                    day_of(now)
                ],
            )?;
            tx.commit()?;
            Ok(true)
        })
    }

    /// Returns the token hashes of a mailbox, or `None` if it does not exist.
    pub fn mailbox_auth(&self, id: &MailboxId) -> rusqlite::Result<Option<MailboxAuth>> {
        self.with(|c| {
            c.query_row(
                "SELECT read_token_hash, send_token_hash FROM mailboxes WHERE id = ?1",
                params![&id[..]],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()
            .map(|row| {
                row.and_then(|(read, send)| {
                    Some(MailboxAuth {
                        read_token_hash: read.try_into().ok()?,
                        send_token_hash: send.try_into().ok()?,
                    })
                })
            })
        })
    }

    /// Deletes a mailbox with its readers and messages.
    pub fn delete_mailbox(&self, id: &MailboxId) -> rusqlite::Result<bool> {
        self.with(|c| Ok(c.execute("DELETE FROM mailboxes WHERE id = ?1", params![&id[..]])? > 0))
    }

    /// Registers a reader or updates its push URL. An existing reader keeps
    /// its last acknowledgement; a new one starts at 0 and so receives all
    /// waiting messages.
    pub fn put_reader(
        &self,
        mailbox: &MailboxId,
        reader: &ReaderId,
        push_url: Option<&str>,
        now: i64,
    ) -> rusqlite::Result<PutReader> {
        self.with(|c| {
            let tx = c.transaction()?;
            let touched = tx.execute(
                "UPDATE mailboxes SET last_used_day = ?2 WHERE id = ?1",
                params![&mailbox[..], day_of(now)],
            )?;
            if touched == 0 {
                return Ok(PutReader::UnknownMailbox);
            }
            let updated = tx.execute(
                "UPDATE readers SET push_url = ?3 WHERE mailbox = ?1 AND id = ?2",
                params![&mailbox[..], &reader[..], push_url],
            )?;
            if updated == 0 {
                let count: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM readers WHERE mailbox = ?1",
                    params![&mailbox[..]],
                    |row| row.get(0),
                )?;
                if count >= MAX_READERS_PER_MAILBOX as i64 {
                    return Ok(PutReader::TooManyReaders);
                }
                tx.execute(
                    "INSERT INTO readers (mailbox, id, push_url, last_ack) VALUES (?1, ?2, ?3, 0)",
                    params![&mailbox[..], &reader[..], push_url],
                )?;
            }
            tx.commit()?;
            Ok(PutReader::Ok)
        })
    }

    /// Removes a reader. Returns `false` if it was not registered. Messages
    /// that all remaining readers have acknowledged are deleted.
    pub fn delete_reader(&self, mailbox: &MailboxId, reader: &ReaderId) -> rusqlite::Result<bool> {
        self.with(|c| {
            let tx = c.transaction()?;
            let removed = tx.execute(
                "DELETE FROM readers WHERE mailbox = ?1 AND id = ?2",
                params![&mailbox[..], &reader[..]],
            )?;
            purge_acknowledged(&tx, mailbox)?;
            tx.commit()?;
            Ok(removed > 0)
        })
    }

    /// Stores a message and assigns the next sequence number.
    pub fn post_message(
        &self,
        mailbox: &MailboxId,
        data: &[u8],
        now: i64,
    ) -> rusqlite::Result<PostMessage> {
        self.with(|c| {
            let tx = c.transaction()?;
            let Some(seq) = tx
                .query_row(
                    "SELECT next_seq FROM mailboxes WHERE id = ?1",
                    params![&mailbox[..]],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
            else {
                return Ok(PostMessage::UnknownMailbox);
            };
            let waiting: i64 = tx.query_row(
                "SELECT COUNT(*) FROM messages WHERE mailbox = ?1",
                params![&mailbox[..]],
                |row| row.get(0),
            )?;
            if waiting >= MAX_WAITING_MESSAGES as i64 {
                return Ok(PostMessage::Full);
            }
            tx.execute(
                "INSERT INTO messages (mailbox, seq, received_at, data) VALUES (?1, ?2, ?3, ?4)",
                params![&mailbox[..], seq, now, data],
            )?;
            tx.execute(
                "UPDATE mailboxes SET next_seq = ?2 WHERE id = ?1",
                params![&mailbox[..], seq + 1],
            )?;
            tx.commit()?;
            Ok(PostMessage::Stored(seq))
        })
    }

    /// Returns the push URLs of all readers of a mailbox that have one.
    pub fn push_targets(&self, mailbox: &MailboxId) -> rusqlite::Result<Vec<(ReaderId, String)>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, push_url FROM readers WHERE mailbox = ?1 AND push_url IS NOT NULL",
            )?;
            let rows = stmt.query_map(params![&mailbox[..]], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut targets = Vec::new();
            for row in rows {
                let (id, url) = row?;
                if let Ok(id) = ReaderId::try_from(id) {
                    targets.push((id, url));
                }
            }
            Ok(targets)
        })
    }

    /// Returns the last acknowledgement of a reader, or `None` if the reader is
    /// not registered. Marks the mailbox as used today.
    pub fn open_reader(
        &self,
        mailbox: &MailboxId,
        reader: &ReaderId,
        now: i64,
    ) -> rusqlite::Result<Option<i64>> {
        self.with(|c| {
            let last_ack = c
                .query_row(
                    "SELECT last_ack FROM readers WHERE mailbox = ?1 AND id = ?2",
                    params![&mailbox[..], &reader[..]],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if last_ack.is_some() {
                c.execute(
                    "UPDATE mailboxes SET last_used_day = ?2 WHERE id = ?1",
                    params![&mailbox[..], day_of(now)],
                )?;
            }
            Ok(last_ack)
        })
    }

    /// Returns the waiting messages with a sequence number above `after`, in
    /// order.
    pub fn messages_after(
        &self,
        mailbox: &MailboxId,
        after: i64,
    ) -> rusqlite::Result<Vec<(i64, Vec<u8>)>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT seq, data FROM messages WHERE mailbox = ?1 AND seq > ?2 ORDER BY seq",
            )?;
            let rows = stmt.query_map(params![&mailbox[..], after], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
            rows.collect()
        })
    }

    /// Records a cumulative acknowledgement up to `seq` and deletes messages
    /// that every reader of the mailbox has acknowledged. An acknowledgement
    /// beyond the last assigned sequence number is capped at that number.
    pub fn ack(
        &self,
        mailbox: &MailboxId,
        reader: &ReaderId,
        seq: i64,
        now: i64,
    ) -> rusqlite::Result<Ack> {
        self.with(|c| {
            let tx = c.transaction()?;
            let updated = tx.execute(
                "UPDATE readers SET last_ack = MAX(last_ack, MIN(?3,
                    (SELECT next_seq - 1 FROM mailboxes WHERE id = ?1)))
                 WHERE mailbox = ?1 AND id = ?2",
                params![&mailbox[..], &reader[..], seq],
            )?;
            if updated == 0 {
                return Ok(Ack::UnknownReader);
            }
            tx.execute(
                "UPDATE mailboxes SET last_used_day = ?2 WHERE id = ?1",
                params![&mailbox[..], day_of(now)],
            )?;
            purge_acknowledged(&tx, mailbox)?;
            tx.commit()?;
            Ok(Ack::Ok)
        })
    }

    /// Number of waiting messages in a mailbox.
    pub fn waiting_messages(&self, mailbox: &MailboxId) -> rusqlite::Result<usize> {
        self.with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM messages WHERE mailbox = ?1",
                params![&mailbox[..]],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n as usize)
        })
    }

    /// Deletes expired messages and vouchers and mailboxes that were not used
    /// for [`MAILBOX_IDLE_DAYS`] days.
    pub fn cleanup(&self, now: i64) -> rusqlite::Result<Cleanup> {
        self.with(|c| {
            let tx = c.transaction()?;
            let messages = tx.execute(
                "DELETE FROM messages WHERE received_at <= ?1",
                params![now - MESSAGE_TTL_SECS],
            )?;
            let vouchers =
                tx.execute("DELETE FROM vouchers WHERE expires_at <= ?1", params![now])?;
            let oldest_day = day_of(now) - MAILBOX_IDLE_DAYS;
            let mut mailboxes = Vec::new();
            {
                let mut stmt = tx.prepare("SELECT id FROM mailboxes WHERE last_used_day < ?1")?;
                let rows = stmt.query_map(params![oldest_day], |row| row.get::<_, Vec<u8>>(0))?;
                for row in rows {
                    if let Ok(id) = MailboxId::try_from(row?) {
                        mailboxes.push(id);
                    }
                }
            }
            tx.execute(
                "DELETE FROM mailboxes WHERE last_used_day < ?1",
                params![oldest_day],
            )?;
            tx.commit()?;
            Ok(Cleanup {
                mailboxes,
                messages,
                vouchers,
            })
        })
    }
}

/// Deletes the messages of `mailbox` that all its readers have acknowledged.
/// Without readers nothing is deleted; such messages wait for expiry.
fn purge_acknowledged(conn: &Connection, mailbox: &MailboxId) -> rusqlite::Result<()> {
    // MIN over no rows is NULL, and `seq <= NULL` matches nothing.
    conn.execute(
        "DELETE FROM messages WHERE mailbox = ?1
           AND seq <= (SELECT MIN(last_ack) FROM readers WHERE mailbox = ?1)",
        params![&mailbox[..]],
    )?;
    Ok(())
}
