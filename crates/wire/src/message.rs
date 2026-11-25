//! Plaintext of a message: common header, typed body, zero padding.
//!
//! The plaintext is what `crypto_box` or `crypto_box_seal` encrypts. Its
//! length is fixed by the envelope form: [`PADDED_PLAINTEXT_LEN`] for the
//! ordinary form and [`SEALED_PLAINTEXT_LEN`] for the sealed form.
//!
//! [`PADDED_PLAINTEXT_LEN`]: crate::PADDED_PLAINTEXT_LEN
//! [`SEALED_PLAINTEXT_LEN`]: crate::SEALED_PLAINTEXT_LEN

use crate::satellite::{
    ContactFlags, ContactSettings, ContactSettingsFor, Part as SatellitePart, Settings, SettingsFor,
};
use crate::{
    DeviceName, Error, Hash, MAX_PART_LEN, MailboxId, MessageId, PublicKey, Reader, ReaderId,
    Reason, RelayUrl, Token, TransferId, Writer,
};

/// Length of the header in front of every body: type, identifier, time,
/// body length.
pub const HEADER_LEN: usize = 1 + 16 + 8 + 2;

/// Message type numbers.
pub mod kind {
    pub const HELLO: u8 = 1;
    pub const HELLO_OK: u8 = 2;
    pub const RULES: u8 = 3;
    pub const REQUEST: u8 = 4;
    pub const RESPONSE: u8 = 5;
    pub const END: u8 = 6;
    pub const REVEAL: u8 = 7;
    // Between own devices, only in the group form.
    pub const DEVICE: u8 = 64;
    pub const DEVICE_REMOVED: u8 = 65;
    pub const REQUESTED: u8 = 66;
    pub const RESPONDED: u8 = 67;
    pub const RULES_SET: u8 = 68;
    pub const CONTACT_PART: u8 = 69;
    pub const CONTACT_REMOVED: u8 = 70;
    pub const STATE_SHARED: u8 = 71;
    pub const STATE_CLEARED: u8 = 72;
    // About satellites, between the full devices (M10).
    pub const SATELLITE_PART: u8 = 73;
    pub const SATELLITE_SETTINGS: u8 = 74;
    pub const SATELLITE_CONTACT_SETTINGS: u8 = 75;
    // The channel of a satellite (M10), only in the group form with the
    // channel key. Up: from the satellite to the full devices.
    pub const SAT_ANSWERED: u8 = 80;
    pub const SAT_ASKED: u8 = 81;
    pub const SAT_STATE_SHARED: u8 = 82;
    pub const SAT_STATE_CLEARED: u8 = 83;
    // Down: from the full devices to the satellite.
    pub const HOME_CONTACT_PART: u8 = 88;
    pub const HOME_RESPONDED: u8 = 89;
    pub const HOME_REQUESTED: u8 = 90;
    pub const HOME_RULES: u8 = 91;
    pub const HOME_CONTACT_SETTINGS: u8 = 92;
    pub const HOME_SETTINGS: u8 = 93;
    pub const HOME_STATE_SHARED: u8 = 94;
    pub const HOME_STATE_CLEARED: u8 = 95;
    pub const HOME_SEND_TOKEN: u8 = 96;

    /// Types of messages between full devices in the sync mailbox; the
    /// rest of 64–127 outside the channel range is reserved for them.
    pub const fn is_sync(kind: u8) -> bool {
        matches!(kind, 64..=79 | 100..=127)
    }

    /// Up types of the channel (80–87): from the satellite.
    pub const fn is_up(kind: u8) -> bool {
        matches!(kind, 80..=87)
    }

    /// Down types of the channel (88–99): to the satellite.
    pub const fn is_down(kind: u8) -> bool {
        matches!(kind, 88..=99)
    }
}

/// A decoded plaintext.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Random per message; for a request also the identifier of the request.
    pub id: MessageId,
    /// Time of sending in Unix seconds.
    pub sent_at: u64,
    pub body: Body,
}

/// The typed part of a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Hello(Hello),
    HelloOk,
    Rules(Rules),
    Request,
    Response(Response),
    End,
    /// The inviting side reveals its secret value `n` after the `hello`.
    Reveal(Hash),
    /// A new device of the person announces itself.
    Device(DeviceAnnounce),
    /// A device of the person was removed.
    DeviceRemoved(ReaderId),
    Requested(Requested),
    Responded(Responded),
    RulesSet(RulesSet),
    ContactPart(ContactPart),
    /// Another device ended the contact with this own mailbox identifier.
    ContactRemoved(MailboxId),
    /// Another device handed its reported state to this one.
    StateShared(StateShared),
    /// Another device ended the state it handed on.
    StateCleared,
    /// `satellite_part` (73): a satellite set up on another full device.
    SatellitePart(SatellitePart),
    /// `satellite_settings` (74).
    SatelliteSettings(SettingsFor),
    /// `satellite_contact_settings` (75).
    SatelliteContactSettings(ContactSettingsFor),
    /// `sat_answered` (80): the satellite answered a request.
    SatAnswered(Responded),
    /// `sat_asked` (81): the satellite asked a contact.
    SatAsked(Requested),
    /// `sat_state_shared` (82): the satellite shares its state.
    SatStateShared(StateShared),
    /// `sat_state_cleared` (83).
    SatStateCleared,
    /// `home_contact_part` (88): one part of a satellite contact record.
    HomeContactPart(ContactPart),
    /// `home_responded` (89): a full device answered a request.
    HomeResponded(Responded),
    /// `home_requested` (90): a full device asked a contact.
    HomeRequested(Requested),
    /// `home_rules` (91): the own rules for a contact changed.
    HomeRules(RulesSet),
    /// `home_contact_settings` (92).
    HomeContactSettings(ContactSettings),
    /// `home_settings` (93).
    HomeSettings(Settings),
    /// `home_state_shared` (94).
    HomeStateShared(StateShared),
    /// `home_state_cleared` (95).
    HomeStateCleared,
    /// `home_send_token` (96): the new send token of the channel.
    HomeSendToken(Token),
    /// A type this does not know. Receivers ignore it; it cannot be
    /// encoded.
    Unknown(u8),
}

/// `device`: a new device of the person announces itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceAnnounce {
    pub reader: ReaderId,
    pub name: DeviceName,
}

/// `requested`: another device of the person sent a request.
///
/// Contacts are identified by the own mailbox for them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requested {
    pub contact: MailboxId,
    pub request: MessageId,
    /// Time of the request.
    pub at: u64,
}

/// `responded`: another device of the person answered a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Responded {
    pub contact: MailboxId,
    pub request: MessageId,
    pub kind: ResponseKind,
    /// Time of the response.
    pub at: u64,
}

/// `rules_set`: another device of the person changed the own rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RulesSet {
    pub contact: MailboxId,
    pub rules: Rules,
}

/// `state_shared`: another device of the person hands on its reported
/// state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateShared {
    pub reason: Reason,
    /// End of the state in Unix seconds.
    pub until: u64,
}

/// `contact_part`: one part of the record of a newly paired contact.
///
/// `index < count` is not checked here: the receiver discards the whole
/// transfer if it does not hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactPart {
    pub contact: MailboxId,
    pub transfer: TransferId,
    pub index: u8,
    pub count: u8,
    pub data: PartData,
}

/// Data of one `contact_part`, at most [`MAX_PART_LEN`] bytes.
#[derive(Clone, Copy)]
pub struct PartData {
    len: u8,
    bytes: [u8; MAX_PART_LEN],
}

impl PartData {
    /// Copies the data, or fails with [`Error::TooLong`] above
    /// [`MAX_PART_LEN`] bytes.
    pub fn new(data: &[u8]) -> Result<Self, Error> {
        if data.len() > MAX_PART_LEN {
            return Err(Error::TooLong);
        }
        let mut out = Self {
            len: data.len() as u8,
            bytes: [0; MAX_PART_LEN],
        };
        out.bytes[..data.len()].copy_from_slice(data);
        Ok(out)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len.into()]
    }
}

impl PartialEq for PartData {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for PartData {}

impl core::fmt::Debug for PartData {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PartData({} bytes)", self.len)
    }
}

/// Contact and transfer of a `contact_part` whose body could not be read,
/// so that the receiver can discard the whole transfer.
///
/// `None` if the plaintext is not a `contact_part` or even these fields are
/// missing.
pub fn contact_part_ids(plaintext: &[u8]) -> Option<(MailboxId, TransferId)> {
    let mut r = Reader::new(plaintext);
    if r.u8().ok()? != kind::CONTACT_PART {
        return None;
    }
    r.take(16 + 8).ok()?;
    let body_len = r.u16().ok()?;
    let mut body = Reader::new(r.take(body_len.into()).ok()?);
    Some((body.array().ok()?, body.array().ok()?))
}

/// Satellite and transfer of a `satellite_part` whose body could not be
/// read, as [`contact_part_ids`].
pub fn satellite_part_ids(plaintext: &[u8]) -> Option<(ReaderId, TransferId)> {
    let mut r = Reader::new(plaintext);
    if r.u8().ok()? != kind::SATELLITE_PART {
        return None;
    }
    r.take(16 + 8).ok()?;
    let body_len = r.u16().ok()?;
    let mut body = Reader::new(r.take(body_len.into()).ok()?);
    Some((body.array().ok()?, body.array().ok()?))
}

/// Accepts an invitation. Sent only in the sealed form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    /// Contact key of the sender.
    pub public_key: PublicKey,
    /// Mailbox of the sender, into which the invitee delivers from now on.
    pub mailbox: MailboxId,
    pub send_token: Token,
    pub relay: RelayUrl,
}

/// How often the sender of these rules may be asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Rules {
    /// Minutes after a manual response before the next request is accepted.
    pub min_interval_minutes: u32,
    /// Minutes after which an unanswered request expires.
    pub expiry_minutes: u32,
    /// Accepted requests within any 7 × 24 hours.
    pub weekly_limit: u16,
}

impl Rules {
    /// Values that apply until the other side has sent its rules.
    pub const DEFAULT: Self = Self {
        min_interval_minutes: 360,
        expiry_minutes: 1440,
        weekly_limit: 7,
    };
}

impl Default for Rules {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The sign of life itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// Identifier of the answered request.
    pub request: MessageId,
    pub kind: ResponseKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResponseKind {
    /// Someone reacted on the device.
    Manual,
    /// A reported state was sent after the waiting time.
    Reported(Reason),
}

const MANUAL: u8 = 1;
const REPORTED: u8 = 2;

impl Message {
    /// Writes the plaintext into `out` and fills the rest with zeros.
    ///
    /// `out` must have the plaintext length of the chosen envelope form. The
    /// content of `out` is unspecified if this fails.
    pub fn encode(&self, out: &mut [u8]) -> Result<(), Error> {
        out.fill(0);
        let (head, body) = out.split_at_mut_checked(HEADER_LEN).ok_or(Error::TooLong)?;
        let mut w = Writer::new(body);
        self.body.write(&mut w)?;
        let body_len = u16::try_from(w.position()).map_err(|_| Error::TooLong)?;

        let mut w = Writer::new(head);
        w.put(&[self.body.kind()?])?;
        w.put(&self.id)?;
        w.put(&self.sent_at.to_be_bytes())?;
        w.put(&body_len.to_be_bytes())
    }

    /// Reads a decrypted plaintext.
    ///
    /// Bytes behind a known body and the padding are ignored, so later
    /// versions can append fields. Unknown types decode to [`Body::Unknown`].
    pub fn decode(plaintext: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(plaintext);
        let kind = r.u8()?;
        let id = r.array()?;
        let sent_at = r.u64()?;
        let body_len = r.u16()?;
        let body = Body::read(kind, &mut Reader::new(r.take(body_len.into())?))?;
        Ok(Self { id, sent_at, body })
    }
}

impl Body {
    /// The type number, or [`Error::InvalidValue`] for [`Body::Unknown`].
    pub fn kind(&self) -> Result<u8, Error> {
        Ok(match self {
            Self::Hello(_) => kind::HELLO,
            Self::HelloOk => kind::HELLO_OK,
            Self::Rules(_) => kind::RULES,
            Self::Request => kind::REQUEST,
            Self::Response(_) => kind::RESPONSE,
            Self::End => kind::END,
            Self::Reveal(_) => kind::REVEAL,
            Self::Device(_) => kind::DEVICE,
            Self::DeviceRemoved(_) => kind::DEVICE_REMOVED,
            Self::Requested(_) => kind::REQUESTED,
            Self::Responded(_) => kind::RESPONDED,
            Self::RulesSet(_) => kind::RULES_SET,
            Self::ContactPart(_) => kind::CONTACT_PART,
            Self::ContactRemoved(_) => kind::CONTACT_REMOVED,
            Self::StateShared(_) => kind::STATE_SHARED,
            Self::StateCleared => kind::STATE_CLEARED,
            Self::SatellitePart(_) => kind::SATELLITE_PART,
            Self::SatelliteSettings(_) => kind::SATELLITE_SETTINGS,
            Self::SatelliteContactSettings(_) => kind::SATELLITE_CONTACT_SETTINGS,
            Self::SatAnswered(_) => kind::SAT_ANSWERED,
            Self::SatAsked(_) => kind::SAT_ASKED,
            Self::SatStateShared(_) => kind::SAT_STATE_SHARED,
            Self::SatStateCleared => kind::SAT_STATE_CLEARED,
            Self::HomeContactPart(_) => kind::HOME_CONTACT_PART,
            Self::HomeResponded(_) => kind::HOME_RESPONDED,
            Self::HomeRequested(_) => kind::HOME_REQUESTED,
            Self::HomeRules(_) => kind::HOME_RULES,
            Self::HomeContactSettings(_) => kind::HOME_CONTACT_SETTINGS,
            Self::HomeSettings(_) => kind::HOME_SETTINGS,
            Self::HomeStateShared(_) => kind::HOME_STATE_SHARED,
            Self::HomeStateCleared => kind::HOME_STATE_CLEARED,
            Self::HomeSendToken(_) => kind::HOME_SEND_TOKEN,
            Self::Unknown(_) => return Err(Error::InvalidValue),
        })
    }

    /// Whether this is a message between own full devices, sent only in
    /// the group form in the sync mailbox. Channel types are not.
    pub fn is_sync(&self) -> bool {
        matches!(self.kind(), Ok(kind) if kind::is_sync(kind))
    }

    /// Whether this is an up type of the channel, from the satellite.
    pub fn is_up(&self) -> bool {
        matches!(self.kind(), Ok(kind) if kind::is_up(kind))
    }

    /// Whether this is a down type of the channel, to the satellite.
    pub fn is_down(&self) -> bool {
        matches!(self.kind(), Ok(kind) if kind::is_down(kind))
    }

    /// The satellite a message between own devices is about, if any.
    pub fn sync_satellite(&self) -> Option<&ReaderId> {
        match self {
            Self::SatellitePart(p) => Some(&p.reader),
            Self::SatelliteSettings(s) => Some(&s.reader),
            Self::SatelliteContactSettings(s) => Some(&s.reader),
            _ => None,
        }
    }

    /// The contact a channel message is about, if any.
    pub fn channel_contact(&self) -> Option<&MailboxId> {
        match self {
            Self::SatAnswered(r) | Self::HomeResponded(r) => Some(&r.contact),
            Self::SatAsked(r) | Self::HomeRequested(r) => Some(&r.contact),
            Self::HomeRules(r) => Some(&r.contact),
            Self::HomeContactSettings(s) => Some(&s.contact),
            Self::HomeContactPart(p) => Some(&p.contact),
            _ => None,
        }
    }

    /// The contact a message between own devices is about, if any.
    pub fn sync_contact(&self) -> Option<&MailboxId> {
        match self {
            Self::Requested(r) => Some(&r.contact),
            Self::Responded(r) => Some(&r.contact),
            Self::RulesSet(r) => Some(&r.contact),
            Self::ContactPart(p) => Some(&p.contact),
            Self::ContactRemoved(contact) => Some(contact),
            _ => None,
        }
    }

    fn write(&self, w: &mut Writer<'_>) -> Result<(), Error> {
        match self {
            Self::Hello(hello) => {
                w.put(&hello.public_key)?;
                w.put(&hello.mailbox)?;
                w.put(&hello.send_token)?;
                w.text(&hello.relay)
            }
            Self::Rules(rules) => rules.write(w),
            Self::Response(response) => {
                w.put(&response.request)?;
                response.kind.write(w)
            }
            Self::HelloOk | Self::Request | Self::End => Ok(()),
            Self::Reveal(secret) => w.put(secret),
            Self::Device(device) => {
                w.put(&device.reader)?;
                w.text(&device.name)
            }
            Self::DeviceRemoved(reader) => w.put(reader),
            Self::Requested(requested) => {
                w.put(&requested.contact)?;
                w.put(&requested.request)?;
                w.put(&requested.at.to_be_bytes())
            }
            Self::Responded(responded) => {
                w.put(&responded.contact)?;
                w.put(&responded.request)?;
                responded.kind.write(w)?;
                w.put(&responded.at.to_be_bytes())
            }
            Self::RulesSet(set) => {
                w.put(&set.contact)?;
                set.rules.write(w)
            }
            Self::ContactPart(part) => {
                w.put(&part.contact)?;
                w.put(&part.transfer)?;
                w.put(&[part.index, part.count, part.data.len])?;
                w.put(part.data.as_bytes())
            }
            Self::ContactRemoved(contact) => w.put(contact),
            Self::StateShared(state) => {
                w.text(&state.reason)?;
                w.put(&state.until.to_be_bytes())
            }
            Self::StateCleared | Self::SatStateCleared | Self::HomeStateCleared => Ok(()),
            Self::SatellitePart(part) => {
                w.put(&part.reader)?;
                w.put(&part.transfer)?;
                w.put(&[part.index, part.count, part.data.len])?;
                w.put(part.data.as_bytes())
            }
            Self::SatelliteSettings(s) => {
                w.put(&s.reader)?;
                s.settings.write(w)
            }
            Self::SatelliteContactSettings(s) => {
                w.put(&s.reader)?;
                w.put(&s.settings.contact)?;
                s.settings.of.write(w)
            }
            Self::SatAnswered(r) | Self::HomeResponded(r) => Self::Responded(r.clone()).write(w),
            Self::SatAsked(r) | Self::HomeRequested(r) => Self::Requested(r.clone()).write(w),
            Self::SatStateShared(s) | Self::HomeStateShared(s) => {
                Self::StateShared(s.clone()).write(w)
            }
            Self::HomeContactPart(p) => Self::ContactPart(p.clone()).write(w),
            Self::HomeRules(r) => Self::RulesSet(r.clone()).write(w),
            Self::HomeContactSettings(s) => {
                w.put(&s.contact)?;
                s.of.write(w)
            }
            Self::HomeSettings(s) => s.write(w),
            Self::HomeSendToken(token) => w.put(token),
            Self::Unknown(_) => Err(Error::InvalidValue),
        }
    }

    fn read(kind: u8, r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(match kind {
            kind::HELLO => Self::Hello(Hello {
                public_key: r.array()?,
                mailbox: r.array()?,
                send_token: r.array()?,
                relay: r.text()?,
            }),
            kind::HELLO_OK => Self::HelloOk,
            kind::RULES => Self::Rules(Rules::read(r)?),
            kind::REQUEST => Self::Request,
            kind::RESPONSE => Self::Response(Response {
                request: r.array()?,
                kind: ResponseKind::read(r)?,
            }),
            kind::END => Self::End,
            kind::REVEAL => Self::Reveal(r.array()?),
            kind::DEVICE => Self::Device(DeviceAnnounce {
                reader: r.array()?,
                name: r.text()?,
            }),
            kind::DEVICE_REMOVED => Self::DeviceRemoved(r.array()?),
            kind::REQUESTED => Self::Requested(Self::requested(r)?),
            kind::RESPONDED => Self::Responded(Self::responded(r)?),
            kind::RULES_SET => Self::RulesSet(RulesSet {
                contact: r.array()?,
                rules: Rules::read(r)?,
            }),
            kind::CONTACT_PART => Self::ContactPart(Self::contact_part(r)?),
            kind::CONTACT_REMOVED => Self::ContactRemoved(r.array()?),
            kind::STATE_SHARED => Self::StateShared(Self::state_shared(r)?),
            kind::STATE_CLEARED => Self::StateCleared,
            kind::SATELLITE_PART => {
                let reader = r.array()?;
                let transfer = r.array()?;
                let [index, count, len] = r.array()?;
                Self::SatellitePart(SatellitePart {
                    reader,
                    transfer,
                    index,
                    count,
                    data: PartData::new(r.take(len.into())?)?,
                })
            }
            kind::SATELLITE_SETTINGS => Self::SatelliteSettings(SettingsFor {
                reader: r.array()?,
                settings: Settings::read(r)?,
            }),
            kind::SATELLITE_CONTACT_SETTINGS => {
                Self::SatelliteContactSettings(ContactSettingsFor {
                    reader: r.array()?,
                    settings: ContactSettings {
                        contact: r.array()?,
                        of: ContactFlags::read(r)?,
                    },
                })
            }
            kind::SAT_ANSWERED => Self::SatAnswered(Self::responded(r)?),
            kind::SAT_ASKED => Self::SatAsked(Self::requested(r)?),
            kind::SAT_STATE_SHARED => Self::SatStateShared(Self::state_shared(r)?),
            kind::SAT_STATE_CLEARED => Self::SatStateCleared,
            kind::HOME_CONTACT_PART => Self::HomeContactPart(Self::contact_part(r)?),
            kind::HOME_RESPONDED => Self::HomeResponded(Self::responded(r)?),
            kind::HOME_REQUESTED => Self::HomeRequested(Self::requested(r)?),
            kind::HOME_RULES => Self::HomeRules(RulesSet {
                contact: r.array()?,
                rules: Rules::read(r)?,
            }),
            kind::HOME_CONTACT_SETTINGS => Self::HomeContactSettings(ContactSettings {
                contact: r.array()?,
                of: ContactFlags::read(r)?,
            }),
            kind::HOME_SETTINGS => Self::HomeSettings(Settings::read(r)?),
            kind::HOME_STATE_SHARED => Self::HomeStateShared(Self::state_shared(r)?),
            kind::HOME_STATE_CLEARED => Self::HomeStateCleared,
            kind::HOME_SEND_TOKEN => Self::HomeSendToken(r.array()?),
            other => Self::Unknown(other),
        })
    }

    fn requested(r: &mut Reader<'_>) -> Result<Requested, Error> {
        Ok(Requested {
            contact: r.array()?,
            request: r.array()?,
            at: r.u64()?,
        })
    }

    fn responded(r: &mut Reader<'_>) -> Result<Responded, Error> {
        Ok(Responded {
            contact: r.array()?,
            request: r.array()?,
            kind: ResponseKind::read(r)?,
            at: r.u64()?,
        })
    }

    fn state_shared(r: &mut Reader<'_>) -> Result<StateShared, Error> {
        Ok(StateShared {
            reason: r.text()?,
            until: r.u64()?,
        })
    }

    fn contact_part(r: &mut Reader<'_>) -> Result<ContactPart, Error> {
        let contact = r.array()?;
        let transfer = r.array()?;
        let [index, count, len] = r.array()?;
        Ok(ContactPart {
            contact,
            transfer,
            index,
            count,
            data: PartData::new(r.take(len.into())?)?,
        })
    }
}

impl Rules {
    pub(crate) fn write(&self, w: &mut Writer<'_>) -> Result<(), Error> {
        w.put(&self.min_interval_minutes.to_be_bytes())?;
        w.put(&self.expiry_minutes.to_be_bytes())?;
        w.put(&self.weekly_limit.to_be_bytes())
    }

    pub(crate) fn read(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Self {
            min_interval_minutes: r.u32()?,
            expiry_minutes: r.u32()?,
            weekly_limit: r.u16()?,
        })
    }
}

impl ResponseKind {
    fn write(&self, w: &mut Writer<'_>) -> Result<(), Error> {
        match self {
            Self::Manual => w.put(&[MANUAL, 0]),
            Self::Reported(reason) => {
                w.put(&[REPORTED])?;
                w.text(reason)
            }
        }
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(match r.u8()? {
            MANUAL => {
                // The reason must be empty; a non-empty one is ignored.
                r.text::<255>()?;
                Self::Manual
            }
            REPORTED => Self::Reported(r.text()?),
            _ => return Err(Error::InvalidValue),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Body, ContactPart, DeviceAnnounce, HEADER_LEN, Hello, Message, PartData, Requested,
        Responded, Response, ResponseKind, Rules, RulesSet, StateShared, contact_part_ids, kind,
    };
    use crate::{
        DeviceName, Error, MAX_DEVICE_NAME_LEN, MAX_PART_LEN, MAX_REASON_LEN, MAX_RELAY_URL_LEN,
        PADDED_PLAINTEXT_LEN, Reason, RelayUrl, SEALED_PLAINTEXT_LEN,
    };

    fn message(body: Body) -> Message {
        Message {
            id: [0x11; 16],
            sent_at: 0x0102_0304_0506_0708,
            body,
        }
    }

    fn round_trip(body: Body, len: usize) -> [u8; PADDED_PLAINTEXT_LEN] {
        let msg = message(body);
        let mut buf = [0xFF; PADDED_PLAINTEXT_LEN];
        msg.encode(&mut buf[..len]).unwrap();
        assert_eq!(Message::decode(&buf[..len]).unwrap(), msg);
        buf
    }

    fn longest_hello() -> Hello {
        Hello {
            public_key: [1; 32],
            mailbox: [2; 32],
            send_token: [3; 32],
            relay: RelayUrl::new(&"r".repeat(MAX_RELAY_URL_LEN)).unwrap(),
        }
    }

    #[test]
    fn header_layout_matches_documentation() {
        let buf = round_trip(Body::Request, PADDED_PLAINTEXT_LEN);
        assert_eq!(buf[0], kind::REQUEST);
        assert_eq!(&buf[1..17], &[0x11; 16]);
        assert_eq!(&buf[17..25], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&buf[25..27], &[0, 0]);
        assert!(buf[HEADER_LEN..].iter().all(|&b| b == 0));
    }

    #[test]
    fn rules_layout_matches_documentation() {
        let buf = round_trip(Body::Rules(Rules::DEFAULT), PADDED_PLAINTEXT_LEN);
        assert_eq!(&buf[25..27], &[0, 10]);
        assert_eq!(&buf[27..37], &[0, 0, 1, 104, 0, 0, 5, 160, 0, 7]);
    }

    #[test]
    fn response_layout_matches_documentation() {
        let reason = Reason::new("schläft").unwrap();
        let buf = round_trip(
            Body::Response(Response {
                request: [9; 16],
                kind: ResponseKind::Reported(reason),
            }),
            PADDED_PLAINTEXT_LEN,
        );
        assert_eq!(&buf[25..27], &[0, 26]);
        assert_eq!(&buf[27..43], &[9; 16]);
        assert_eq!(&buf[43..45], &[2, 8]);
        assert_eq!(&buf[45..53], "schläft".as_bytes());

        let buf = round_trip(
            Body::Response(Response {
                request: [9; 16],
                kind: ResponseKind::Manual,
            }),
            PADDED_PLAINTEXT_LEN,
        );
        assert_eq!(&buf[25..27], &[0, 18]);
        assert_eq!(&buf[43..45], &[1, 0]);
    }

    #[test]
    fn all_types_round_trip() {
        for body in [
            Body::Hello(longest_hello()),
            Body::HelloOk,
            Body::Rules(Rules::DEFAULT),
            Body::Request,
            Body::Response(Response {
                request: [7; 16],
                kind: ResponseKind::Reported(Reason::new(&"g".repeat(MAX_REASON_LEN)).unwrap()),
            }),
            Body::End,
            Body::Reveal([8; 32]),
        ] {
            round_trip(body, PADDED_PLAINTEXT_LEN);
        }
    }

    #[test]
    fn reveal_layout_matches_documentation() {
        let buf = round_trip(Body::Reveal([8; 32]), PADDED_PLAINTEXT_LEN);
        assert_eq!(buf[0], 7);
        assert_eq!(&buf[25..27], &[0, 32]);
        assert_eq!(&buf[27..59], &[8; 32]);
        assert!(buf[59..].iter().all(|&b| b == 0));
        assert!(!Body::Reveal([8; 32]).is_sync());
        let mut short = buf;
        short[25..27].copy_from_slice(&31u16.to_be_bytes());
        assert_eq!(Message::decode(&short), Err(Error::Truncated));
    }

    #[test]
    fn longest_hello_fits_sealed_form() {
        round_trip(Body::Hello(longest_hello()), SEALED_PLAINTEXT_LEN);
    }

    #[test]
    fn unknown_type_decodes_but_does_not_encode() {
        let mut buf = round_trip(Body::Request, PADDED_PLAINTEXT_LEN);
        buf[0] = 99;
        assert_eq!(Message::decode(&buf).unwrap().body, Body::Unknown(99));
        assert_eq!(
            message(Body::Unknown(99)).encode(&mut buf),
            Err(Error::InvalidValue)
        );
    }

    #[test]
    fn longer_body_is_accepted_shorter_is_rejected() {
        let mut buf = round_trip(Body::Rules(Rules::DEFAULT), PADDED_PLAINTEXT_LEN);
        buf[26] = 12;
        assert_eq!(
            Message::decode(&buf).unwrap().body,
            Body::Rules(Rules::DEFAULT)
        );
        buf[26] = 9;
        assert_eq!(Message::decode(&buf), Err(Error::Truncated));
    }

    #[test]
    fn body_length_beyond_plaintext_is_rejected() {
        let mut buf = round_trip(Body::Request, PADDED_PLAINTEXT_LEN);
        buf[25..27].copy_from_slice(&230u16.to_be_bytes());
        assert_eq!(Message::decode(&buf), Err(Error::Truncated));
    }

    #[test]
    fn undefined_response_kind_is_rejected() {
        let mut buf = round_trip(
            Body::Response(Response {
                request: [9; 16],
                kind: ResponseKind::Manual,
            }),
            PADDED_PLAINTEXT_LEN,
        );
        buf[43] = 3;
        assert_eq!(Message::decode(&buf), Err(Error::InvalidValue));
    }

    fn longest_part() -> ContactPart {
        ContactPart {
            contact: [4; 32],
            transfer: [5; 16],
            index: 1,
            count: 3,
            data: PartData::new(&[6; MAX_PART_LEN]).unwrap(),
        }
    }

    fn sync_bodies() -> [Body; 9] {
        [
            Body::Device(DeviceAnnounce {
                reader: [1; 16],
                name: DeviceName::new(&"n".repeat(MAX_DEVICE_NAME_LEN)).unwrap(),
            }),
            Body::DeviceRemoved([2; 16]),
            Body::Requested(Requested {
                contact: [3; 32],
                request: [4; 16],
                at: 5,
            }),
            Body::Responded(Responded {
                contact: [3; 32],
                request: [4; 16],
                kind: ResponseKind::Manual,
                at: 6,
            }),
            Body::Responded(Responded {
                contact: [3; 32],
                request: [4; 16],
                kind: ResponseKind::Reported(Reason::new(&"g".repeat(MAX_REASON_LEN)).unwrap()),
                at: 7,
            }),
            Body::RulesSet(RulesSet {
                contact: [3; 32],
                rules: Rules::DEFAULT,
            }),
            Body::ContactPart(longest_part()),
            Body::ContactRemoved([8; 32]),
            Body::StateShared(StateShared {
                reason: Reason::new(&"p".repeat(MAX_REASON_LEN)).unwrap(),
                until: 9,
            }),
        ]
    }

    #[test]
    fn sync_types_round_trip_and_are_sync() {
        for body in sync_bodies() {
            assert!(body.is_sync());
            round_trip(body, PADDED_PLAINTEXT_LEN);
        }
        assert!(!Body::End.is_sync());
    }

    #[test]
    fn sync_layouts_match_documentation() {
        let [
            device,
            removed,
            requested,
            manual,
            reported,
            rules_set,
            part,
            contact_removed,
            state_shared,
        ] = sync_bodies().map(|body| round_trip(body, PADDED_PLAINTEXT_LEN));

        assert_eq!(device[0], 64);
        assert_eq!(&device[25..27], &[0, 49]);
        assert_eq!(&device[27..43], &[1; 16]);
        assert_eq!(device[43], 32);
        assert_eq!(&device[44..76], &[b'n'; 32]);

        assert_eq!(removed[0], 65);
        assert_eq!(&removed[25..27], &[0, 16]);
        assert_eq!(&removed[27..43], &[2; 16]);

        assert_eq!(requested[0], 66);
        assert_eq!(&requested[25..27], &[0, 56]);
        assert_eq!(&requested[27..59], &[3; 32]);
        assert_eq!(&requested[59..75], &[4; 16]);
        assert_eq!(&requested[75..83], &5u64.to_be_bytes());

        assert_eq!(manual[0], 67);
        assert_eq!(&manual[25..27], &[0, 58]);
        assert_eq!(&manual[75..77], &[1, 0]);
        assert_eq!(&manual[77..85], &6u64.to_be_bytes());
        assert_eq!(&reported[25..27], &[0, 122]);
        assert_eq!(&reported[75..77], &[2, 64]);
        assert_eq!(&reported[77..141], &[b'g'; 64]);
        assert_eq!(&reported[141..149], &7u64.to_be_bytes());

        assert_eq!(rules_set[0], 68);
        assert_eq!(&rules_set[25..27], &[0, 42]);
        assert_eq!(&rules_set[59..69], &[0, 0, 1, 104, 0, 0, 5, 160, 0, 7]);

        // The longest part fills the plaintext of the ordinary form exactly.
        assert_eq!(part[0], 69);
        assert_eq!(&part[25..27], &[0, 229]);
        assert_eq!(&part[27..59], &[4; 32]);
        assert_eq!(&part[59..75], &[5; 16]);
        assert_eq!(&part[75..78], &[1, 3, 178]);
        assert!(part[78..].iter().all(|&b| b == 6));

        assert_eq!(contact_removed[0], 70);
        assert_eq!(&contact_removed[25..27], &[0, 32]);
        assert_eq!(&contact_removed[27..59], &[8; 32]);

        assert_eq!(state_shared[0], 71);
        assert_eq!(&state_shared[25..27], &[0, 73]);
        assert_eq!(state_shared[27], 64);
        assert_eq!(&state_shared[28..92], &[b'p'; 64]);
        assert_eq!(&state_shared[92..100], &9u64.to_be_bytes());
    }

    #[test]
    fn state_cleared_is_an_empty_sync_body() {
        assert!(Body::StateCleared.is_sync());
        assert_eq!(Body::StateCleared.sync_contact(), None);
        let buf = round_trip(Body::StateCleared, PADDED_PLAINTEXT_LEN);
        assert_eq!(buf[0], 72);
        assert_eq!(&buf[25..27], &[0, 0]);
    }

    #[test]
    fn short_sync_bodies_are_rejected_longer_accepted() {
        for body in sync_bodies() {
            let buf = round_trip(body.clone(), PADDED_PLAINTEXT_LEN);
            let len = u16::from_be_bytes([buf[25], buf[26]]);
            let mut short = buf;
            short[25..27].copy_from_slice(&(len - 1).to_be_bytes());
            assert_eq!(Message::decode(&short), Err(Error::Truncated), "{body:?}");
            if usize::from(len) + HEADER_LEN < PADDED_PLAINTEXT_LEN {
                let mut long = buf;
                long[25..27].copy_from_slice(&(len + 1).to_be_bytes());
                assert_eq!(Message::decode(&long).unwrap().body, body);
            }
        }
    }

    #[test]
    fn oversized_part_and_name_are_rejected() {
        let mut buf = round_trip(Body::ContactPart(longest_part()), PADDED_PLAINTEXT_LEN);
        // A length above 178 cannot be stored, even if the body were longer.
        buf[77] = 179;
        assert_eq!(Message::decode(&buf), Err(Error::Truncated));
        buf[25..27].copy_from_slice(&200u16.to_be_bytes());
        buf[77] = 179;
        assert_eq!(Message::decode(&buf[..]), Err(Error::Truncated));
        let mut big = [0u8; 300];
        big[..PADDED_PLAINTEXT_LEN].copy_from_slice(&buf);
        big[25..27].copy_from_slice(&230u16.to_be_bytes());
        assert_eq!(Message::decode(&big), Err(Error::TooLong));
        assert_eq!(
            contact_part_ids(&big),
            Some(([4; 32], [5; 16])),
            "ids stay readable to discard the transfer"
        );

        let mut buf = round_trip(sync_bodies()[0].clone(), PADDED_PLAINTEXT_LEN);
        buf[43] = 33;
        buf[25..27].copy_from_slice(&50u16.to_be_bytes());
        assert_eq!(Message::decode(&buf), Err(Error::TooLong));
        assert_eq!(PartData::new(&[0; MAX_PART_LEN + 1]), Err(Error::TooLong));
    }

    #[test]
    fn contact_part_ids_only_for_contact_parts() {
        let buf = round_trip(Body::ContactRemoved([8; 32]), PADDED_PLAINTEXT_LEN);
        assert_eq!(contact_part_ids(&buf), None);
        let mut buf = round_trip(Body::ContactPart(longest_part()), PADDED_PLAINTEXT_LEN);
        buf[25..27].copy_from_slice(&40u16.to_be_bytes());
        assert_eq!(contact_part_ids(&buf), None);
    }

    fn channel_bodies() -> [Body; 20] {
        use crate::satellite::{ContactFlags, ContactSettings, ContactSettingsFor, Part, Preset};
        use crate::satellite::{Settings, SettingsFor};
        let reason = || Reason::new(&"g".repeat(MAX_REASON_LEN)).unwrap();
        let responded = |kind| Responded {
            contact: [3; 32],
            request: [4; 16],
            kind,
            at: 7,
        };
        let requested = Requested {
            contact: [3; 32],
            request: [4; 16],
            at: 5,
        };
        let state = StateShared {
            reason: reason(),
            until: 9,
        };
        let settings = Settings {
            waiting_minutes: 20,
            preset: Some(Preset {
                minutes: 480,
                share: true,
                reason: reason(),
            }),
        };
        let contact_settings = ContactSettings {
            contact: [3; 32],
            of: ContactFlags {
                reported: true,
                ask_button: 1,
            },
        };
        [
            Body::SatellitePart(Part {
                reader: [1; 16],
                transfer: [5; 16],
                index: 1,
                count: 2,
                data: PartData::new(&[6; MAX_PART_LEN]).unwrap(),
            }),
            Body::SatelliteSettings(SettingsFor {
                reader: [1; 16],
                settings,
            }),
            Body::SatelliteContactSettings(ContactSettingsFor {
                reader: [1; 16],
                settings: contact_settings,
            }),
            Body::SatAnswered(responded(ResponseKind::Manual)),
            Body::SatAnswered(responded(ResponseKind::Reported(reason()))),
            Body::SatAsked(requested.clone()),
            Body::SatStateShared(state.clone()),
            Body::SatStateCleared,
            Body::HomeContactPart(longest_part()),
            Body::HomeResponded(responded(ResponseKind::Reported(reason()))),
            Body::HomeRequested(requested),
            Body::HomeRules(RulesSet {
                contact: [3; 32],
                rules: Rules::DEFAULT,
            }),
            Body::HomeContactSettings(contact_settings),
            Body::HomeSettings(settings),
            Body::HomeSettings(Settings::DEFAULT),
            Body::HomeStateShared(state),
            Body::HomeStateCleared,
            Body::HomeSendToken([8; 32]),
            Body::HomeResponded(responded(ResponseKind::Manual)),
            Body::SatAnswered(responded(ResponseKind::Manual)),
        ]
    }

    #[test]
    fn channel_and_satellite_types_round_trip() {
        for body in channel_bodies() {
            round_trip(body.clone(), PADDED_PLAINTEXT_LEN);
            let kind = body.kind().unwrap();
            let classes = [body.is_sync(), body.is_up(), body.is_down()];
            match kind {
                73..=75 => assert_eq!(classes, [true, false, false], "{body:?}"),
                80..=83 => assert_eq!(classes, [false, true, false], "{body:?}"),
                88..=96 => assert_eq!(classes, [false, false, true], "{body:?}"),
                _ => panic!("{kind}"),
            }
        }
        // Old types keep their class; unknown ones have none.
        assert!(Body::StateCleared.is_sync() && !Body::StateCleared.is_up());
        assert!(!Body::Request.is_sync() && !Body::Request.is_down());
        for unknown in [76u8, 79, 84, 87, 97, 99, 100, 127, 128] {
            let body = Body::Unknown(unknown);
            assert!(!body.is_sync() && !body.is_up() && !body.is_down());
            assert_eq!(
                kind::is_sync(unknown),
                matches!(unknown, 76..=79 | 100..=127),
                "{unknown}"
            );
            assert_eq!(kind::is_up(unknown), matches!(unknown, 84..=87));
            assert_eq!(kind::is_down(unknown), matches!(unknown, 97..=99));
        }
    }

    #[test]
    fn channel_layouts_match_documentation() {
        let bufs = channel_bodies().map(|body| round_trip(body, PADDED_PLAINTEXT_LEN));
        let [
            part,
            settings,
            contact_settings,
            answered,
            answered_reported,
            asked,
            sat_shared,
            sat_cleared,
            home_part,
            home_responded,
            home_requested,
            home_rules,
            home_contact_settings,
            home_settings,
            default_settings,
            home_shared,
            home_cleared,
            send_token,
            _,
            _,
        ] = bufs;

        assert_eq!(part[0], 73);
        assert_eq!(&part[25..27], &[0, 213]);
        assert_eq!(&part[27..43], &[1; 16]);
        assert_eq!(&part[43..59], &[5; 16]);
        assert_eq!(&part[59..62], &[1, 2, 178]);

        assert_eq!(settings[0], 74);
        assert_eq!(&settings[25..27], &[0, 16 + 72]);
        assert_eq!(&settings[27..43], &[1; 16]);
        assert_eq!(&settings[43..45], &20u16.to_be_bytes());
        assert_eq!(&settings[45..49], &480u32.to_be_bytes());
        assert_eq!(&settings[49..51], &[1, 64]);

        assert_eq!(contact_settings[0], 75);
        assert_eq!(&contact_settings[25..27], &[0, 50]);
        assert_eq!(&contact_settings[43..75], &[3; 32]);
        assert_eq!(&contact_settings[75..77], &[1, 1]);

        // Up and down bodies are those of their sync counterparts.
        assert_eq!(answered[0], 80);
        assert_eq!(&answered[25..27], &[0, 58]);
        assert_eq!(&answered[75..77], &[1, 0]);
        assert_eq!(&answered[77..85], &7u64.to_be_bytes());
        assert_eq!(&answered_reported[25..27], &[0, 122]);
        assert_eq!(&answered_reported[1..], &home_responded[1..]);
        assert_eq!(home_responded[0], 89);
        assert_eq!(asked[0], 81);
        assert_eq!(&asked[25..27], &[0, 56]);
        assert_eq!(&asked[1..], &home_requested[1..]);
        assert_eq!(home_requested[0], 90);
        assert_eq!(sat_shared[0], 82);
        assert_eq!(&sat_shared[25..27], &[0, 73]);
        assert_eq!(&sat_shared[1..], &home_shared[1..]);
        assert_eq!(home_shared[0], 94);
        assert_eq!((sat_cleared[0], &sat_cleared[25..27]), (83, &[0u8, 0][..]));
        assert_eq!(
            (home_cleared[0], &home_cleared[25..27]),
            (95, &[0u8, 0][..])
        );

        assert_eq!(home_part[0], 88);
        assert_eq!(&home_part[25..27], &[0, 229]);
        assert_eq!(home_rules[0], 91);
        assert_eq!(&home_rules[25..27], &[0, 42]);
        assert_eq!(home_contact_settings[0], 92);
        assert_eq!(&home_contact_settings[25..27], &[0, 34]);
        assert_eq!(&home_contact_settings[59..61], &[1, 1]);
        assert_eq!(home_settings[0], 93);
        assert_eq!(&home_settings[25..27], &[0, 72]);
        assert_eq!(&default_settings[25..27], &[0, 8]);
        assert_eq!(&default_settings[27..35], &[0, 15, 0, 0, 0, 0, 0, 0]);
        assert_eq!(send_token[0], 96);
        assert_eq!(&send_token[25..27], &[0, 32]);
        assert_eq!(&send_token[27..59], &[8; 32]);
    }

    #[test]
    fn short_channel_bodies_are_rejected_longer_accepted() {
        for body in channel_bodies() {
            let buf = round_trip(body.clone(), PADDED_PLAINTEXT_LEN);
            let len = u16::from_be_bytes([buf[25], buf[26]]);
            if len > 0 {
                let mut short = buf;
                short[25..27].copy_from_slice(&(len - 1).to_be_bytes());
                assert_eq!(Message::decode(&short), Err(Error::Truncated), "{body:?}");
            }
            if usize::from(len) + HEADER_LEN < PADDED_PLAINTEXT_LEN {
                let mut long = buf;
                long[25..27].copy_from_slice(&(len + 1).to_be_bytes());
                assert_eq!(Message::decode(&long).unwrap().body, body);
            }
        }
    }

    #[test]
    fn undefined_flags_in_settings_are_rejected() {
        let bufs = channel_bodies().map(|body| round_trip(body, PADDED_PLAINTEXT_LEN));
        let mut share = bufs[13];
        share[33] = 2;
        assert_eq!(Message::decode(&share), Err(Error::InvalidValue));
        let mut reported = bufs[12];
        reported[59] = 7;
        assert_eq!(Message::decode(&reported), Err(Error::InvalidValue));
        let mut part = bufs[0];
        part[61] = 179;
        part[25..27].copy_from_slice(&229u16.to_be_bytes());
        assert_eq!(Message::decode(&part), Err(Error::TooLong));
    }

    #[test]
    fn too_small_buffer_is_reported() {
        let mut buf = [0; HEADER_LEN + 10];
        assert_eq!(
            message(Body::Hello(longest_hello())).encode(&mut buf),
            Err(Error::TooLong)
        );
    }
}
