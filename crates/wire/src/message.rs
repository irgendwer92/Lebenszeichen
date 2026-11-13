//! Plaintext of a message: common header, typed body, zero padding.
//!
//! The plaintext is what `crypto_box` or `crypto_box_seal` encrypts. Its
//! length is fixed by the envelope form: [`PADDED_PLAINTEXT_LEN`] for the
//! ordinary form and [`SEALED_PLAINTEXT_LEN`] for the sealed form.
//!
//! [`PADDED_PLAINTEXT_LEN`]: crate::PADDED_PLAINTEXT_LEN
//! [`SEALED_PLAINTEXT_LEN`]: crate::SEALED_PLAINTEXT_LEN

use crate::{Error, MailboxId, MessageId, PublicKey, Reader, Reason, RelayUrl, Token, Writer};

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
    /// A type this version does not know. Receivers ignore it; it cannot be
    /// encoded.
    Unknown(u8),
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
            Self::Unknown(_) => return Err(Error::InvalidValue),
        })
    }

    fn write(&self, w: &mut Writer<'_>) -> Result<(), Error> {
        match self {
            Self::Hello(hello) => {
                w.put(&hello.public_key)?;
                w.put(&hello.mailbox)?;
                w.put(&hello.send_token)?;
                w.text(&hello.relay)
            }
            Self::Rules(rules) => {
                w.put(&rules.min_interval_minutes.to_be_bytes())?;
                w.put(&rules.expiry_minutes.to_be_bytes())?;
                w.put(&rules.weekly_limit.to_be_bytes())
            }
            Self::Response(response) => {
                w.put(&response.request)?;
                match &response.kind {
                    ResponseKind::Manual => w.put(&[MANUAL, 0]),
                    ResponseKind::Reported(reason) => {
                        w.put(&[REPORTED])?;
                        w.text(reason)
                    }
                }
            }
            Self::HelloOk | Self::Request | Self::End => Ok(()),
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
            kind::RULES => Self::Rules(Rules {
                min_interval_minutes: r.u32()?,
                expiry_minutes: r.u32()?,
                weekly_limit: r.u16()?,
            }),
            kind::REQUEST => Self::Request,
            kind::RESPONSE => {
                let request = r.array()?;
                let kind = match r.u8()? {
                    MANUAL => {
                        // The reason must be empty; a non-empty one is ignored.
                        r.text::<255>()?;
                        ResponseKind::Manual
                    }
                    REPORTED => ResponseKind::Reported(r.text()?),
                    _ => return Err(Error::InvalidValue),
                };
                Self::Response(Response { request, kind })
            }
            kind::END => Self::End,
            other => Self::Unknown(other),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Body, HEADER_LEN, Hello, Message, Response, ResponseKind, Rules, kind};
    use crate::{
        Error, MAX_REASON_LEN, MAX_RELAY_URL_LEN, PADDED_PLAINTEXT_LEN, Reason, RelayUrl,
        SEALED_PLAINTEXT_LEN,
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
        ] {
            round_trip(body, PADDED_PLAINTEXT_LEN);
        }
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

    #[test]
    fn too_small_buffer_is_reported() {
        let mut buf = [0; HEADER_LEN + 10];
        assert_eq!(
            message(Body::Hello(longest_hello())).encode(&mut buf),
            Err(Error::TooLong)
        );
    }
}
