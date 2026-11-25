//! The satellite (M10, `docs/m10-satellite.md`, part 2): bodies of the
//! channel messages and of the sync messages about satellites, and the
//! records they carry.
//!
//! - [`ContactRecord`]: a contact as the satellite needs it, in
//!   `home_contact_part` (type 88) and in the setup.
//! - [`Record`]: the satellite as the other full devices need it, in
//!   `satellite_part` (type 73).
//! - [`SetupHeader`] and [`write_entry`]/[`Entries`]: what a full device
//!   hands the satellite at setup (over BLE later, over USB serial from
//!   `lz` for development).

use crate::{ContactName, message::PartData};
use crate::{
    DeviceName, Error, MAX_CONTACT_NAME_LEN, MAX_DEVICE_NAME_LEN, MAX_PART_LEN, MAX_REASON_LEN,
    MAX_RELAY_URL_LEN, MailboxId, MessageId, PublicKey, Reader, ReaderId, Reason, RelayUrl, Rules,
    Stamp, Token, TransferId, Writer, text::BoundedStr,
};

/// Most own requests per contact in the satellite contact record; with more
/// in a week, the oldest are left out.
pub const MAX_RECENT_REQUESTS: usize = 16;

/// Longest WiFi name in the setup, in bytes of UTF-8.
pub const MAX_WIFI_NAME_LEN: usize = 32;

/// Longest WiFi password in the setup, in bytes of UTF-8.
pub const MAX_WIFI_PASSWORD_LEN: usize = 64;

/// Version byte at the start of the setup.
pub const SETUP_VERSION: u8 = 1;

/// Name of a WiFi network.
pub type WifiName = BoundedStr<MAX_WIFI_NAME_LEN>;

/// Password of a WiFi network.
pub type WifiPassword = BoundedStr<MAX_WIFI_PASSWORD_LEN>;

/// `home_settings` (93): the satellite's own settings, set on a full
/// device. Also the second part of `satellite_settings` (74).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Settings {
    /// Minutes after a request before the satellite sends a reported
    /// answer.
    pub waiting_minutes: u16,
    /// The long-press preset; `None` on the wire as a duration of 0.
    pub preset: Option<Preset>,
}

/// The state a long press sets on the satellite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Preset {
    /// How long the state lasts, at least 1.
    pub minutes: u32,
    /// Whether the long press also shares it with the full devices.
    pub share: bool,
    pub reason: Reason,
}

impl Settings {
    /// Until the first `home_settings` arrives: waiting time 15 minutes,
    /// no preset.
    pub const DEFAULT: Self = Self {
        waiting_minutes: 15,
        preset: None,
    };

    /// Longest body: waiting time, duration, share, reason.
    pub const MAX_LEN: usize = 2 + 4 + 1 + 1 + MAX_REASON_LEN;

    pub(crate) fn write(&self, w: &mut Writer<'_>) -> Result<(), Error> {
        w.put(&self.waiting_minutes.to_be_bytes())?;
        match &self.preset {
            Some(preset) => {
                w.put(&preset.minutes.to_be_bytes())?;
                w.put(&[u8::from(preset.share)])?;
                w.text(&preset.reason)
            }
            None => w.put(&[0, 0, 0, 0, 0, 0]),
        }
    }

    pub(crate) fn read(r: &mut Reader<'_>) -> Result<Self, Error> {
        let waiting_minutes = r.u16()?;
        let minutes = r.u32()?;
        let share = flag(r.u8()?)?;
        let reason: Reason = r.text()?;
        let preset = match minutes {
            0 => None,
            minutes => Some(Preset {
                minutes,
                share,
                reason,
            }),
        };
        Ok(Self {
            waiting_minutes,
            preset,
        })
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// `home_contact_settings` (92): what the satellite does for one contact.
/// Also the second part of `satellite_contact_settings` (75).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContactSettings {
    /// Own mailbox of the contact.
    pub contact: MailboxId,
    /// Settings without the contact, as also in the contact record.
    pub of: ContactFlags,
}

/// The settings of the satellite for one contact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ContactFlags {
    /// Reported answers to this contact are allowed on the satellite.
    pub reported: bool,
    /// Number of the ask button bound to this contact, 0 for none.
    pub ask_button: u8,
}

impl ContactFlags {
    pub(crate) fn write(&self, w: &mut Writer<'_>) -> Result<(), Error> {
        w.put(&[u8::from(self.reported), self.ask_button])
    }

    pub(crate) fn read(r: &mut Reader<'_>) -> Result<Self, Error> {
        let [reported, ask_button] = r.array()?;
        Ok(Self {
            reported: flag(reported)?,
            ask_button,
        })
    }
}

/// `satellite_settings` (74): settings of one satellite, between the full
/// devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsFor {
    pub reader: ReaderId,
    pub settings: Settings,
}

/// `satellite_contact_settings` (75).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContactSettingsFor {
    pub reader: ReaderId,
    pub settings: ContactSettings,
}

/// `satellite_part` (73): one part of the [`Record`] of a satellite.
///
/// Checked by the receiver as `contact_part`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub reader: ReaderId,
    pub transfer: TransferId,
    pub index: u8,
    pub count: u8,
    pub data: PartData,
}

/// A `0`/`1` byte; anything else is not defined.
fn flag(byte: u8) -> Result<bool, Error> {
    match byte {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(Error::InvalidValue),
    }
}

/// The open own request of a contact, in the satellite contact record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenRequest {
    pub id: MessageId,
    pub at: u64,
}

/// Times of the own requests to a contact in the last 7 days, oldest
/// first, at most [`MAX_RECENT_REQUESTS`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Recent {
    len: u8,
    times: [u64; MAX_RECENT_REQUESTS],
}

impl Recent {
    /// The newest [`MAX_RECENT_REQUESTS`] of `times`, which must be sorted
    /// oldest first.
    pub fn newest(times: &[u64]) -> Self {
        let start = times.len().saturating_sub(MAX_RECENT_REQUESTS);
        let kept = &times[start..];
        let mut out = Self {
            len: kept.len() as u8,
            times: [0; MAX_RECENT_REQUESTS],
        };
        out.times[..kept.len()].copy_from_slice(kept);
        out
    }

    pub fn as_slice(&self) -> &[u64] {
        &self.times[..self.len.into()]
    }
}

impl core::fmt::Debug for Recent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_list().entries(self.as_slice()).finish()
    }
}

/// The satellite contact record: a contact as the satellite needs it.
///
/// The own mailbox identifier is not part of it: it is the contact
/// identifier of `home_contact_part`, and comes before the record in the
/// setup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactRecord {
    /// Secret contact key of this side.
    pub secret_key: [u8; 32],
    /// Public contact key of the contact.
    pub peer_key: PublicKey,
    /// Enrolment token of the satellite on the own mailbox; it replaces it
    /// with its own at once.
    pub enrolment_token: Token,
    pub own_relay: RelayUrl,
    pub peer_mailbox: MailboxId,
    pub peer_send_token: Token,
    pub peer_relay: RelayUrl,
    pub own_rules: Rules,
    pub own_rules_stamp: Stamp,
    pub peer_rules: Rules,
    pub peer_rules_stamp: Stamp,
    /// The satellite's settings for this contact; carry no stamp, see
    /// `docs/m10-satellite.md`.
    pub settings: ContactFlags,
    /// The open own request, if any; on the wire zero bytes for none.
    pub open_request: Option<OpenRequest>,
    /// Time of the contact's last manual answer to an own request.
    pub last_answer: Option<u64>,
    /// Times of the own requests in the last 7 days, the open one included.
    pub recent: Recent,
    /// Local name of the contact.
    pub name: ContactName,
}

impl ContactRecord {
    /// Longest record in bytes.
    pub const MAX_LEN: usize = 3 * 32
        + 1
        + MAX_RELAY_URL_LEN
        + 2 * 32
        + 1
        + MAX_RELAY_URL_LEN
        + 2 * (10 + 8 + 16)
        + 2
        + 16
        + 8
        + 8
        + 1
        + 8 * MAX_RECENT_REQUESTS
        + 1
        + MAX_CONTACT_NAME_LEN;

    /// Most parts of `home_contact_part` a record is split into.
    pub const MAX_PARTS: usize = Self::MAX_LEN.div_ceil(MAX_PART_LEN);

    /// Writes the binary form and returns its length.
    pub fn to_bytes(&self, out: &mut [u8; Self::MAX_LEN]) -> usize {
        let mut w = Writer::new(out);
        self.write(&mut w).expect("MAX_LEN covers every record");
        w.position()
    }

    fn write(&self, w: &mut Writer<'_>) -> Result<(), Error> {
        w.put(&self.secret_key)?;
        w.put(&self.peer_key)?;
        w.put(&self.enrolment_token)?;
        w.text(&self.own_relay)?;
        w.put(&self.peer_mailbox)?;
        w.put(&self.peer_send_token)?;
        w.text(&self.peer_relay)?;
        self.own_rules.write(w)?;
        self.own_rules_stamp.write(w)?;
        self.peer_rules.write(w)?;
        self.peer_rules_stamp.write(w)?;
        self.settings.write(w)?;
        match &self.open_request {
            Some(open) => {
                w.put(&open.id)?;
                w.put(&open.at.to_be_bytes())?;
            }
            None => w.put(&[0; 24])?,
        }
        w.put(&self.last_answer.unwrap_or(0).to_be_bytes())?;
        w.put(&[self.recent.len])?;
        for at in self.recent.as_slice() {
            w.put(&at.to_be_bytes())?;
        }
        w.text(&self.name)
    }

    /// Reads the binary form. Trailing bytes are ignored, so later versions
    /// can append fields.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        Self::read(&mut Reader::new(bytes))
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, Error> {
        let secret_key = r.array()?;
        let peer_key = r.array()?;
        let enrolment_token = r.array()?;
        let own_relay = r.text()?;
        let peer_mailbox = r.array()?;
        let peer_send_token = r.array()?;
        let peer_relay = r.text()?;
        let own_rules = Rules::read(r)?;
        let own_rules_stamp = Stamp::read(r)?;
        let peer_rules = Rules::read(r)?;
        let peer_rules_stamp = Stamp::read(r)?;
        let settings = ContactFlags::read(r)?;
        let id: MessageId = r.array()?;
        let at = r.u64()?;
        // An identifier of zero bytes means none; the time is then ignored.
        let open_request = (id != [0; 16]).then_some(OpenRequest { id, at });
        let last_answer = Some(r.u64()?).filter(|&at| at != 0);
        let count = usize::from(r.u8()?);
        if count > MAX_RECENT_REQUESTS {
            return Err(Error::TooLong);
        }
        let mut recent = Recent {
            len: count as u8,
            times: [0; MAX_RECENT_REQUESTS],
        };
        for slot in &mut recent.times[..count] {
            *slot = r.u64()?;
        }
        Ok(Self {
            secret_key,
            peer_key,
            enrolment_token,
            own_relay,
            peer_mailbox,
            peer_send_token,
            peer_relay,
            own_rules,
            own_rules_stamp,
            peer_rules,
            peer_rules_stamp,
            settings,
            open_request,
            last_answer,
            recent,
            name: r.text()?,
        })
    }
}

/// The satellite record: a satellite as the other full devices need it,
/// split into `satellite_part` messages.
///
/// The reader identifier of the satellite is not part of it: it is in
/// every `satellite_part`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub name: DeviceName,
    /// Relay of the channel mailbox.
    pub relay: RelayUrl,
    pub mailbox: MailboxId,
    /// Enrolment token of the receiving full device on the channel
    /// mailbox: its shared token, as for `contact_part`.
    pub enrolment_token: Token,
    pub send_token: Token,
    pub key: [u8; 32],
}

impl Record {
    /// Longest record in bytes.
    pub const MAX_LEN: usize = 1 + MAX_DEVICE_NAME_LEN + 1 + MAX_RELAY_URL_LEN + 4 * 32;

    /// Most parts of `satellite_part` a record is split into.
    pub const MAX_PARTS: usize = Self::MAX_LEN.div_ceil(MAX_PART_LEN);

    pub fn to_bytes(&self, out: &mut [u8; Self::MAX_LEN]) -> usize {
        let mut w = Writer::new(out);
        self.write(&mut w).expect("MAX_LEN covers every record");
        w.position()
    }

    fn write(&self, w: &mut Writer<'_>) -> Result<(), Error> {
        w.text(&self.name)?;
        w.text(&self.relay)?;
        w.put(&self.mailbox)?;
        w.put(&self.enrolment_token)?;
        w.put(&self.send_token)?;
        w.put(&self.key)
    }

    /// Reads the binary form; trailing bytes are ignored.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(bytes);
        Ok(Self {
            name: r.text()?,
            relay: r.text()?,
            mailbox: r.array()?,
            enrolment_token: r.array()?,
            send_token: r.array()?,
            key: r.array()?,
        })
    }
}

/// The fixed part at the start of the setup a full device hands the
/// satellite. The contacts follow it, see [`write_entry`] and [`Entries`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupHeader {
    /// Reader identifier of the satellite on every mailbox.
    pub reader: ReaderId,
    pub wifi_name: WifiName,
    pub wifi_password: WifiPassword,
    /// Relay of the channel mailbox.
    pub relay: RelayUrl,
    pub mailbox: MailboxId,
    /// Enrolment token of the satellite on the channel mailbox.
    pub enrolment_token: Token,
    pub send_token: Token,
    /// The channel key.
    pub key: [u8; 32],
    /// Stamp of the settings, all zeros for the defaults.
    pub settings_stamp: Stamp,
    pub settings: Settings,
    /// Number of contacts that follow.
    pub contacts: u8,
}

impl SetupHeader {
    /// Longest header in bytes.
    pub const MAX_LEN: usize = 1
        + 16
        + 1
        + MAX_WIFI_NAME_LEN
        + 1
        + MAX_WIFI_PASSWORD_LEN
        + 1
        + MAX_RELAY_URL_LEN
        + 4 * 32
        + 8
        + 16
        + Settings::MAX_LEN
        + 1;

    pub fn to_bytes(&self, out: &mut [u8; Self::MAX_LEN]) -> usize {
        let mut w = Writer::new(out);
        self.write(&mut w).expect("MAX_LEN covers every header");
        w.position()
    }

    fn write(&self, w: &mut Writer<'_>) -> Result<(), Error> {
        w.put(&[SETUP_VERSION])?;
        w.put(&self.reader)?;
        w.text(&self.wifi_name)?;
        w.text(&self.wifi_password)?;
        w.text(&self.relay)?;
        w.put(&self.mailbox)?;
        w.put(&self.enrolment_token)?;
        w.put(&self.send_token)?;
        w.put(&self.key)?;
        self.settings_stamp.write(w)?;
        self.settings.write(w)?;
        w.put(&[self.contacts])
    }

    /// Reads the header of a setup and returns it with the bytes behind
    /// it, which hold the contacts.
    pub fn from_bytes(bytes: &[u8]) -> Result<(Self, Entries<'_>), Error> {
        let mut r = Reader::new(bytes);
        match r.u8()? {
            SETUP_VERSION => {}
            other => return Err(Error::UnsupportedVersion(other)),
        }
        let header = Self {
            reader: r.array()?,
            wifi_name: r.text()?,
            wifi_password: r.text()?,
            relay: r.text()?,
            mailbox: r.array()?,
            enrolment_token: r.array()?,
            send_token: r.array()?,
            key: r.array()?,
            settings_stamp: Stamp::read(&mut r)?,
            settings: Settings::read(&mut r)?,
            contacts: r.u8()?,
        };
        let entries = Entries {
            rest: r,
            left: header.contacts,
        };
        Ok((header, entries))
    }
}

/// Longest contact entry of the setup: own mailbox, length, record.
pub const MAX_ENTRY_LEN: usize = 32 + 2 + ContactRecord::MAX_LEN;

/// Writes one contact of the setup into `out`: the own mailbox identifier
/// (32), the length of the record (2), the record. Returns the length.
pub fn write_entry(
    contact: &MailboxId,
    record: &ContactRecord,
    out: &mut [u8; MAX_ENTRY_LEN],
) -> usize {
    let mut bytes = [0; ContactRecord::MAX_LEN];
    let len = record.to_bytes(&mut bytes);
    out[..32].copy_from_slice(contact);
    out[32..34].copy_from_slice(&(len as u16).to_be_bytes());
    out[34..34 + len].copy_from_slice(&bytes[..len]);
    34 + len
}

/// The contacts of a setup, in order. Yields an error, and nothing after
/// it, if an entry cannot be read.
pub struct Entries<'a> {
    rest: Reader<'a>,
    left: u8,
}

impl Iterator for Entries<'_> {
    type Item = Result<(MailboxId, ContactRecord), Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        let entry = (|| {
            let contact = self.rest.array()?;
            let len = self.rest.u16()?;
            let record = ContactRecord::from_bytes(self.rest.take(len.into())?)?;
            Ok((contact, record))
        })();
        if entry.is_err() {
            self.left = 0;
        }
        Some(entry)
    }
}

// The longest records, as in `docs/m10-satellite.md`.
const _: () = assert!(ContactRecord::MAX_LEN == 698);
const _: () = assert!(ContactRecord::MAX_PARTS == 4);
const _: () = assert!(Record::MAX_LEN == 282);
const _: () = assert!(Record::MAX_PARTS == 2);

#[cfg(test)]
mod tests {
    use super::{
        ContactFlags, ContactRecord, Entries, MAX_ENTRY_LEN, MAX_RECENT_REQUESTS, OpenRequest,
        Preset, Recent, Record, Settings, SetupHeader, write_entry,
    };
    use crate::{
        ContactName, DeviceName, Error, MAX_RELAY_URL_LEN, Reason, RelayUrl, Rules, Stamp,
        record::split,
    };

    fn record(relay: &str, name: &str, recent: &[u64]) -> ContactRecord {
        ContactRecord {
            secret_key: [1; 32],
            peer_key: [2; 32],
            enrolment_token: [3; 32],
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
            settings: ContactFlags {
                reported: true,
                ask_button: 2,
            },
            open_request: Some(OpenRequest { id: [7; 16], at: 9 }),
            last_answer: Some(8),
            recent: Recent::newest(recent),
            name: ContactName::new(name).unwrap(),
        }
    }

    #[test]
    fn contact_record_layout_matches_documentation() {
        let mut buf = [0; ContactRecord::MAX_LEN];
        let original = record("https://r.example", "Anna", &[5, 9]);
        let len = original.to_bytes(&mut buf);
        assert_eq!(len, 96 + 18 + 64 + 18 + 68 + 2 + 24 + 8 + 1 + 16 + 5);
        assert_eq!(&buf[64..96], &[3; 32]);
        assert_eq!(buf[96], 17);
        assert_eq!(&buf[196..206], &[0, 0, 1, 104, 0, 0, 5, 160, 0, 7]);
        assert_eq!(&buf[264..266], &[1, 2], "reported, ask button");
        assert_eq!(&buf[266..282], &[7; 16]);
        assert_eq!(&buf[282..290], &9u64.to_be_bytes());
        assert_eq!(&buf[290..298], &8u64.to_be_bytes());
        assert_eq!(buf[298], 2);
        assert_eq!(&buf[299..307], &5u64.to_be_bytes());
        assert_eq!(&buf[307..315], &9u64.to_be_bytes());
        assert_eq!(buf[315], 4);
        assert_eq!(&buf[316..320], b"Anna");
        assert_eq!(ContactRecord::from_bytes(&buf[..len]).unwrap(), original);
    }

    #[test]
    fn longest_contact_record_has_698_bytes_in_four_parts() {
        let times: [u64; 20] = core::array::from_fn(|i| i as u64 + 1);
        let longest = record(&"r".repeat(MAX_RELAY_URL_LEN), &"n".repeat(64), &times);
        assert_eq!(longest.recent.as_slice(), &times[4..], "the newest 16");
        let mut buf = [0; ContactRecord::MAX_LEN];
        assert_eq!(longest.to_bytes(&mut buf), 698);
        let lens: [usize; 4] = core::array::from_fn(|i| split(&buf).nth(i).unwrap().len());
        assert_eq!(lens, [178, 178, 178, 164]);
        assert_eq!(ContactRecord::from_bytes(&buf).unwrap(), longest);
    }

    #[test]
    fn empty_parts_of_the_contact_record() {
        let mut short = record("", "", &[]);
        short.open_request = None;
        short.last_answer = None;
        short.settings = ContactFlags::default();
        let mut buf = [0; ContactRecord::MAX_LEN];
        let len = short.to_bytes(&mut buf);
        assert_eq!(len, 3 * 32 + 1 + 2 * 32 + 1 + 68 + 2 + 24 + 8 + 1 + 1);
        assert_eq!(ContactRecord::from_bytes(&buf[..len]).unwrap(), short);
        // Trailing bytes are ignored, a short record refused.
        assert!(ContactRecord::from_bytes(&buf[..len + 5]).is_ok());
        assert_eq!(
            ContactRecord::from_bytes(&buf[..len - 1]),
            Err(Error::Truncated)
        );
    }

    #[test]
    fn contact_record_refuses_undefined_values() {
        let original = record("https://r.example", "Anna", &[1]);
        let mut buf = [0; ContactRecord::MAX_LEN];
        let len = original.to_bytes(&mut buf);
        let mut bad = buf;
        bad[264] = 2;
        assert_eq!(
            ContactRecord::from_bytes(&bad[..len]),
            Err(Error::InvalidValue)
        );
        let mut bad = buf;
        bad[298] = MAX_RECENT_REQUESTS as u8 + 1;
        assert_eq!(ContactRecord::from_bytes(&bad), Err(Error::TooLong));
    }

    fn satellite() -> Record {
        Record {
            name: DeviceName::new(&"s".repeat(32)).unwrap(),
            relay: RelayUrl::new(&"r".repeat(MAX_RELAY_URL_LEN)).unwrap(),
            mailbox: [1; 32],
            enrolment_token: [2; 32],
            send_token: [3; 32],
            key: [4; 32],
        }
    }

    #[test]
    fn satellite_record_has_282_bytes_in_two_parts() {
        let mut buf = [0; Record::MAX_LEN];
        assert_eq!(satellite().to_bytes(&mut buf), 282);
        assert_eq!(buf[0], 32);
        assert_eq!(buf[33], 120);
        assert_eq!(&buf[154..186], &[1; 32]);
        assert_eq!(&buf[250..282], &[4; 32]);
        assert_eq!(split(&buf).count(), 2);
        assert_eq!(Record::from_bytes(&buf).unwrap(), satellite());
        assert_eq!(Record::from_bytes(&buf[..281]), Err(Error::Truncated));
    }

    fn settings() -> Settings {
        Settings {
            waiting_minutes: 30,
            preset: Some(Preset {
                minutes: 480,
                share: true,
                reason: Reason::new("asleep").unwrap(),
            }),
        }
    }

    fn header(contacts: u8) -> SetupHeader {
        SetupHeader {
            reader: [9; 16],
            wifi_name: super::WifiName::new("home").unwrap(),
            wifi_password: super::WifiPassword::new("secret").unwrap(),
            relay: RelayUrl::new("https://r.example").unwrap(),
            mailbox: [1; 32],
            enrolment_token: [2; 32],
            send_token: [3; 32],
            key: [4; 32],
            settings_stamp: Stamp { at: 5, id: [6; 16] },
            settings: settings(),
            contacts,
        }
    }

    #[test]
    fn setup_round_trips_with_its_contacts() {
        let mut bytes = [0; SetupHeader::MAX_LEN + 2 * MAX_ENTRY_LEN];
        let mut len = header(2).to_bytes((&mut bytes[..SetupHeader::MAX_LEN]).try_into().unwrap());
        assert_eq!(bytes[0], 1);
        assert_eq!(&bytes[1..17], &[9; 16]);
        assert_eq!(bytes[17], 4);
        for (fill, name) in [(1u8, "Anna"), (2, "Bert")] {
            let mut entry = [0; MAX_ENTRY_LEN];
            let n = write_entry(
                &[fill; 32],
                &record("https://r.example", name, &[1]),
                &mut entry,
            );
            bytes[len..len + n].copy_from_slice(&entry[..n]);
            len += n;
        }
        let (read, entries) = SetupHeader::from_bytes(&bytes[..len]).unwrap();
        assert_eq!(read, header(2));
        let names: [_; 2] = core::array::from_fn(|_| ());
        let mut entries: Entries<'_> = entries;
        for (i, _) in names.iter().enumerate() {
            let (contact, record) = entries.next().unwrap().unwrap();
            assert_eq!(contact, [i as u8 + 1; 32]);
            assert_eq!(record.name.as_str(), ["Anna", "Bert"][i]);
        }
        assert!(entries.next().is_none());

        // A cut entry fails, and nothing comes after it.
        let (_, mut entries) = SetupHeader::from_bytes(&bytes[..len - 1]).unwrap();
        assert!(entries.next().unwrap().is_ok());
        assert_eq!(entries.next().unwrap(), Err(Error::Truncated));
        assert!(entries.next().is_none());
        // Another version is refused.
        bytes[0] = 2;
        assert!(matches!(
            SetupHeader::from_bytes(&bytes),
            Err(Error::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn setup_header_has_its_maximum() {
        let mut longest = header(255);
        longest.wifi_name = super::WifiName::new(&"w".repeat(32)).unwrap();
        longest.wifi_password = super::WifiPassword::new(&"p".repeat(64)).unwrap();
        longest.relay = RelayUrl::new(&"r".repeat(MAX_RELAY_URL_LEN)).unwrap();
        longest.settings.preset.as_mut().unwrap().reason = Reason::new(&"g".repeat(64)).unwrap();
        let mut buf = [0; SetupHeader::MAX_LEN];
        assert_eq!(longest.to_bytes(&mut buf), SetupHeader::MAX_LEN);
        assert_eq!(SetupHeader::MAX_LEN, 461);
    }

    #[test]
    fn settings_without_preset_are_zero_bytes() {
        let mut buf = [0xFF; Settings::MAX_LEN];
        let mut w = crate::Writer::new(&mut buf);
        Settings::DEFAULT.write(&mut w).unwrap();
        assert_eq!(w.position(), 8);
        assert_eq!(&buf[..8], &[0, 15, 0, 0, 0, 0, 0, 0]);
        let read = Settings::read(&mut crate::Reader::new(&buf)).unwrap();
        assert_eq!(read, Settings::DEFAULT);
        // A share byte other than 0 or 1 is refused.
        let mut bad = buf;
        bad[6] = 2;
        assert_eq!(
            Settings::read(&mut crate::Reader::new(&bad)),
            Err(Error::InvalidValue)
        );
    }
}
