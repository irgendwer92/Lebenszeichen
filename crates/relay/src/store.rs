//! SQLite storage of the relay.
//!
//! Stored are mailboxes with token hashes, how they were created, readers
//! with push URL and last acknowledgement, waiting messages with their
//! arrival time, open vouchers (hash only) and handover slots. Nothing else:
//! no history, no addresses, no names.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::ids::{self, HandoverId, Hash, MailboxId, ReaderId, Token};
use crate::limits::{
    HANDOVER_TTL_SECS, MAILBOX_IDLE_DAYS, MAX_DERIVED_MAILBOXES, MAX_HANDOVERS,
    MAX_HANDOVERS_PER_TOKEN, MAX_READERS_PER_MAILBOX, MAX_WAITING_MESSAGES, MESSAGE_TTL_SECS,
    REVEAL_BYTES,
};

/// Schema version 0, as created before `user_version` was used. A database
/// at version 0 may already contain these tables.
const SCHEMA_V0: &str = "
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

/// Version 0 to 1 (M5): origin of mailboxes, derived mailboxes, handovers.
///
/// Mailboxes from before the migration count as voucher mailboxes, so people
/// invited before the update can derive mailboxes; that is all the default of
/// `origin` is for, inserts always name it. `parent` is the voucher mailbox of
/// a derived one and deliberately no foreign key: a derived mailbox outlives
/// its parent.
///
/// A handover slot holds `data` and the hash of the token that fetches it. A
/// reveal slot (`reveal` = 1) is reserved with `data` NULL and its token is
/// the reply token, for `PUT` and `GET`; it has no `auth_hash`, so it does not
/// count towards the limit per token.
const MIGRATION_V1: &str = "
ALTER TABLE mailboxes ADD COLUMN origin INTEGER NOT NULL DEFAULT 1;
ALTER TABLE mailboxes ADD COLUMN parent BLOB;
CREATE INDEX mailboxes_read_token_hash ON mailboxes(read_token_hash);
CREATE INDEX mailboxes_parent ON mailboxes(parent);
CREATE TABLE handovers (
    id         BLOB PRIMARY KEY,
    data       BLOB,
    fetch_hash BLOB NOT NULL,
    auth_hash  BLOB,
    expires_at INTEGER NOT NULL,
    reveal     INTEGER NOT NULL
) WITHOUT ROWID;
";

/// The schema version this code works with.
pub const SCHEMA_VERSION: i64 = 1;

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

/// How a mailbox was created, as stored in `mailboxes.origin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// With the access token.
    AccessToken = 0,
    /// With a voucher; may derive mailboxes.
    Voucher = 1,
    /// With the read token of a voucher mailbox.
    Derived = 2,
}

/// What authorizes the creation of a mailbox.
#[derive(Debug, Clone, Copy)]
pub enum Grant {
    /// The access token of the relay.
    AccessToken,
    /// `SHA-256` of another bearer token: a voucher, or else the read token of
    /// a voucher mailbox.
    Token(Hash),
}

/// Result of creating a mailbox.
#[derive(Debug, PartialEq, Eq)]
pub enum CreateMailbox {
    /// Created with this origin.
    Created(Origin),
    /// Neither a valid voucher nor the read token of a voucher mailbox.
    Unauthorized,
    /// The voucher mailbox has derived the maximum number of mailboxes.
    Quota,
}

/// A handover slot to store with `PUT`.
#[derive(Debug, Clone, Copy)]
pub struct NewHandover<'a> {
    /// The content.
    pub data: &'a [u8],
    /// `SHA-256` of the fetch token.
    pub fetch_hash: Hash,
    /// `SHA-256` of the token that authorized the `PUT`.
    pub auth_hash: Hash,
    /// `SHA-256` of the reply token; reserves the reveal slot.
    pub reply_hash: Option<Hash>,
}

/// Result of storing a handover slot.
#[derive(Debug, PartialEq, Eq)]
pub enum PutHandover {
    /// Stored, with the reveal slot reserved if asked for.
    Stored,
    /// The identifier, or its reveal identifier, is taken or reserved.
    Exists,
    /// The authorizing token already has the maximum of waiting slots.
    Quota,
    /// The relay already has the maximum of slots.
    Full,
}

/// Result of filling a reveal slot.
#[derive(Debug, PartialEq, Eq)]
pub enum PutReveal {
    /// Stored.
    Stored,
    /// Neither reserved nor stored.
    Unknown,
    /// Already filled, or not a reveal slot.
    Exists,
    /// The token is not the reply token.
    Unauthorized,
    /// The body is not exactly [`REVEAL_BYTES`] long.
    BadLength,
}

/// Result of fetching a handover slot.
#[derive(Debug, PartialEq, Eq)]
pub enum TakeHandover {
    /// The content; the slot is deleted.
    Data(Vec<u8>),
    /// No content under this identifier.
    Unknown,
    /// The token is not the one for this slot.
    Unauthorized,
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
    /// Number of expired handover slots and reservations deleted.
    pub handovers: usize,
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

    fn init(mut conn: Connection) -> rusqlite::Result<Self> {
        // `secure_delete` overwrites deleted content, so delivered messages
        // do not linger in free pages of the file.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "secure_delete", "ON")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&mut conn)?;
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

    /// Creates a mailbox under `id`. A [`Grant::Token`] is tried as a voucher
    /// first, which is then redeemed, and else as the read token of a voucher
    /// mailbox, whose derived mailboxes are counted; all in one transaction.
    pub fn create_mailbox(
        &self,
        id: &MailboxId,
        auth: &MailboxAuth,
        grant: Grant,
        now: i64,
    ) -> rusqlite::Result<CreateMailbox> {
        self.with(|c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (origin, parent) = match grant {
                Grant::AccessToken => (Origin::AccessToken, None),
                Grant::Token(hash) => {
                    let redeemed = tx.execute(
                        "DELETE FROM vouchers WHERE hash = ?1 AND expires_at > ?2",
                        params![&hash[..], now],
                    )?;
                    if redeemed == 1 {
                        (Origin::Voucher, None)
                    } else {
                        let Some(parent) = tx
                            .query_row(
                                "SELECT id FROM mailboxes WHERE read_token_hash = ?1 AND origin = ?2
                                 ORDER BY id LIMIT 1",
                                params![&hash[..], Origin::Voucher as i64],
                                |row| row.get::<_, Vec<u8>>(0),
                            )
                            .optional()?
                        else {
                            return Ok(CreateMailbox::Unauthorized);
                        };
                        let derived: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM mailboxes WHERE parent = ?1",
                            params![&parent],
                            |row| row.get(0),
                        )?;
                        if derived >= MAX_DERIVED_MAILBOXES as i64 {
                            return Ok(CreateMailbox::Quota);
                        }
                        (Origin::Derived, Some(parent))
                    }
                }
            };
            tx.execute(
                "INSERT INTO mailboxes
                   (id, read_token_hash, send_token_hash, next_seq, last_used_day, origin, parent)
                 VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6)",
                params![
                    &id[..],
                    &auth.read_token_hash[..],
                    &auth.send_token_hash[..],
                    day_of(now),
                    origin as i64,
                    parent
                ],
            )?;
            tx.commit()?;
            Ok(CreateMailbox::Created(origin))
        })
    }

    /// Whether `hash` is the read token hash of any mailbox.
    pub fn is_read_token(&self, hash: &Hash) -> rusqlite::Result<bool> {
        self.with(|c| {
            c.query_row(
                "SELECT EXISTS (SELECT 1 FROM mailboxes WHERE read_token_hash = ?1)",
                params![&hash[..]],
                |row| row.get(0),
            )
        })
    }

    /// Stores a handover slot and, with a reply hash, reserves its reveal
    /// slot in the same transaction.
    pub fn put_handover(
        &self,
        id: &HandoverId,
        slot: &NewHandover<'_>,
        now: i64,
    ) -> rusqlite::Result<PutHandover> {
        self.with(|c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            purge_handovers(&tx, now)?;
            let reveal = slot.reply_hash.map(|hash| (ids::reveal_slot(id), hash));
            if handover_exists(&tx, id)?
                || reveal
                    .as_ref()
                    .map(|(reveal_id, _)| handover_exists(&tx, reveal_id))
                    .transpose()?
                    .unwrap_or(false)
            {
                return Ok(PutHandover::Exists);
            }
            let per_token: i64 = tx.query_row(
                "SELECT COUNT(*) FROM handovers WHERE auth_hash = ?1",
                params![&slot.auth_hash[..]],
                |row| row.get(0),
            )?;
            if per_token >= MAX_HANDOVERS_PER_TOKEN as i64 {
                return Ok(PutHandover::Quota);
            }
            let total: i64 =
                tx.query_row("SELECT COUNT(*) FROM handovers", [], |row| row.get(0))?;
            let needed = 1 + i64::from(reveal.is_some());
            if total + needed > MAX_HANDOVERS as i64 {
                return Ok(PutHandover::Full);
            }
            let expires_at = now + HANDOVER_TTL_SECS;
            tx.execute(
                "INSERT INTO handovers (id, data, fetch_hash, auth_hash, expires_at, reveal)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0)",
                params![
                    &id[..],
                    slot.data,
                    &slot.fetch_hash[..],
                    &slot.auth_hash[..],
                    expires_at
                ],
            )?;
            if let Some((reveal_id, reply_hash)) = reveal {
                tx.execute(
                    "INSERT INTO handovers (id, data, fetch_hash, auth_hash, expires_at, reveal)
                     VALUES (?1, NULL, ?2, NULL, ?3, 1)",
                    params![&reveal_id[..], &reply_hash[..], expires_at],
                )?;
            }
            tx.commit()?;
            Ok(PutHandover::Stored)
        })
    }

    /// Fills a reserved reveal slot, once, with exactly [`REVEAL_BYTES`]
    /// bytes. `data` is `None` if the body was longer. The expiry of the
    /// reservation stays.
    pub fn put_reveal(
        &self,
        id: &HandoverId,
        token: &Token,
        data: Option<&[u8]>,
        now: i64,
    ) -> rusqlite::Result<PutReveal> {
        self.with(|c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            purge_handovers(&tx, now)?;
            let Some((filled, reveal, fetch_hash)) = tx
                .query_row(
                    "SELECT data IS NOT NULL, reveal, fetch_hash FROM handovers WHERE id = ?1",
                    params![&id[..]],
                    |row| {
                        Ok((
                            row.get::<_, bool>(0)?,
                            row.get::<_, bool>(1)?,
                            row.get::<_, Vec<u8>>(2)?,
                        ))
                    },
                )
                .optional()?
            else {
                return Ok(PutReveal::Unknown);
            };
            if filled || !reveal {
                return Ok(PutReveal::Exists);
            }
            if !hash_matches(token, &fetch_hash) {
                return Ok(PutReveal::Unauthorized);
            }
            let Some(data) = data.filter(|d| d.len() == REVEAL_BYTES) else {
                return Ok(PutReveal::BadLength);
            };
            tx.execute(
                "UPDATE handovers SET data = ?2 WHERE id = ?1",
                params![&id[..], data],
            )?;
            tx.commit()?;
            Ok(PutReveal::Stored)
        })
    }

    /// Returns the content of a handover slot and deletes it, if `token` is
    /// the one for it. A reserved reveal slot without content is unknown.
    pub fn take_handover(
        &self,
        id: &HandoverId,
        token: &Token,
        now: i64,
    ) -> rusqlite::Result<TakeHandover> {
        self.with(|c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            purge_handovers(&tx, now)?;
            let Some((data, fetch_hash)) = tx
                .query_row(
                    "SELECT data, fetch_hash FROM handovers WHERE id = ?1 AND data IS NOT NULL",
                    params![&id[..]],
                    |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .optional()?
            else {
                return Ok(TakeHandover::Unknown);
            };
            if !hash_matches(token, &fetch_hash) {
                return Ok(TakeHandover::Unauthorized);
            }
            tx.execute("DELETE FROM handovers WHERE id = ?1", params![&id[..]])?;
            tx.commit()?;
            Ok(TakeHandover::Data(data))
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

    /// Deletes expired messages, vouchers and handover slots and mailboxes
    /// that were not used
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
            let handovers = purge_handovers(&tx, now)?;
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
                handovers,
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

/// Brings the schema to [`SCHEMA_VERSION`], each step exactly once and in one
/// transaction. A fresh database takes the same steps as an old one, so both
/// end up identical.
fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH),
            Some(format!(
                "Schemaversion {version} ist neuer als {SCHEMA_VERSION}"
            )),
        ));
    }
    if version < 1 {
        tx.execute_batch(SCHEMA_V0)?;
        tx.execute_batch(MIGRATION_V1)?;
    }
    if version < SCHEMA_VERSION {
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    tx.commit()
}

/// Deletes expired handover slots and reservations.
fn purge_handovers(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<usize> {
    tx.execute("DELETE FROM handovers WHERE expires_at <= ?1", params![now])
}

/// Whether a handover slot or reservation exists under `id`.
fn handover_exists(tx: &Transaction<'_>, id: &HandoverId) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM handovers WHERE id = ?1)",
        params![&id[..]],
        |row| row.get(0),
    )
}

/// Checks a presented token against a stored hash of unchecked length.
fn hash_matches(token: &Token, stored: &[u8]) -> bool {
    Hash::try_from(stored).is_ok_and(|hash| ids::token_matches(token, &hash))
}
