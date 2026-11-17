//! The background path on Android: what the Kotlin layer asks of the core
//! after a wake signal or a reaction to a notification, without Tauri.
//!
//! Every function takes the path of the state file and returns plain data
//! that `jni_bridge` hands over as JSON. They live outside `jni_bridge` so
//! that they compile and run in the tests on the desktop as well.

use std::path::Path;

use lz_client::ctx::{Ctx, Status};
use lz_client::i18n::Arg;
use lz_client::relay::Http;
use serde::Serialize;

use crate::answer::Answer;
use crate::commands::{new_ctx, runtime};

/// A request of a contact that waits for an answer.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct OpenRequest {
    pub name: String,
    /// Sending time of the request, Unix seconds. With the name it tells
    /// requests of the same contact apart.
    pub since: u64,
    pub expires_at: u64,
    pub title: String,
}

/// The open requests, and what went wrong on the way, if anything.
#[derive(Debug, Default, Serialize)]
pub struct Requests {
    pub requests: Vec<OpenRequest>,
    /// Answers to own requests that this fetch received.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<Answer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Seconds until a reported answer is due; `Some(0)` if one is due now
    /// (after a fetch: it could not be sent). `None`: nothing pending. Only
    /// set by [`fetch`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_wait: Option<u64>,
}

/// Result of an answer or of setting the push address.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Outcome {
    pub ok: bool,
    /// Worth trying again later, for example after a network error.
    pub retry: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Outcome {
    const DONE: Self = Self {
        ok: true,
        retry: false,
        error: None,
    };

    fn failed(error: String, retry: bool) -> Self {
        Self {
            ok: false,
            retry,
            error: Some(error),
        }
    }
}

/// Text of the notification and the full-screen display.
pub fn title(name: &str) -> String {
    crate::lang::texts().text("app-alert-title", &[("name", Arg::Text(name))])
}

/// A command context whose text output is thrown away; the Kotlin layer
/// keeps its own log.
fn quiet_ctx(path: &Path) -> Result<Ctx<Http>, String> {
    new_ctx(path.to_path_buf(), Box::new(std::io::sink()))
}

fn list(ctx: &Ctx<Http>) -> Result<Vec<OpenRequest>, String> {
    let overview = ctx.overview().map_err(|error| error.to_string())?;
    Ok(overview
        .contacts
        .into_iter()
        .filter(|contact| contact.status == Status::Paired)
        .filter_map(|contact| {
            let open = contact.incoming?;
            Some(OpenRequest {
                title: title(&contact.name),
                name: contact.name,
                since: open.since,
                expires_at: open.expires_at,
            })
        })
        .collect())
}

/// The open requests as the state file has them, without the network.
pub fn open_requests(path: &Path) -> Requests {
    match quiet_ctx(path).and_then(|ctx| list(&ctx)) {
        Ok(requests) => Requests {
            requests,
            ..Requests::default()
        },
        Err(error) => Requests {
            error: Some(error),
            ..Requests::default()
        },
    }
}

/// Fetches what waits at the relays, then lists the open requests. A failed
/// fetch still lists what the state file has.
pub fn fetch(path: &Path) -> Requests {
    let fetched = quiet_ctx(path).and_then(|mut ctx| {
        let notices = runtime()?
            .block_on(ctx.sync())
            .map_err(|error| error.to_string())?;
        let (t, now) = (ctx.texts(), ctx.now());
        Ok(notices
            .iter()
            .filter_map(|notice| Answer::from_notice(t, notice, now))
            .collect())
    });
    let mut requests = open_requests(path);
    requests.report_wait = report_wait(path);
    match fetched {
        Ok(answers) => requests.answers = answers,
        Err(error) => {
            requests.error.get_or_insert(error);
        }
    }
    requests
}

/// Seconds until the next reported answer is due, see [`Requests::report_wait`].
pub fn report_wait(path: &Path) -> Option<u64> {
    quiet_ctx(path).ok()?.report_wait().ok()?
}

/// A state report from another app.
#[derive(Debug, Clone, Copy)]
pub enum External<'a> {
    /// `minutes` as the sender gave it: Tasker and Home Assistant send text.
    /// `share`: also on the other own devices (M8c).
    Set {
        reason: Option<&'a str>,
        minutes: Option<&'a str>,
        share: bool,
    },
    Clear {
        share: bool,
    },
}

/// What became of a state report from another app.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Applied {
    pub applied: bool,
    /// As [`Requests::report_wait`], after the change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_wait: Option<u64>,
    /// A message to the other own devices waits: fetch soon to send it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub send: bool,
}

impl Applied {
    const DROPPED: Self = Self {
        applied: false,
        report_wait: None,
        send: false,
    };
}

/// Applies a state report from another app. Dropped without a trace in the
/// answer if reports are off, the token is missing or wrong, or the values
/// are not valid.
pub fn report_from_app(path: &Path, token: Option<&str>, call: External<'_>) -> Applied {
    let Some(token) = token else {
        return Applied::DROPPED;
    };
    let Ok(mut ctx) = quiet_ctx(path) else {
        return Applied::DROPPED;
    };
    if !ctx.report_token_matches(token).unwrap_or(false) {
        return Applied::DROPPED;
    }
    let (done, share) = match call {
        External::Set {
            reason,
            minutes,
            share,
        } => {
            let minutes = minutes.and_then(|text| text.trim().parse::<u32>().ok());
            match (reason, minutes) {
                (Some(reason), Some(minutes)) => (ctx.set_state(reason, minutes, share), share),
                _ => return Applied::DROPPED,
            }
        }
        External::Clear { share } => (ctx.clear_state(share), share),
    };
    if done.is_err() {
        return Applied::DROPPED;
    }
    Applied {
        applied: true,
        report_wait: ctx.report_wait().ok().flatten(),
        send: share,
    }
}

/// What a battery check did, and whether the battery setting is on.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct BatteryChecked {
    #[serde(flatten)]
    pub applied: Applied,
    /// The setting is on (or could not be read): keep checking.
    pub battery: bool,
}

/// Checks the battery level against the battery setting (M8c). `applied`
/// if the battery state was handed over or ended; then `send` is set too.
pub fn check_battery(path: &Path, percent: u8, charging: bool) -> BatteryChecked {
    let Ok(mut ctx) = quiet_ctx(path) else {
        return BatteryChecked {
            applied: Applied::DROPPED,
            battery: true,
        };
    };
    let applied = match ctx.check_battery(percent, charging) {
        Ok(lz_core::report::BatteryChange::None) | Err(_) => Applied::DROPPED,
        Ok(_) => Applied {
            applied: true,
            report_wait: ctx.report_wait().ok().flatten(),
            send: true,
        },
    };
    let battery = ctx.battery_reason().map_or(true, |reason| reason.is_some());
    BatteryChecked { applied, battery }
}

fn is_open(path: &Path, name: &str, since: u64) -> Result<bool, String> {
    let ctx = quiet_ctx(path)?;
    Ok(list(&ctx)?
        .iter()
        .any(|open| open.name == name && open.since == since))
}

/// Answers the request of `name` sent at `since`. A request that is no
/// longer open (answered elsewhere, expired, contact gone) counts as done.
pub fn respond(path: &Path, name: &str, since: u64) -> Outcome {
    match is_open(path, name, since) {
        Ok(false) => return Outcome::DONE,
        Ok(true) => {}
        Err(error) => return Outcome::failed(error, false),
    }
    let answered = quiet_ctx(path).and_then(|mut ctx| {
        runtime()?
            .block_on(ctx.respond(name))
            .map_err(|error| error.to_string())
    });
    match answered {
        Ok(()) => Outcome::DONE,
        // Still open after a failure: most likely the network.
        Err(error) => match is_open(path, name, since) {
            Ok(false) => Outcome::DONE,
            Ok(true) => Outcome::failed(error, true),
            Err(_) => Outcome::failed(error, false),
        },
    }
}

/// Sets or removes the push address. Never worth a retry here: once stored,
/// every later fetch sends it until all mailboxes have it.
pub fn set_push_url(path: &Path, url: Option<&str>) -> Outcome {
    let result = quiet_ctx(path).and_then(|mut ctx| {
        runtime()?
            .block_on(ctx.set_push_url(url))
            .map_err(|error| error.to_string())
    });
    match result {
        Ok(()) => Outcome::DONE,
        Err(error) => Outcome::failed(error, false),
    }
}

#[cfg(test)]
mod tests {
    use lz_client::ctx;
    use lz_client::i18n::Language;
    use lz_core::store::{JsonFile, Store};

    use super::{
        Applied, External, Outcome, check_battery, fetch, open_requests, report_from_app,
        report_wait, respond, set_push_url, title,
    };

    fn device() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.json");
        ctx::init(
            &JsonFile::new(&path),
            None,
            None,
            Language::De,
            &mut std::io::sink(),
        )
        .unwrap();
        (dir, path)
    }

    #[test]
    fn no_device_is_an_error_not_a_crash() {
        crate::lang::set_current(Language::De);
        let dir = tempfile::tempdir().unwrap();
        let requests = open_requests(&dir.path().join("missing.json"));
        assert!(requests.requests.is_empty());
        assert!(requests.error.is_some_and(|e| e.contains("Kein Gerät")));
    }

    #[test]
    fn a_request_that_is_not_open_counts_as_answered() {
        let (_dir, path) = device();
        assert!(open_requests(&path).requests.is_empty());
        assert_eq!(respond(&path, "Anna", 1_794_600_000), Outcome::DONE);
    }

    #[test]
    fn invalid_push_url_is_not_retried() {
        let (_dir, path) = device();
        let outcome = set_push_url(&path, Some("http://push.example/up"));
        assert!(!outcome.ok && !outcome.retry);
        // No mailboxes yet: a valid address needs no relay at all.
        assert_eq!(
            set_push_url(&path, Some("https://push.example/up")),
            Outcome::DONE
        );
    }

    #[test]
    fn fetch_without_mailboxes_lists_nothing() {
        let (_dir, path) = device();
        let requests = fetch(&path);
        assert!(requests.requests.is_empty());
        assert_eq!(requests.error, None);
    }

    #[test]
    fn title_names_the_contact() {
        crate::lang::set_current(Language::De);
        assert_eq!(title("Anna"), "Anna fragt nach einem Lebenszeichen");
    }

    fn state_of(path: &std::path::Path) -> Option<String> {
        let device = JsonFile::new(path).load().unwrap();
        device
            .reports
            .state
            .map(|s| format!("{}:{}", s.reason, s.until))
    }

    fn set<'a>(reason: &'a str, minutes: &'a str) -> External<'a> {
        External::Set {
            reason: Some(reason),
            minutes: Some(minutes),
            share: false,
        }
    }

    fn token_of(path: &std::path::Path) -> String {
        let mut ctx = super::quiet_ctx(path).unwrap();
        ctx.enable_report_token().unwrap()
    }

    #[test]
    fn report_with_the_right_token_sets_the_state() {
        let (_dir, path) = device();
        let token = token_of(&path);
        let applied = report_from_app(&path, Some(&token), set("asleep", "480"));
        assert!(applied.applied);
        assert_eq!(applied.report_wait, None);
        assert!(state_of(&path).unwrap().starts_with("asleep:"));
        // Cleared again with the same token.
        let applied = report_from_app(&path, Some(&token), External::Clear { share: false });
        assert!(applied.applied);
        assert_eq!(state_of(&path), None);
    }

    #[test]
    fn battery_check_follows_the_setting() {
        let (_dir, path) = device();
        let off = check_battery(&path, 1, false);
        assert!(!off.applied.applied && !off.battery);
        super::quiet_ctx(&path)
            .unwrap()
            .set_battery(Some("Phone empty"))
            .unwrap();
        let low = check_battery(&path, 2, false);
        assert!(low.applied.applied && low.applied.send && low.battery);
        assert!(state_of(&path).unwrap().starts_with("Phone empty:"));
        let json = serde_json::to_value(&low).unwrap();
        assert_eq!(json["applied"], true);
        assert_eq!(json["battery"], true);
        assert!(check_battery(&path, 6, false).applied.applied);
        assert_eq!(state_of(&path), None);
    }

    #[test]
    fn minutes_as_text_or_number_alike() {
        let (_dir, path) = device();
        let token = token_of(&path);
        for text in ["60", " 60 "] {
            assert!(report_from_app(&path, Some(&token), set("asleep", text)).applied);
        }
        // Not a number, zero, too long: dropped, the old state stays.
        for text in ["", "abc", "6.5", "-1", "0", "99999999"] {
            assert_eq!(
                report_from_app(&path, Some(&token), set("other", text)),
                Applied::DROPPED,
                "{text}"
            );
        }
        let missing = External::Set {
            reason: Some("other"),
            minutes: None,
            share: false,
        };
        assert_eq!(
            report_from_app(&path, Some(&token), missing),
            Applied::DROPPED
        );
        let no_reason = External::Set {
            reason: None,
            minutes: Some("5"),
            share: false,
        };
        assert_eq!(
            report_from_app(&path, Some(&token), no_reason),
            Applied::DROPPED
        );
        assert!(state_of(&path).unwrap().starts_with("asleep:"));
    }

    #[test]
    fn wrong_missing_or_switched_off_token_changes_nothing() {
        let (_dir, path) = device();
        // Switch off from the start: any token, any call.
        assert_eq!(
            report_from_app(&path, Some(&"0".repeat(64)), set("asleep", "60")),
            Applied::DROPPED
        );
        let token = token_of(&path);
        assert!(report_from_app(&path, Some(&token), set("asleep", "60")).applied);
        let before = state_of(&path);
        let wrong = "f".repeat(64);
        for given in [None, Some(wrong.as_str()), Some(""), Some("short")] {
            assert_eq!(
                report_from_app(&path, given, set("other", "5")),
                Applied::DROPPED
            );
            assert_eq!(
                report_from_app(&path, given, External::Clear { share: false }),
                Applied::DROPPED
            );
        }
        assert_eq!(state_of(&path), before);
        // Switched off: the token that worked before no longer does.
        super::quiet_ctx(&path)
            .unwrap()
            .disable_report_token()
            .unwrap();
        assert_eq!(
            report_from_app(&path, Some(&token), External::Clear { share: false }),
            Applied::DROPPED
        );
        assert_eq!(state_of(&path), before);
    }

    #[test]
    fn wait_is_none_without_a_pending_report() {
        let (_dir, path) = device();
        assert_eq!(report_wait(&path), None);
        assert_eq!(fetch(&path).report_wait, None);
        assert_eq!(
            report_wait(std::path::Path::new("/nonexistent/x.json")),
            None
        );
    }
}
