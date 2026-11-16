//! The background path on Android: what the Kotlin layer asks of the core
//! after a wake signal or a reaction to a notification, without Tauri.
//!
//! Every function takes the path of the state file and returns plain data
//! that `jni_bridge` hands over as JSON. They live outside `jni_bridge` so
//! that they compile and run in the tests on the desktop as well.

use std::path::Path;

use lz_client::ctx::{Ctx, Status};
use lz_client::relay::Http;
use serde::Serialize;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    format!("{name} fragt nach einem Lebenszeichen")
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
            error: None,
        },
        Err(error) => Requests {
            requests: Vec::new(),
            error: Some(error),
        },
    }
}

/// Fetches what waits at the relays, then lists the open requests. A failed
/// fetch still lists what the state file has.
pub fn fetch(path: &Path) -> Requests {
    let fetched = quiet_ctx(path).and_then(|mut ctx| {
        runtime()?
            .block_on(ctx.sync())
            .map(drop)
            .map_err(|error| error.to_string())
    });
    let mut requests = open_requests(path);
    if let Err(error) = fetched {
        requests.error.get_or_insert(error);
    }
    requests
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
    use lz_core::store::JsonFile;

    use super::{Outcome, fetch, open_requests, respond, set_push_url, title};

    fn device() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.json");
        ctx::init(
            &JsonFile::new(&path),
            None,
            None,
            crate::commands::LANG,
            &mut std::io::sink(),
        )
        .unwrap();
        (dir, path)
    }

    #[test]
    fn no_device_is_an_error_not_a_crash() {
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
        assert_eq!(title("Anna"), "Anna fragt nach einem Lebenszeichen");
    }
}
