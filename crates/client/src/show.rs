//! Human-readable output: times in local time, rules, events.

use jiff::tz::TimeZone;
use jiff::{Timestamp, ZonedDateTime};
use lz_core::wire::{ResponseKind, Rules};

use crate::i18n::Texts;

/// A point in time relative to `now`: "heute 18:00", "morgen 07:30",
/// "Mo 16.11. 18:00", with the year if it differs. Times are on the 24-hour
/// clock in every language.
pub fn when(t: Texts, at: u64, now: u64) -> String {
    if at == u64::MAX {
        return tr!(t, "time-never");
    }
    let (Some(local_at), Some(local_now)) = (local(at), local(now)) else {
        return tr!(t, "time-unix", at = &at.to_string());
    };
    let Ok(days) = local_at.date().since(local_now.date()) else {
        return tr!(t, "time-unix", at = &at.to_string());
    };
    let time = format!("{:02}:{:02}", local_at.hour(), local_at.minute());
    let id = match days.get_days() {
        -1 => "time-yesterday",
        0 => "time-today",
        1 => "time-tomorrow",
        _ if local_at.year() == local_now.year() => "time-date",
        _ => "time-date-year",
    };
    let weekday = t.text(
        &format!("time-weekday-{}", local_at.weekday().to_monday_one_offset()),
        &[],
    );
    let month_name = t.text(&format!("time-month-{}", local_at.month()), &[]);
    tr!(
        t,
        id,
        time = &time,
        weekday = &weekday,
        day = &local_at.day().to_string(),
        day2 = &format!("{:02}", local_at.day()),
        month2 = &format!("{:02}", local_at.month()),
        month_name = &month_name,
        year = &local_at.year().to_string()
    )
}

/// The point in time in the time zone of the system; UTC if the system names
/// none.
fn local(at: u64) -> Option<ZonedDateTime> {
    let at = Timestamp::from_second(i64::try_from(at).ok()?).ok()?;
    Some(at.to_zoned_datetime(TimeZone::system()))
}

/// A duration in minutes, in hours where that is exact.
pub fn minutes(t: Texts, m: u32) -> String {
    if m != 0 && m.is_multiple_of(60) {
        tr!(t, "time-hours", count = &(m / 60).to_string())
    } else {
        tr!(t, "time-minutes", count = &m.to_string())
    }
}

/// The request rules in one line.
pub fn rules(t: Texts, rules: &Rules) -> String {
    tr!(
        t,
        "rules-line",
        interval = &minutes(t, rules.min_interval_minutes),
        expiry = &minutes(t, rules.expiry_minutes),
        weekly = &rules.weekly_limit.to_string()
    )
}

/// A response as the asking side sees it.
pub fn response(t: Texts, kind: &ResponseKind, at: u64, now: u64) -> String {
    match kind {
        ResponseKind::Manual => response_manual(t, at, now),
        ResponseKind::Reported(reason) => reported(t, reason),
    }
}

/// A manual response: a check mark and the time.
pub fn response_manual(t: Texts, at: u64, now: u64) -> String {
    tr!(t, "response-manual", at = &when(t, at, now))
}

pub fn reported(t: Texts, reason: impl std::fmt::Display) -> String {
    tr!(t, "response-reported", reason = &reason.to_string())
}

/// The reason of the own reported state, quoted.
pub fn reported_state(t: Texts, reason: &str) -> String {
    tr!(t, "state-reason", reason = reason)
}

/// " (shared)" or " (from another device)" after a state, or nothing.
pub fn state_origin(t: Texts, shared: bool, handed: bool) -> String {
    if handed {
        tr!(t, "state-origin-handed")
    } else if shared {
        tr!(t, "state-origin-shared")
    } else {
        String::new()
    }
}

/// Shown next to every verification code of a pairing: someone who controls
/// the channel of the invitation could pass the digits on as well.
pub fn compare_hint(t: Texts) -> String {
    tr!(t, "notice-compare-hint")
}

/// A point where a client may tell the person what to do next.
///
/// The shared texts are complete sentences without commands; each client
/// turns a hint into its own wording (`lz` names a command, the desktop app
/// has buttons and mostly says nothing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint<'a> {
    /// There is no device yet.
    Init,
    /// Inviting needs an own relay with access token.
    InitWithAccessToken,
    /// The other side can invite with a voucher.
    InviteWithVoucher,
    /// Something will arrive that has to be fetched.
    Fetch,
    /// Fetching goes on until the person stops it.
    StopListening,
    Confirm(&'a str),
    Reject(&'a str),
    Respond(&'a str),
    Remove(&'a str),
    /// Invite this contact again.
    Invite(&'a str),
    /// Invite someone; there are no contacts yet.
    InviteSomeone,
    /// Enter the shown device code on an existing device.
    AddDevice,
    /// Start adding this device again.
    JoinAgain,
    /// Add a further device to this one.
    JoinSomeDevice,
}

/// Turns a hint into the wording of a client in a language, or into
/// nothing.
pub type Hints = fn(&Hint<'_>, Texts) -> Option<String>;

/// For clients that show no hints.
pub fn no_hints(_: &Hint<'_>, _: Texts) -> Option<String> {
    None
}

/// The hints that apply, each on a line of its own after `text`.
pub fn with_hints(text: String, hints: Hints, t: Texts, wanted: &[Hint<'_>]) -> String {
    wanted
        .iter()
        .filter_map(|hint| hints(hint, t))
        .fold(text, |text, hint| format!("{text}\n{hint}"))
}

/// Something that happened while fetching messages, for display and tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// Someone accepted an invitation; the code must be compared.
    Accepted {
        name: String,
        code: String,
    },
    /// The inviting side revealed its secret value: the code can be
    /// compared now. If `paired`, it had confirmed already.
    Revealed {
        name: String,
        code: String,
        paired: bool,
    },
    /// The revealed value does not fit the invitation: someone interferes.
    /// The pairing was ended and the own mailbox deleted.
    RevealMismatch {
        name: String,
    },
    /// No valid `reveal` arrived in time, or the pairing was begun with an
    /// older version. It was ended and the own mailbox deleted.
    PairingFailed {
        name: String,
    },
    /// The other side confirmed the pairing.
    Paired {
        name: String,
    },
    Rules {
        name: String,
        rules: Rules,
    },
    Request {
        name: String,
    },
    Response {
        name: String,
        kind: ResponseKind,
        at: u64,
    },
    /// This device answered a request of the contact with the reported
    /// state, because the waiting time passed without a manual answer.
    Reported {
        name: String,
        reason: String,
    },
    /// The other side ended the contact, by `end` or because its mailbox is
    /// gone.
    Ended {
        name: String,
    },
    /// The other side rejected the pairing.
    Rejected {
        name: String,
    },
    /// A further acceptance of an invitation arrived.
    SecondAcceptance {
        name: String,
        paired: bool,
    },
    /// A message was dropped. Shown only with `--verbose`.
    Dropped {
        name: Option<String>,
        why: String,
    },
    /// A problem with a relay or mailbox that the person should know about.
    Problem(String),
    /// Another own device sent a request to this contact. `device` is its
    /// name if there is only one other device.
    OwnRequested {
        name: String,
        device: Option<String>,
    },
    /// Another own device answered or reported on a request of this
    /// contact.
    OwnResponded {
        name: String,
        device: Option<String>,
        kind: ResponseKind,
    },
    /// Another own device changed how often this contact may ask.
    OwnRules {
        name: String,
        device: Option<String>,
        rules: Rules,
    },
    /// A contact paired on another own device arrived here.
    OwnContactAdded {
        name: String,
        device: Option<String>,
    },
    /// Another own device ended this contact; it is gone here as well.
    OwnContactRemoved {
        name: String,
        device: Option<String>,
    },
    /// Another own device handed on its state (`Some` reason) or ended
    /// it (`None`).
    OwnState {
        reason: Option<String>,
        device: Option<String>,
    },
    /// Another own device announced itself, or changed its name.
    DeviceAdded {
        name: String,
    },
    /// Another own device was removed.
    DeviceRemoved {
        name: Option<String>,
    },
    /// This device was removed by another one; its state is deleted.
    Removed,
    /// Removing a device could only unregister it on the mailboxes of
    /// these contacts, as their relays lack `reader_tokens` (M10): it
    /// could register itself again there.
    RemovalWeak {
        names: Vec<String>,
    },
    /// Removing a device also revoked this many readers that no own device
    /// knew of (M10).
    HiddenReaders {
        count: usize,
    },
    /// Requests of these contacts were open when a device was removed; it
    /// knows them and could answer them, so the person should (M10).
    AnswerOpenRequests {
        names: Vec<String>,
    },
    /// The relay no longer accepts this device's token for the own mailbox
    /// of this contact (`None`: the sync mailbox), and it could not take a
    /// token of its own: its access was withdrawn, as the shared token is
    /// dropped when another device is removed. It has to be added again
    /// (M10).
    LostAccess {
        name: Option<String>,
    },
    /// A satellite set up on another own device is linked here now (M10).
    SatelliteAdded {
        name: String,
    },
    /// The channel of this satellite is gone: it was removed, here or on
    /// another own device. This device forgot it.
    SatelliteGone {
        name: String,
    },
    /// The satellite answered a request of this contact.
    SatelliteAnswered {
        name: String,
        satellite: String,
        kind: ResponseKind,
    },
    /// The satellite asked this contact for a sign of life.
    SatelliteAsked {
        name: String,
        satellite: String,
    },
    /// The satellite shared its state (`Some` reason) or ended it (`None`).
    SatelliteState {
        reason: Option<String>,
        satellite: String,
    },
    /// Another own device changed the settings of this satellite.
    SatelliteSettings {
        satellite: String,
    },
    /// These contacts were not given to the satellite: their relay lacks
    /// `reader_tokens`.
    SatelliteOldRelay {
        satellite: String,
        names: Vec<String>,
    },
    /// These contacts were not given to the satellite: it would need more
    /// than its 3 relays.
    SatelliteTooManyRelays {
        satellite: String,
        names: Vec<String>,
    },
}

/// "auf „Handy“", or "auf einem anderen Gerät" if it is not known which.
fn on(t: Texts, device: &Option<String>) -> String {
    match device {
        Some(name) => tr!(t, "notice-on-device", name = name),
        None => tr!(t, "notice-on-other-device"),
    }
}

impl Notice {
    pub fn is_dropped(&self) -> bool {
        matches!(self, Self::Dropped { .. })
    }

    /// Text of the notice in a language, with times relative to `now` and
    /// the hints of the client.
    pub fn text(&self, t: Texts, now: u64, hints: Hints) -> String {
        match self {
            Self::Accepted { name, code } => with_hints(
                tr!(t, "notice-accepted", name = name, code = code),
                hints,
                t,
                &[Hint::Confirm(name), Hint::Reject(name)],
            ),
            Self::Revealed {
                name,
                code,
                paired: false,
            } => with_hints(
                tr!(t, "notice-revealed", name = name, code = code),
                hints,
                t,
                &[Hint::Reject(name)],
            ),
            Self::Revealed {
                name,
                code,
                paired: true,
            } => with_hints(
                tr!(t, "notice-revealed-paired", name = name, code = code),
                hints,
                t,
                &[Hint::Remove(name)],
            ),
            Self::RevealMismatch { name } => tr!(t, "notice-reveal-mismatch", name = name),
            Self::PairingFailed { name } => tr!(t, "notice-pairing-failed", name = name),
            Self::Paired { name } => tr!(t, "notice-paired", name = name),
            Self::Rules { name, rules: r } => {
                tr!(t, "notice-rules", name = name, rules = &rules(t, r))
            }
            Self::Request { name } => with_hints(
                tr!(t, "notice-request", name = name),
                hints,
                t,
                &[Hint::Respond(name)],
            ),
            Self::Response { name, kind, at } => tr!(
                t,
                "notice-response",
                name = name,
                response = &response(t, kind, *at, now)
            ),
            Self::Reported { name, reason } => with_hints(
                tr!(
                    t,
                    "notice-reported",
                    name = name,
                    reason = &reported(t, reason)
                ),
                hints,
                t,
                &[Hint::Respond(name)],
            ),
            Self::Ended { name } => tr!(t, "notice-ended", name = name),
            Self::Rejected { name } => tr!(t, "notice-rejected", name = name),
            Self::SecondAcceptance { name, paired } => {
                let (id, undo) = if *paired {
                    ("notice-second-acceptance-paired", Hint::Remove(name))
                } else {
                    ("notice-second-acceptance", Hint::Reject(name))
                };
                with_hints(
                    tr!(
                        t,
                        "notice-second-acceptance-warning",
                        name = name,
                        what = &tr!(t, id, name = name)
                    ),
                    hints,
                    t,
                    &[undo],
                )
            }
            Self::Dropped { name, why } => match name {
                Some(name) => tr!(t, "notice-dropped-from", name = name, why = why),
                None => tr!(t, "notice-dropped", why = why),
            },
            Self::Problem(text) => text.clone(),
            Self::OwnRequested { name, device } => {
                tr!(
                    t,
                    "notice-own-requested",
                    name = name,
                    device = &on(t, device)
                )
            }
            Self::OwnResponded { name, device, kind } => match kind {
                ResponseKind::Manual => {
                    tr!(
                        t,
                        "notice-own-responded",
                        name = name,
                        device = &on(t, device)
                    )
                }
                ResponseKind::Reported(reason) => tr!(
                    t,
                    "notice-own-reported",
                    name = name,
                    device = &on(t, device),
                    reason = &reported(t, reason)
                ),
            },
            Self::OwnRules {
                name,
                device,
                rules: r,
            } => tr!(
                t,
                "notice-own-rules",
                name = name,
                device = &on(t, device),
                rules = &rules(t, r)
            ),
            Self::OwnContactAdded { name, device } => {
                tr!(
                    t,
                    "notice-own-contact-added",
                    name = name,
                    device = &on(t, device)
                )
            }
            Self::OwnContactRemoved { name, device } => {
                tr!(
                    t,
                    "notice-own-contact-removed",
                    name = name,
                    device = &on(t, device)
                )
            }
            Self::OwnState {
                reason: Some(reason),
                device,
            } => tr!(
                t,
                "notice-own-state",
                reason = reason,
                device = &on(t, device)
            ),
            Self::OwnState {
                reason: None,
                device,
            } => tr!(t, "notice-own-state-cleared", device = &on(t, device)),
            Self::DeviceAdded { name } => tr!(t, "notice-device-added", name = name),
            Self::DeviceRemoved { name } => match name {
                Some(name) => tr!(t, "notice-device-removed", name = name),
                None => tr!(t, "notice-device-removed-unnamed"),
            },
            Self::Removed => tr!(t, "notice-removed"),
            Self::RemovalWeak { names } => {
                tr!(t, "notice-removal-weak", names = &names.join(", "))
            }
            Self::HiddenReaders { count } => tr!(t, "notice-hidden-readers", count = *count),
            Self::AnswerOpenRequests { names } => {
                tr!(t, "notice-answer-open-requests", names = &names.join(", "))
            }
            Self::LostAccess { name } => match name {
                Some(name) => tr!(t, "notice-lost-access", name = name),
                None => tr!(t, "notice-lost-access-unnamed"),
            },
            Self::SatelliteAdded { name } => tr!(t, "notice-satellite-added", name = name),
            Self::SatelliteGone { name } => tr!(t, "notice-satellite-gone", name = name),
            Self::SatelliteAnswered {
                name,
                satellite,
                kind,
            } => match kind {
                ResponseKind::Manual => tr!(
                    t,
                    "notice-satellite-answered",
                    name = name,
                    satellite = satellite
                ),
                ResponseKind::Reported(reason) => tr!(
                    t,
                    "notice-satellite-reported",
                    name = name,
                    satellite = satellite,
                    reason = &reported(t, reason)
                ),
            },
            Self::SatelliteAsked { name, satellite } => tr!(
                t,
                "notice-satellite-asked",
                name = name,
                satellite = satellite
            ),
            Self::SatelliteState {
                reason: Some(reason),
                satellite,
            } => tr!(
                t,
                "notice-satellite-state",
                reason = reason,
                satellite = satellite
            ),
            Self::SatelliteState {
                reason: None,
                satellite,
            } => tr!(t, "notice-satellite-state-cleared", satellite = satellite),
            Self::SatelliteSettings { satellite } => {
                tr!(t, "notice-satellite-settings", satellite = satellite)
            }
            Self::SatelliteOldRelay { satellite, names } => tr!(
                t,
                "notice-satellite-old-relay",
                satellite = satellite,
                names = &names.join(", ")
            ),
            Self::SatelliteTooManyRelays { satellite, names } => tr!(
                t,
                "notice-satellite-too-many-relays",
                satellite = satellite,
                names = &names.join(", ")
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{minutes, when};
    use crate::i18n::Language;

    /// A day in the middle of the year, so that neighbours share the year.
    fn now() -> u64 {
        1_794_650_000
    }

    #[test]
    fn relative_days_german() {
        let (t, now) = (Language::De.texts(), now());
        assert!(when(t, now, now).starts_with("heute "));
        assert!(when(t, now + 24 * 3600, now).starts_with("morgen "));
        assert!(when(t, now - 24 * 3600, now).starts_with("gestern "));
        assert!(when(t, now + 3 * 24 * 3600, now).contains('.'));
        assert_eq!(when(t, u64::MAX, now), "nie");
    }

    #[test]
    fn relative_days_english() {
        let (t, now) = (Language::En.texts(), now());
        assert!(when(t, now, now).starts_with("today "));
        assert!(when(t, now + 24 * 3600, now).starts_with("tomorrow "));
        assert!(when(t, now - 24 * 3600, now).starts_with("yesterday "));
        assert_eq!(when(t, u64::MAX, now), "never");
    }

    #[test]
    fn dates_and_clock_times() {
        let at = |y, m, d, h, min| {
            jiff::civil::date(y, m, d)
                .at(h, min, 0, 0)
                .to_zoned_datetime(jiff::tz::TimeZone::system())
                .unwrap()
                .timestamp()
                .as_second() as u64
        };
        let now = at(2026, 11, 16, 9, 0);
        let (de, en) = (Language::De.texts(), Language::En.texts());
        // Monday 23 November 2026, in the afternoon and at 07:05.
        assert_eq!(when(de, at(2026, 11, 23, 16, 7), now), "Mo 23.11. 16:07");
        assert_eq!(when(en, at(2026, 11, 23, 16, 7), now), "Mon 23 Nov 16:07");
        assert_eq!(when(de, at(2026, 11, 3, 7, 5), now), "Di 03.11. 07:05");
        assert_eq!(when(en, at(2026, 11, 3, 7, 5), now), "Tue 3 Nov 07:05");
        assert_eq!(when(de, at(2026, 11, 16, 16, 7), now), "heute 16:07");
        assert_eq!(when(en, at(2026, 11, 16, 16, 7), now), "today 16:07");
        assert_eq!(when(de, at(2026, 11, 15, 23, 59), now), "gestern 23:59");
        assert_eq!(when(en, at(2026, 11, 15, 23, 59), now), "yesterday 23:59");
        assert_eq!(when(de, at(2027, 1, 4, 18, 0), now), "Mo 04.01.2027 18:00");
        assert_eq!(when(en, at(2027, 1, 4, 18, 0), now), "Mon 4 Jan 2027 18:00");
    }

    #[test]
    fn durations() {
        for language in [Language::De, Language::En] {
            let t = language.texts();
            assert_eq!(minutes(t, 360), "6 h");
            assert_eq!(minutes(t, 90), "90 min");
            assert_eq!(minutes(t, 0), "0 min");
        }
    }
}
