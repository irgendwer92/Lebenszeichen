//! Contact record: a newly paired contact as sent to the other own devices,
//! split into `contact_part` messages.

use crate::{
    ContactName, Error, MAX_CONTACT_NAME_LEN, MAX_PART_LEN, MAX_RELAY_URL_LEN, MailboxId,
    MessageId, PublicKey, Reader, RelayUrl, Rules, Token, Writer,
};

/// Longest record in bytes, with both longest relay addresses and name.
pub const MAX_LEN: usize = 3 * 32
    + 1
    + MAX_RELAY_URL_LEN
    + 2 * 32
    + 1
    + MAX_RELAY_URL_LEN
    + 2 * (10 + 8 + 16)
    + 1
    + MAX_CONTACT_NAME_LEN;

/// Most parts a record of at most [`MAX_LEN`] bytes is split into.
pub const MAX_PARTS: usize = MAX_LEN.div_ceil(MAX_PART_LEN);

const _: () = assert!(MAX_LEN == 535);

/// Sending time and message identifier of the last applied rules.
///
/// Later wins; for the same time the larger identifier, bytewise. The
/// derived order is exactly that. All zeros means none were applied yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stamp {
    pub at: u64,
    pub id: MessageId,
}

/// What another own device cannot derive about a newly paired contact.
///
/// The own mailbox identifier is not part of it: it is the contact
/// identifier of the `contact_part` messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactRecord {
    /// Secret contact key of this side.
    pub secret_key: [u8; 32],
    /// Public contact key of the contact.
    pub peer_key: PublicKey,
    /// Read token of the own mailbox.
    pub read_token: Token,
    pub own_relay: RelayUrl,
    pub peer_mailbox: MailboxId,
    pub peer_send_token: Token,
    pub peer_relay: RelayUrl,
    pub own_rules: Rules,
    pub own_rules_stamp: Stamp,
    pub peer_rules: Rules,
    pub peer_rules_stamp: Stamp,
    /// Local name of the contact.
    pub name: ContactName,
}

impl ContactRecord {
    /// Writes the binary form and returns its length.
    pub fn to_bytes(&self, out: &mut [u8; MAX_LEN]) -> usize {
        let mut w = Writer::new(out);
        self.write(&mut w).expect("MAX_LEN covers every record");
        w.position()
    }

    fn write(&self, w: &mut Writer<'_>) -> Result<(), Error> {
        w.put(&self.secret_key)?;
        w.put(&self.peer_key)?;
        w.put(&self.read_token)?;
        w.text(&self.own_relay)?;
        w.put(&self.peer_mailbox)?;
        w.put(&self.peer_send_token)?;
        w.text(&self.peer_relay)?;
        self.own_rules.write(w)?;
        self.own_rules_stamp.write(w)?;
        self.peer_rules.write(w)?;
        self.peer_rules_stamp.write(w)?;
        w.text(&self.name)
    }

    /// Reads the binary form, put together from all parts. Trailing bytes
    /// are ignored, so later versions can append fields.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(bytes);
        Ok(Self {
            secret_key: r.array()?,
            peer_key: r.array()?,
            read_token: r.array()?,
            own_relay: r.text()?,
            peer_mailbox: r.array()?,
            peer_send_token: r.array()?,
            peer_relay: r.text()?,
            own_rules: Rules::read(&mut r)?,
            own_rules_stamp: Stamp::read(&mut r)?,
            peer_rules: Rules::read(&mut r)?,
            peer_rules_stamp: Stamp::read(&mut r)?,
            name: r.text()?,
        })
    }
}

impl Stamp {
    pub(crate) fn write(&self, w: &mut Writer<'_>) -> Result<(), Error> {
        w.put(&self.at.to_be_bytes())?;
        w.put(&self.id)
    }

    pub(crate) fn read(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Self {
            at: r.u64()?,
            id: r.array()?,
        })
    }
}

/// Splits an encoded record into the data of its parts, in order.
pub fn split(record: &[u8]) -> core::slice::Chunks<'_, u8> {
    record.chunks(MAX_PART_LEN)
}

#[cfg(test)]
mod tests {
    use super::{ContactRecord, MAX_LEN, MAX_PARTS, Stamp, split};
    use crate::{ContactName, Error, MAX_RELAY_URL_LEN, RelayUrl, Rules};

    fn record(relay: &str, name: &str) -> ContactRecord {
        ContactRecord {
            secret_key: [1; 32],
            peer_key: [2; 32],
            read_token: [3; 32],
            own_relay: RelayUrl::new(relay).unwrap(),
            peer_mailbox: [4; 32],
            peer_send_token: [5; 32],
            peer_relay: RelayUrl::new(relay).unwrap(),
            own_rules: Rules::DEFAULT,
            own_rules_stamp: Stamp {
                at: 0x0102_0304_0506_0708,
                id: [6; 16],
            },
            peer_rules: Rules {
                min_interval_minutes: 1,
                expiry_minutes: 2,
                weekly_limit: 3,
            },
            peer_rules_stamp: Stamp::default(),
            name: ContactName::new(name).unwrap(),
        }
    }

    #[test]
    fn layout_matches_documentation() {
        let mut buf = [0; MAX_LEN];
        let len = record("https://r.example", "Anna").to_bytes(&mut buf);
        assert_eq!(len, 3 * 32 + 18 + 2 * 32 + 18 + 2 * 34 + 5);
        assert_eq!(&buf[..32], &[1; 32]);
        assert_eq!(&buf[32..64], &[2; 32]);
        assert_eq!(&buf[64..96], &[3; 32]);
        assert_eq!(buf[96], 17);
        assert_eq!(&buf[97..114], b"https://r.example");
        assert_eq!(&buf[114..146], &[4; 32]);
        assert_eq!(&buf[146..178], &[5; 32]);
        assert_eq!(buf[178], 17);
        assert_eq!(&buf[196..206], &[0, 0, 1, 104, 0, 0, 5, 160, 0, 7]);
        assert_eq!(&buf[206..214], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&buf[214..230], &[6; 16]);
        assert_eq!(&buf[230..240], &[0, 0, 0, 1, 0, 0, 0, 2, 0, 3]);
        assert_eq!(&buf[240..264], &[0; 24]);
        assert_eq!(buf[264], 4);
        assert_eq!(&buf[265..269], b"Anna");
        assert_eq!(
            ContactRecord::from_bytes(&buf[..len]).unwrap(),
            record("https://r.example", "Anna")
        );
    }

    #[test]
    fn longest_record_has_535_bytes_in_four_parts() {
        let longest = record(&"r".repeat(MAX_RELAY_URL_LEN), &"n".repeat(64));
        let mut buf = [0; MAX_LEN];
        assert_eq!(longest.to_bytes(&mut buf), 535);
        assert_eq!(MAX_PARTS, 4);
        let mut lens = [0; MAX_PARTS + 1];
        let mut joined = [0; MAX_LEN];
        let mut at = 0;
        for (i, part) in split(&buf).enumerate() {
            lens[i] = part.len();
            joined[at..at + part.len()].copy_from_slice(part);
            at += part.len();
        }
        assert_eq!(lens, [178, 178, 178, 1, 0]);
        assert_eq!(joined, buf);
        assert_eq!(ContactRecord::from_bytes(&buf).unwrap(), longest);
    }

    #[test]
    fn shortest_record_needs_two_parts() {
        let mut buf = [0; MAX_LEN];
        let len = record("", "").to_bytes(&mut buf);
        assert_eq!(len, 231);
        assert_eq!(split(&buf[..len]).count(), 2);
    }

    #[test]
    fn incomplete_record_is_rejected_trailing_bytes_ignored() {
        let mut buf = [0; MAX_LEN];
        let len = record("https://r.example", "Anna").to_bytes(&mut buf);
        assert_eq!(
            ContactRecord::from_bytes(&buf[..len - 1]),
            Err(Error::Truncated)
        );
        assert!(ContactRecord::from_bytes(&buf[..len + 3]).is_ok());
        buf[96] = 121;
        assert_eq!(ContactRecord::from_bytes(&buf), Err(Error::TooLong));
    }

    #[test]
    fn stamps_order_by_time_then_identifier() {
        let stamp = |at, id| Stamp { at, id: [id; 16] };
        assert!(stamp(2, 0) > stamp(1, 9));
        assert!(stamp(1, 2) > stamp(1, 1));
        assert!(stamp(1, 1) == stamp(1, 1));
        let mut id = [0; 16];
        id[15] = 1;
        assert!(Stamp { at: 1, id } < stamp(1, 1));
        assert_eq!(Stamp::default(), stamp(0, 0));
    }
}
