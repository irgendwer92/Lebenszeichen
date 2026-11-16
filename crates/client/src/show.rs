//! Human-readable output: times in local time, rules, events.

use chrono::{DateTime, Datelike, Local, TimeZone};
use lz_core::wire::{ResponseKind, Rules};

const WEEKDAYS: [&str; 7] = ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"];

/// A point in time relative to `now`: "heute 18:00", "morgen 07:30",
/// "Mo 16.11. 18:00", with the year if it differs.
pub fn when(at: u64, now: u64) -> String {
    if at == u64::MAX {
        return "nie".into();
    }
    let (Some(at), Some(now)) = (local(at), local(now)) else {
        return format!("{at} (Unix-Zeit)");
    };
    let time = at.format("%H:%M");
    let days = (at.date_naive() - now.date_naive()).num_days();
    match days {
        -1 => format!("gestern {time}"),
        0 => format!("heute {time}"),
        1 => format!("morgen {time}"),
        _ if at.year() == now.year() => format!(
            "{} {} {time}",
            WEEKDAYS[at.weekday().num_days_from_monday() as usize],
            at.format("%d.%m.")
        ),
        _ => format!(
            "{} {} {time}",
            WEEKDAYS[at.weekday().num_days_from_monday() as usize],
            at.format("%d.%m.%Y")
        ),
    }
}

fn local(at: u64) -> Option<DateTime<Local>> {
    Local.timestamp_opt(i64::try_from(at).ok()?, 0).single()
}

/// A duration in minutes, in hours where that is exact.
pub fn minutes(m: u32) -> String {
    if m != 0 && m.is_multiple_of(60) {
        format!("{} h", m / 60)
    } else {
        format!("{m} min")
    }
}

/// The request rules in one line.
pub fn rules(rules: &Rules) -> String {
    format!(
        "Mindestabstand {}, Ablauf nach {}, höchstens {} pro Woche",
        minutes(rules.min_interval_minutes),
        minutes(rules.expiry_minutes),
        rules.weekly_limit
    )
}

/// A response as the asking side sees it.
pub fn response(kind: &ResponseKind, at: u64, now: u64) -> String {
    match kind {
        ResponseKind::Manual => format!("✓ {}", when(at, now)),
        ResponseKind::Reported(reason) => reported(reason),
    }
}

pub fn reported(reason: impl std::fmt::Display) -> String {
    format!("„{reason}“ (gemeldet)")
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

/// Turns a hint into the wording of a client, or into nothing.
pub type Hints = fn(&Hint<'_>) -> Option<String>;

/// For clients that show no hints.
pub fn no_hints(_: &Hint<'_>) -> Option<String> {
    None
}

/// The hints that apply, each on a line of its own after `text`.
pub fn with_hints(text: String, hints: Hints, wanted: &[Hint<'_>]) -> String {
    wanted
        .iter()
        .filter_map(hints)
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
}

/// "auf „Handy“", or "auf einem anderen Gerät" if it is not known which.
fn on(device: &Option<String>) -> String {
    match device {
        Some(name) => format!("auf „{name}“"),
        None => "auf einem anderen Gerät".into(),
    }
}

impl Notice {
    pub fn is_dropped(&self) -> bool {
        matches!(self, Self::Dropped { .. })
    }

    /// Text of the notice, with times relative to `now` and the hints of
    /// the client.
    pub fn text(&self, now: u64, hints: Hints) -> String {
        match self {
            Self::Accepted { name, code } => with_hints(
                format!(
                    "{name} hat die Einladung angenommen. Prüfcode: {code}\n\
                     Bestätige nur, wenn er mit dem Prüfcode bei {name} übereinstimmt."
                ),
                hints,
                &[Hint::Confirm(name), Hint::Reject(name)],
            ),
            Self::Paired { name } => format!("{name} hat die Kopplung bestätigt."),
            Self::Rules { name, rules: r } => {
                format!("Regeln von {name} für deine Anfragen: {}", rules(r))
            }
            Self::Request { name } => with_hints(
                format!("{name} fragt nach einem Lebenszeichen."),
                hints,
                &[Hint::Respond(name)],
            ),
            Self::Response { name, kind, at } => {
                format!("{name}: {}", response(kind, *at, now))
            }
            Self::Ended { name } => format!("{name} hat die Verbindung beendet."),
            Self::Rejected { name } => format!(
                "{name} hat die Kopplung abgelehnt, vermutlich weil der Prüfcode nicht übereinstimmte. \
                 Für einen neuen Versuch braucht es eine neue Einladung."
            ),
            Self::SecondAcceptance { name, paired } => {
                let (what, undo) = if *paired {
                    (
                        format!(
                            "Prüfe mit {name}, ob der Prüfcode wirklich übereinstimmte. \
                             Wenn nicht, entferne den Kontakt und lade neu ein."
                        ),
                        Hint::Remove(name),
                    )
                } else {
                    (
                        format!(
                            "Vergleiche den Prüfcode mit {name} besonders sorgfältig, bevor du \
                             bestätigst. Stimmt er nicht, lehne ab und lade neu ein."
                        ),
                        Hint::Reject(name),
                    )
                };
                with_hints(
                    format!(
                        "Achtung: Die Einladung an {name} wurde ein weiteres Mal angenommen, \
                         der Code ist also noch jemand anderem bekannt. {what}"
                    ),
                    hints,
                    &[undo],
                )
            }
            Self::Dropped { name, why } => match name {
                Some(name) => format!("(verworfen, {name}: {why})"),
                None => format!("(verworfen: {why})"),
            },
            Self::Problem(text) => text.clone(),
            Self::OwnRequested { name, device } => {
                format!("Anfrage an {name} {} gesendet.", on(device))
            }
            Self::OwnResponded { name, device, kind } => match kind {
                ResponseKind::Manual => {
                    format!("Anfrage von {name} {} beantwortet.", on(device))
                }
                ResponseKind::Reported(reason) => format!(
                    "Zur Anfrage von {name} {} gemeldet: {}",
                    on(device),
                    reported(reason)
                ),
            },
            Self::OwnRules {
                name,
                device,
                rules: r,
            } => format!("Regeln für {name} {} geändert: {}", on(device), rules(r)),
            Self::OwnContactAdded { name, device } => {
                format!(
                    "{name} wurde {} gekoppelt und ist jetzt auch hier.",
                    on(device)
                )
            }
            Self::OwnContactRemoved { name, device } => {
                format!("{name} wurde {} entfernt.", on(device))
            }
            Self::DeviceAdded { name } => format!(
                "Eigenes Gerät „{name}“ ist dabei. Hast du es nicht selbst hinzugefügt, \
                 entferne es und kopple deine Kontakte neu."
            ),
            Self::DeviceRemoved { name } => match name {
                Some(name) => format!("Eigenes Gerät „{name}“ wurde entfernt."),
                None => "Ein eigenes Gerät wurde entfernt.".into(),
            },
            Self::Removed => "Dieses Gerät wurde entfernt.".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{minutes, when};

    #[test]
    fn relative_days() {
        let now = 1_794_650_000;
        assert!(when(now, now).starts_with("heute "));
        assert!(when(now + 24 * 3600, now).starts_with("morgen "));
        assert!(when(now - 24 * 3600, now).starts_with("gestern "));
        assert!(when(now + 3 * 24 * 3600, now).contains('.'));
        assert_eq!(when(u64::MAX, now), "nie");
    }

    #[test]
    fn durations() {
        assert_eq!(minutes(360), "6 h");
        assert_eq!(minutes(90), "90 min");
        assert_eq!(minutes(0), "0 min");
    }
}
