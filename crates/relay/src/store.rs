//! SQLite storage of the relay.
//!
//! Stored are mailboxes with token hashes, how they were created, how many
//! mailboxes a voucher mailbox has derived, readers
//! with push URL and last acknowledgement, waiting messages with their
//! arrival time, open vouchers (hash only) and handover slots. Nothing else:
//! no history, no addresses, no names.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use subtle::ConstantTimeEq;

use crate::ids::{self, HandoverId, Hash, MailboxId, ReaderId, Token};
use crate::limits::{
    HANDOVER_TTL_SECS, MAILBOX_IDLE_DAYS, MAX_DERIVED_MAILBOXES, MAX_DERIVED_MAILBOXES_EVER,
    MAX_GUEST_HANDOVERS, MAX_HANDOVERS, MAX_HANDOVERS_PER_FAMILY, MAX_HANDOVERS_PER_TOKEN,
    MAX_READERS_PER_MAILBOX, MAX_WAITING_MESSAGES, MESSAGE_TTL_SECS, REVEAL_BYTES,
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

/// Version 1 to 2 (M9): voucher families.
///
/// `derived_total` counts the mailboxes a voucher mailbox has derived in its
/// lifetime, so that deleting and creating them again cannot go on forever;
/// it starts at the number of derived mailboxes that exist.
///
/// `family` of a handover slot is the voucher mailbox of the voucher family
/// whose read token authorized it, and NULL for the access token and read
/// tokens of mailboxes created with it. A reveal slot carries the family of
/// the slot that reserved it. Slots from before the migration expire within
/// 10 minutes and count as the access token's.
const MIGRATION_V2: &str = "
ALTER TABLE mailboxes ADD COLUMN derived_total INTEGER NOT NULL DEFAULT 0;
UPDATE mailboxes SET derived_total =
    (SELECT COUNT(*) FROM mailboxes AS child WHERE child.parent = mailboxes.id)
    WHERE origin = 1;
ALTER TABLE handovers ADD COLUMN family BLOB;
CREATE INDEX handovers_family ON handovers(family);
";

/// Version 2 to 3 (M10): a read token per reader.
///
/// `mailboxes.read_token_hash` becomes the shared token of all legacy
/// readers and may be NULL: dropped, or never set for a mailbox created in
/// the new form. SQLite cannot drop `NOT NULL` from a column, so the table is
/// rebuilt; [`migrate`] runs with foreign keys off, so that dropping the old
/// table does not cascade to readers and messages. Existing mailboxes keep
/// their read token as the shared token.
///
/// `readers.token_hash` is the hash of the reader's own token, NULL for a
/// legacy reader that subscribes with the shared token; `role` is 1 for a
/// full reader and 2 for a satellite. Existing readers become legacy full
/// readers.
const MIGRATION_V3: &str = "
CREATE TABLE mailboxes_v3 (
    id              BLOB PRIMARY KEY,
    read_token_hash BLOB,
    send_token_hash BLOB NOT NULL,
    next_seq        INTEGER NOT NULL,
    last_used_day   INTEGER NOT NULL,
    origin          INTEGER NOT NULL DEFAULT 1,
    parent          BLOB,
    derived_total   INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;
INSERT INTO mailboxes_v3
    (id, read_token_hash, send_token_hash, next_seq, last_used_day, origin, parent, derived_total)
    SELECT id, read_token_hash, send_token_hash, next_seq, last_used_day, origin, parent,
           derived_total
    FROM mailboxes;
DROP TABLE mailboxes;
ALTER TABLE mailboxes_v3 RENAME TO mailboxes;
CREATE INDEX mailboxes_read_token_hash ON mailboxes(read_token_hash);
CREATE INDEX mailboxes_parent ON mailboxes(parent);
ALTER TABLE readers ADD COLUMN token_hash BLOB;
ALTER TABLE readers ADD COLUMN role INTEGER NOT NULL DEFAULT 1;
CREATE INDEX readers_token_hash ON readers(token_hash);
";

/// The schema version this code works with.
pub const SCHEMA_VERSION: i64 = 3;

/// Day number used for "day of last use": whole days since the Unix epoch.
pub fn day_of(now: i64) -> i64 {
    now.div_euclid(86_400)
}

/// Role of a reader with a token of its own, as stored in `readers.role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A full device: may manage the mailbox and its readers.
    Full = 1,
    /// A satellite: may only read, and change its own push URL and token.
    Satellite = 2,
}

impl Role {
    /// Any value but that of a full reader counts as a satellite, so that an
    /// unexpected value never grants more.
    fn from_db(value: i64) -> Self {
        if value == Self::Full as i64 {
            Self::Full
        } else {
            Self::Satellite
        }
    }
}

/// Who a bearer token for a mailbox belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Principal {
    /// The shared token of the mailbox, held by legacy readers.
    Shared,
    /// The own token of this reader.
    Reader(ReaderId, Role),
}

impl Principal {
    /// May register, change and revoke other readers and delete the
    /// mailbox.
    fn manages(self) -> bool {
        !matches!(self, Self::Reader(_, Role::Satellite))
    }

    /// May drop the shared token and set the send token.
    fn is_full_reader(self) -> bool {
        matches!(self, Self::Reader(_, Role::Full))
    }
}

/// How a mailbox is read, for [`Store::create_mailbox`].
#[derive(Debug, Clone, Copy)]
pub enum ReadAccess {
    /// A shared read token with this hash (the old form).
    Shared(Hash),
    /// No shared token; this reader is the first full reader, with a token
    /// of its own with this hash.
    Reader(ReaderId, Hash),
}

/// A mailbox to create.
#[derive(Debug, Clone, Copy)]
pub struct NewMailbox {
    /// How it is read.
    pub read: ReadAccess,
    /// `SHA-256` of the send token.
    pub send_token_hash: Hash,
}

/// A waiting message: its sequence number and content.
pub type Stored = (i64, Vec<u8>);

/// Outcome of a call that needs a token of the mailbox.
#[derive(Debug, PartialEq, Eq)]
pub enum Access<T> {
    /// Done.
    Ok(T),
    /// The mailbox does not exist.
    UnknownMailbox,
    /// The reader is not registered.
    UnknownReader,
    /// The token does not match, or does not have the right.
    Unauthorized,
}

/// A change to a reader with `PUT`. `None` leaves a field as it is.
#[derive(Debug, Clone, Default)]
pub struct ReaderChange {
    /// The push URL, or `Some(None)` to remove it.
    pub push_url: Option<Option<String>>,
    /// `SHA-256` of the reader's own token.
    pub token_hash: Option<Hash>,
    /// The role.
    pub role: Option<Role>,
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

/// Who a token that authorizes a handover slot belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// The access token, or the read token of a mailbox created with it.
    Owner,
    /// The read token of a voucher mailbox or of a mailbox derived from it;
    /// the identifier of the voucher mailbox.
    Voucher(MailboxId),
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
    /// The voucher mailbox has the maximum number of derived mailboxes, or
    /// has derived the maximum in its lifetime.
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
    /// The family of the token that authorized the `PUT`.
    pub family: Family,
}

/// Result of storing a handover slot.
#[derive(Debug, PartialEq, Eq)]
pub enum PutHandover {
    /// Stored, with the reveal slot reserved if asked for.
    Stored,
    /// The identifier, or its reveal identifier, is taken or reserved.
    Exists,
    /// The authorizing token or its voucher family already has the maximum
    /// of waiting slots.
    Quota,
    /// The relay, or all voucher families together, already have the maximum
    /// of slots.
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
    /// The reader is registered. `token_changed` if an existing reader got
    /// a new token, so that live subscriptions must be checked again.
    Ok {
        /// The reader had another token before.
        token_changed: bool,
    },
    /// The mailbox does not exist.
    UnknownMailbox,
    /// The token does not match, or does not have the right.
    Unauthorized,
    /// The mailbox already has the maximum number of readers.
    TooManyReaders,
    /// A new reader without a token on a mailbox without shared token, or a
    /// token hash that the shared token or another reader already has.
    BadRequest,
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
        conn.pragma_update(None, "secure_delete", "ON")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Foreign keys are off while migrating: version 3 rebuilds the
        // mailboxes table, and dropping it must not cascade. The pragma has
        // no effect inside a transaction, so it is set around it.
        conn.pragma_update(None, "foreign_keys", "OFF")?;
        migrate(&mut conn)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
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
    /// first, which is then redeemed, and else as the shared token or a full
    /// reader's token of a voucher mailbox, whose derived mailboxes are
    /// counted; all in one transaction. With [`ReadAccess::Reader`] the
    /// mailbox has no shared token and the reader is registered as its first
    /// full reader.
    pub fn create_mailbox(
        &self,
        id: &MailboxId,
        mailbox: &NewMailbox,
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
                                "SELECT id FROM mailboxes WHERE origin = ?2 AND (
                                     read_token_hash = ?1
                                     OR id IN (SELECT mailbox FROM readers
                                               WHERE token_hash = ?1 AND role = ?3))
                                 ORDER BY id LIMIT 1",
                                params![&hash[..], Origin::Voucher as i64, Role::Full as i64],
                                |row| row.get::<_, Vec<u8>>(0),
                            )
                            .optional()?
                        else {
                            return Ok(CreateMailbox::Unauthorized);
                        };
                        let (derived, total): (i64, i64) = tx.query_row(
                            "SELECT (SELECT COUNT(*) FROM mailboxes WHERE parent = ?1),
                                    derived_total
                             FROM mailboxes WHERE id = ?1",
                            params![&parent],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )?;
                        if derived >= MAX_DERIVED_MAILBOXES as i64
                            || total >= MAX_DERIVED_MAILBOXES_EVER as i64
                        {
                            return Ok(CreateMailbox::Quota);
                        }
                        tx.execute(
                            "UPDATE mailboxes SET derived_total = derived_total + 1 WHERE id = ?1",
                            params![&parent],
                        )?;
                        (Origin::Derived, Some(parent))
                    }
                }
            };
            let shared = match &mailbox.read {
                ReadAccess::Shared(hash) => Some(&hash[..]),
                ReadAccess::Reader(..) => None,
            };
            tx.execute(
                "INSERT INTO mailboxes
                   (id, read_token_hash, send_token_hash, next_seq, last_used_day, origin, parent)
                 VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6)",
                params![
                    &id[..],
                    shared,
                    &mailbox.send_token_hash[..],
                    day_of(now),
                    origin as i64,
                    parent
                ],
            )?;
            if let ReadAccess::Reader(reader, token_hash) = &mailbox.read {
                tx.execute(
                    "INSERT INTO readers (mailbox, id, push_url, last_ack, token_hash, role)
                     VALUES (?1, ?2, NULL, 0, ?3, ?4)",
                    params![&id[..], &reader[..], &token_hash[..], Role::Full as i64],
                )?;
            }
            tx.commit()?;
            Ok(CreateMailbox::Created(origin))
        })
    }

    /// The family of the token with this hash, if it is the shared token or
    /// a full reader's token of some mailbox; `None` otherwise, also for a
    /// satellite's token. If several mailboxes match, a voucher family wins
    /// over the access token's, so that the stricter limits apply.
    pub fn token_family(&self, hash: &Hash) -> rusqlite::Result<Option<Family>> {
        self.with(|c| {
            let row = c
                .query_row(
                    "SELECT CASE origin WHEN ?2 THEN NULL WHEN ?3 THEN id
                            ELSE COALESCE(parent, id) END
                     FROM mailboxes
                     WHERE read_token_hash = ?1
                        OR id IN (SELECT mailbox FROM readers
                                  WHERE token_hash = ?1 AND role = ?4)
                     ORDER BY origin = ?2, id LIMIT 1",
                    params![
                        &hash[..],
                        Origin::AccessToken as i64,
                        Origin::Voucher as i64,
                        Role::Full as i64
                    ],
                    |row| row.get::<_, Option<Vec<u8>>>(0),
                )
                .optional()?;
            Ok(row.map(|family| match family {
                None => Family::Owner,
                // Identifiers are written with their fixed length; anything
                // else would be a voucher family that matches no other.
                Some(id) => {
                    Family::Voucher(id.try_into().unwrap_or([0xff; lz_wire::MAILBOX_ID_LEN]))
                }
            }))
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
            let needed = 1 + i64::from(reveal.is_some());
            let family = match &slot.family {
                Family::Owner => None,
                Family::Voucher(id) => Some(&id[..]),
            };
            if let Some(family) = family {
                let per_family: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM handovers WHERE family = ?1 AND reveal = 0",
                    params![family],
                    |row| row.get(0),
                )?;
                if per_family >= MAX_HANDOVERS_PER_FAMILY as i64 {
                    return Ok(PutHandover::Quota);
                }
                let guests: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM handovers WHERE family IS NOT NULL",
                    [],
                    |row| row.get(0),
                )?;
                if guests + needed > MAX_GUEST_HANDOVERS as i64 {
                    return Ok(PutHandover::Full);
                }
            }
            let total: i64 =
                tx.query_row("SELECT COUNT(*) FROM handovers", [], |row| row.get(0))?;
            if total + needed > MAX_HANDOVERS as i64 {
                return Ok(PutHandover::Full);
            }
            let expires_at = now + HANDOVER_TTL_SECS;
            tx.execute(
                "INSERT INTO handovers
                   (id, data, fetch_hash, auth_hash, expires_at, reveal, family)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
                params![
                    &id[..],
                    slot.data,
                    &slot.fetch_hash[..],
                    &slot.auth_hash[..],
                    expires_at,
                    family
                ],
            )?;
            if let Some((reveal_id, reply_hash)) = reveal {
                tx.execute(
                    "INSERT INTO handovers
                       (id, data, fetch_hash, auth_hash, expires_at, reveal, family)
                     VALUES (?1, NULL, ?2, NULL, ?3, 1, ?4)",
                    params![&reveal_id[..], &reply_hash[..], expires_at, family],
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

    /// The hash of the send token of a mailbox, or `None` if it does not
    /// exist.
    pub fn send_token_hash(&self, id: &MailboxId) -> rusqlite::Result<Option<Hash>> {
        self.with(|c| {
            let row = c
                .query_row(
                    "SELECT send_token_hash FROM mailboxes WHERE id = ?1",
                    params![&id[..]],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()?;
            // A stored hash of another length matches no token.
            Ok(row.map(|hash| hash.try_into().unwrap_or([0; 32])))
        })
    }

    /// Who the token with this hash belongs to on `mailbox`.
    pub fn principal(
        &self,
        mailbox: &MailboxId,
        hash: &Hash,
    ) -> rusqlite::Result<Access<Principal>> {
        self.with(|c| {
            Ok(match Credentials::load(c, mailbox)? {
                None => Access::UnknownMailbox,
                Some(creds) => match creds.principal(hash) {
                    Some(principal) => Access::Ok(principal),
                    None => Access::Unauthorized,
                },
            })
        })
    }

    /// Deletes a mailbox with its readers and messages, with the shared
    /// token or a full reader's token.
    pub fn delete_mailbox(&self, id: &MailboxId, hash: &Hash) -> rusqlite::Result<Access<()>> {
        self.managed(id, hash, Principal::manages, |tx| {
            tx.execute("DELETE FROM mailboxes WHERE id = ?1", params![&id[..]])?;
            Ok(())
        })
    }

    /// Drops the shared token of a mailbox, with a full reader's token.
    /// Legacy readers can no longer subscribe. Done also if there was none.
    pub fn drop_shared_token(&self, id: &MailboxId, hash: &Hash) -> rusqlite::Result<Access<()>> {
        self.managed(id, hash, Principal::is_full_reader, |tx| {
            tx.execute(
                "UPDATE mailboxes SET read_token_hash = NULL WHERE id = ?1",
                params![&id[..]],
            )?;
            Ok(())
        })
    }

    /// Replaces the send token of a mailbox, with a full reader's token.
    pub fn set_send_token(
        &self,
        id: &MailboxId,
        hash: &Hash,
        send_token_hash: &Hash,
    ) -> rusqlite::Result<Access<()>> {
        self.managed(id, hash, Principal::is_full_reader, |tx| {
            tx.execute(
                "UPDATE mailboxes SET send_token_hash = ?2 WHERE id = ?1",
                params![&id[..], &send_token_hash[..]],
            )?;
            Ok(())
        })
    }

    /// Runs `action` in one transaction if the token with this hash belongs
    /// to a principal for which `allowed` holds.
    fn managed(
        &self,
        id: &MailboxId,
        hash: &Hash,
        allowed: fn(Principal) -> bool,
        action: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<()>,
    ) -> rusqlite::Result<Access<()>> {
        self.with(|c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let Some(creds) = Credentials::load(&tx, id)? else {
                return Ok(Access::UnknownMailbox);
            };
            if !creds.principal(hash).is_some_and(allowed) {
                return Ok(Access::Unauthorized);
            }
            action(&tx)?;
            tx.commit()?;
            Ok(Access::Ok(()))
        })
    }

    /// Registers a reader or changes it, authorized by the token with hash
    /// `hash`; all in one transaction. The shared token and full readers may
    /// register and change any reader; a satellite may change only its own
    /// push URL and token. A new reader starts with acknowledgement 0, as a
    /// full reader unless the role is given, and needs a token of its own
    /// if the mailbox has no shared token.
    pub fn put_reader(
        &self,
        mailbox: &MailboxId,
        reader: &ReaderId,
        hash: &Hash,
        change: &ReaderChange,
        now: i64,
    ) -> rusqlite::Result<PutReader> {
        self.with(|c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let Some(creds) = Credentials::load(&tx, mailbox)? else {
                return Ok(PutReader::UnknownMailbox);
            };
            let Some(principal) = creds.principal(hash) else {
                return Ok(PutReader::Unauthorized);
            };
            if let Principal::Reader(own, Role::Satellite) = principal
                && (own != *reader || change.role.is_some())
            {
                return Ok(PutReader::Unauthorized);
            }
            if let Some(new) = &change.token_hash
                && creds.hash_taken(new, reader)
            {
                return Ok(PutReader::BadRequest);
            }
            tx.execute(
                "UPDATE mailboxes SET last_used_day = ?2 WHERE id = ?1",
                params![&mailbox[..], day_of(now)],
            )?;
            let token_changed = match creds.reader(reader) {
                Some(existing) => {
                    if let Some(push_url) = &change.push_url {
                        tx.execute(
                            "UPDATE readers SET push_url = ?3 WHERE mailbox = ?1 AND id = ?2",
                            params![&mailbox[..], &reader[..], push_url],
                        )?;
                    }
                    if let Some(role) = change.role {
                        tx.execute(
                            "UPDATE readers SET role = ?3 WHERE mailbox = ?1 AND id = ?2",
                            params![&mailbox[..], &reader[..], role as i64],
                        )?;
                    }
                    match &change.token_hash {
                        Some(new) if existing.key != Key::Own(*new) => {
                            tx.execute(
                                "UPDATE readers SET token_hash = ?3 WHERE mailbox = ?1 AND id = ?2",
                                params![&mailbox[..], &reader[..], &new[..]],
                            )?;
                            true
                        }
                        _ => false,
                    }
                }
                None => {
                    if creds.shared.is_none() && change.token_hash.is_none() {
                        return Ok(PutReader::BadRequest);
                    }
                    if creds.readers.len() >= MAX_READERS_PER_MAILBOX {
                        return Ok(PutReader::TooManyReaders);
                    }
                    tx.execute(
                        "INSERT INTO readers (mailbox, id, push_url, last_ack, token_hash, role)
                         VALUES (?1, ?2, ?3, 0, ?4, ?5)",
                        params![
                            &mailbox[..],
                            &reader[..],
                            change.push_url.clone().flatten(),
                            change.token_hash.as_ref().map(|h| &h[..]),
                            change.role.unwrap_or(Role::Full) as i64
                        ],
                    )?;
                    false
                }
            };
            tx.commit()?;
            Ok(PutReader::Ok { token_changed })
        })
    }

    /// Revokes every reader of the mailbox except those in `keep` and the
    /// caller, which is always kept; with `drop_shared` also drops the shared
    /// token; all in one transaction. Only a full reader's own token may do
    /// this. Returns the revoked readers. Messages that all remaining readers
    /// have acknowledged are deleted.
    pub fn revoke_readers(
        &self,
        mailbox: &MailboxId,
        hash: &Hash,
        keep: &[ReaderId],
        drop_shared: bool,
    ) -> rusqlite::Result<Access<Vec<ReaderId>>> {
        self.with(|c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let Some(creds) = Credentials::load(&tx, mailbox)? else {
                return Ok(Access::UnknownMailbox);
            };
            let Some(Principal::Reader(caller, Role::Full)) = creds.principal(hash) else {
                return Ok(Access::Unauthorized);
            };
            let revoked: Vec<ReaderId> = creds
                .readers
                .iter()
                .map(|r| r.id)
                .filter(|id| *id != caller && !keep.contains(id))
                .collect();
            for id in &revoked {
                tx.execute(
                    "DELETE FROM readers WHERE mailbox = ?1 AND id = ?2",
                    params![&mailbox[..], &id[..]],
                )?;
            }
            if drop_shared {
                tx.execute(
                    "UPDATE mailboxes SET read_token_hash = NULL WHERE id = ?1",
                    params![&mailbox[..]],
                )?;
            }
            purge_acknowledged(&tx, mailbox)?;
            tx.commit()?;
            Ok(Access::Ok(revoked))
        })
    }

    /// Removes a reader, authorized by the token with hash `hash`: the
    /// shared token and full readers may remove any reader, a satellite
    /// only itself. Messages that all remaining readers have acknowledged
    /// are deleted.
    pub fn delete_reader(
        &self,
        mailbox: &MailboxId,
        reader: &ReaderId,
        hash: &Hash,
    ) -> rusqlite::Result<Access<()>> {
        self.with(|c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let Some(creds) = Credentials::load(&tx, mailbox)? else {
                return Ok(Access::UnknownMailbox);
            };
            match creds.principal(hash) {
                None => return Ok(Access::Unauthorized),
                Some(Principal::Reader(own, Role::Satellite)) if own != *reader => {
                    return Ok(Access::Unauthorized);
                }
                Some(_) => {}
            }
            let removed = tx.execute(
                "DELETE FROM readers WHERE mailbox = ?1 AND id = ?2",
                params![&mailbox[..], &reader[..]],
            )?;
            if removed == 0 {
                return Ok(Access::UnknownReader);
            }
            purge_acknowledged(&tx, mailbox)?;
            tx.commit()?;
            Ok(Access::Ok(()))
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

    /// Opens a subscription of `reader` with the token with hash `hash`:
    /// returns the reader's last acknowledgement and marks the mailbox as
    /// used today. Checked in this order: the mailbox, the reader, the token,
    /// so that a revoked reader learns that it is gone.
    pub fn open_subscription(
        &self,
        mailbox: &MailboxId,
        reader: &ReaderId,
        hash: &Hash,
        now: i64,
    ) -> rusqlite::Result<Access<i64>> {
        self.with(|c| {
            let access = reader_access(c, mailbox, reader, hash)?;
            if let Access::Ok(_) = access {
                c.execute(
                    "UPDATE mailboxes SET last_used_day = ?2 WHERE id = ?1",
                    params![&mailbox[..], day_of(now)],
                )?;
            }
            Ok(access)
        })
    }

    /// Whether a subscription of `reader` with the token with hash `hash`
    /// is still valid.
    pub fn check_subscription(
        &self,
        mailbox: &MailboxId,
        reader: &ReaderId,
        hash: &Hash,
    ) -> rusqlite::Result<Access<()>> {
        self.with(|c| Ok(map_access(reader_access(c, mailbox, reader, hash)?, |_| ())))
    }

    /// The waiting messages with a sequence number above `after`, in order,
    /// if the subscription of `reader` with the token with hash `hash` is
    /// still valid.
    pub fn messages_for(
        &self,
        mailbox: &MailboxId,
        reader: &ReaderId,
        hash: &Hash,
        after: i64,
    ) -> rusqlite::Result<Access<Vec<Stored>>> {
        self.with(|c| {
            if let Some(denied) = denied(reader_access(c, mailbox, reader, hash)?) {
                return Ok(denied);
            }
            let mut stmt = c.prepare(
                "SELECT seq, data FROM messages WHERE mailbox = ?1 AND seq > ?2 ORDER BY seq",
            )?;
            let rows = stmt.query_map(params![&mailbox[..], after], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
            Ok(Access::Ok(rows.collect::<rusqlite::Result<_>>()?))
        })
    }

    /// Records a cumulative acknowledgement up to `seq` of `reader`, whose
    /// subscription used the token with hash `hash`, and deletes messages
    /// that every reader of the mailbox has acknowledged. An acknowledgement
    /// beyond the last assigned sequence number is capped at that number.
    pub fn ack(
        &self,
        mailbox: &MailboxId,
        reader: &ReaderId,
        hash: &Hash,
        seq: i64,
        now: i64,
    ) -> rusqlite::Result<Access<()>> {
        self.with(|c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(denied) = denied(reader_access(&tx, mailbox, reader, hash)?) {
                return Ok(denied);
            }
            tx.execute(
                "UPDATE readers SET last_ack = MAX(last_ack, MIN(?3,
                    (SELECT next_seq - 1 FROM mailboxes WHERE id = ?1)))
                 WHERE mailbox = ?1 AND id = ?2",
                params![&mailbox[..], &reader[..], seq],
            )?;
            tx.execute(
                "UPDATE mailboxes SET last_used_day = ?2 WHERE id = ?1",
                params![&mailbox[..], day_of(now)],
            )?;
            purge_acknowledged(&tx, mailbox)?;
            tx.commit()?;
            Ok(Access::Ok(()))
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
                "schema version {version} is newer than {SCHEMA_VERSION}"
            )),
        ));
    }
    if version < 1 {
        tx.execute_batch(SCHEMA_V0)?;
        tx.execute_batch(MIGRATION_V1)?;
    }
    if version < 2 {
        tx.execute_batch(MIGRATION_V2)?;
    }
    if version < 3 {
        tx.execute_batch(MIGRATION_V3)?;
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

/// How a reader authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    /// A legacy reader: with the shared token.
    Legacy,
    /// With its own token, of this hash.
    Own(Hash),
    /// A stored hash of another length; matches nothing.
    Broken,
}

impl Key {
    fn from_db(value: Option<Vec<u8>>) -> Self {
        match value {
            None => Self::Legacy,
            Some(bytes) => Hash::try_from(bytes).map_or(Self::Broken, Self::Own),
        }
    }
}

/// A registered reader, as far as authorization needs it.
#[derive(Debug, Clone, Copy)]
struct ReaderRow {
    id: ReaderId,
    key: Key,
    role: Role,
    last_ack: i64,
}

/// The shared token and the readers of a mailbox.
struct Credentials {
    shared: Option<Hash>,
    readers: Vec<ReaderRow>,
}

impl Credentials {
    /// `None` if the mailbox does not exist.
    fn load(conn: &Connection, mailbox: &MailboxId) -> rusqlite::Result<Option<Self>> {
        let Some(shared) = conn
            .query_row(
                "SELECT read_token_hash FROM mailboxes WHERE id = ?1",
                params![&mailbox[..]],
                |row| row.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?
        else {
            return Ok(None);
        };
        let mut stmt =
            conn.prepare("SELECT id, token_hash, role, last_ack FROM readers WHERE mailbox = ?1")?;
        let rows = stmt.query_map(params![&mailbox[..]], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Option<Vec<u8>>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut readers = Vec::new();
        for row in rows {
            let (id, key, role, last_ack) = row?;
            if let Ok(id) = ReaderId::try_from(id) {
                readers.push(ReaderRow {
                    id,
                    key: Key::from_db(key),
                    role: Role::from_db(role),
                    last_ack,
                });
            }
        }
        Ok(Some(Self {
            // A stored hash of another length is no shared token.
            shared: shared.and_then(|hash| Hash::try_from(hash).ok()),
            readers,
        }))
    }

    /// Who the token with this hash belongs to. Every hash is compared, in
    /// constant time each; a reader's own token wins, though
    /// [`Credentials::hash_taken`] keeps hashes apart.
    fn principal(&self, hash: &Hash) -> Option<Principal> {
        let mut found = None;
        for reader in &self.readers {
            if let Key::Own(own) = &reader.key
                && bool::from(own.ct_eq(hash))
            {
                found = Some(Principal::Reader(reader.id, reader.role));
            }
        }
        let shared = self
            .shared
            .is_some_and(|shared| bool::from(shared.ct_eq(hash)));
        found.or(shared.then_some(Principal::Shared))
    }

    fn reader(&self, id: &ReaderId) -> Option<&ReaderRow> {
        self.readers.iter().find(|r| r.id == *id)
    }

    /// Whether the shared token or a reader other than `reader` has this
    /// hash already.
    fn hash_taken(&self, hash: &Hash, reader: &ReaderId) -> bool {
        self.shared == Some(*hash)
            || self
                .readers
                .iter()
                .any(|r| r.id != *reader && r.key == Key::Own(*hash))
    }
}

/// Checks a subscription of `reader` with the token with hash `hash`, in the
/// order mailbox, reader, token; returns the last acknowledgement. A reader
/// with a token of its own subscribes only with it, a legacy reader only with
/// the shared token.
fn reader_access(
    conn: &Connection,
    mailbox: &MailboxId,
    reader: &ReaderId,
    hash: &Hash,
) -> rusqlite::Result<Access<i64>> {
    let Some(creds) = Credentials::load(conn, mailbox)? else {
        return Ok(Access::UnknownMailbox);
    };
    let Some(row) = creds.reader(reader) else {
        return Ok(Access::UnknownReader);
    };
    let matches = match &row.key {
        Key::Legacy => creds
            .shared
            .is_some_and(|shared| bool::from(shared.ct_eq(hash))),
        Key::Own(own) => own.ct_eq(hash).into(),
        Key::Broken => false,
    };
    Ok(if matches {
        Access::Ok(row.last_ack)
    } else {
        Access::Unauthorized
    })
}

fn map_access<T, U>(access: Access<T>, f: impl FnOnce(T) -> U) -> Access<U> {
    match access {
        Access::Ok(value) => Access::Ok(f(value)),
        Access::UnknownMailbox => Access::UnknownMailbox,
        Access::UnknownReader => Access::UnknownReader,
        Access::Unauthorized => Access::Unauthorized,
    }
}

/// The refusal in `access`, if it is one.
fn denied<T, U>(access: Access<T>) -> Option<Access<U>> {
    match access {
        Access::Ok(_) => None,
        other => Some(map_access(other, |_| unreachable!())),
    }
}

/// Checks a presented token against a stored hash of unchecked length.
fn hash_matches(token: &Token, stored: &[u8]) -> bool {
    Hash::try_from(stored).is_ok_and(|hash| ids::token_matches(token, &hash))
}
