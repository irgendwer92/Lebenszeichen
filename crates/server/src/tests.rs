//! The page API and the token of the local interface, over loopback TCP
//! and a socket in a temporary directory; no relay is contacted.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use lz_client::i18n::Language;
use lz_client::local;
use lz_core::contact::ContactState;
use lz_core::pairing::{Address, PendingInvitation, accept};
use lz_core::store::{Device, JsonFile, Store, StoredContact};
use lz_core::wire::{Reason, RelayUrl};
use rand_core::OsRng;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixStream};

use crate::cli::{Args, local, shared};
use crate::page::{self, allowed, percent_decode};
use crate::server::Shared;

/// A running page and socket on a temporary directory.
struct Harness {
    dir: tempfile::TempDir,
    shared: Arc<Shared>,
    page: SocketAddr,
    tasks: Vec<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Harness {
    async fn start(allow: Vec<IpAddr>) -> Self {
        lz_client::install_tls_provider();
        let dir = tempfile::tempdir().unwrap();
        let args = Args {
            state: dir.path().join("device.json"),
            socket: dir.path().join("run").join("lz.sock"),
            http: None,
            page: None,
            page_allow: allow.clone(),
            relay: Some("https://relay.example/".to_owned()),
        };
        let shared = shared(&args, Language::En).unwrap();
        let socket = local::bind(&args.socket).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let page = listener.local_addr().unwrap();
        let tasks = vec![
            tokio::spawn(local::serve(socket, local(&shared))),
            tokio::spawn(page::serve(listener, Arc::clone(&shared), allow)),
        ];
        Self {
            dir,
            shared,
            page,
            tasks,
        }
    }

    async fn allowed() -> Self {
        Self::start(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]).await
    }

    fn store(&self) -> JsonFile {
        JsonFile::new(&self.shared.state)
    }

    /// Creates the state file with a paired contact "Anna B".
    fn set_up(&self) {
        let now = lz_client::ctx::system_now();
        let mut device = Device::new(&mut OsRng);
        device.contacts.push(paired_contact("Anna B", now));
        self.store().create(&device).unwrap();
    }

    async fn call(&self, method: &str, path: &str, body: &str) -> (u16, Value, String) {
        let stream = TcpStream::connect(self.page).await.unwrap();
        exchange(stream, method, path, "", body).await
    }

    async fn socket(&self, method: &str, token: &str, body: &str) -> u16 {
        let stream = UnixStream::connect(self.dir.path().join("run").join("lz.sock"))
            .await
            .unwrap();
        let headers = format!("Authorization: Bearer {token}\r\n");
        exchange(stream, method, "/state", &headers, body).await.0
    }
}

/// Sends one request and returns status, body as JSON (`Null` if empty or
/// not JSON) and the head.
async fn exchange<S>(
    mut stream: S,
    method: &str,
    path: &str,
    headers: &str,
    body: &str,
) -> (u16, Value, String)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{headers}\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).await.unwrap();
    let answer = String::from_utf8(answer).unwrap();
    let (head, body) = answer.split_once("\r\n\r\n").unwrap();
    let status = head[9..12].parse().unwrap();
    let body = serde_json::from_str(body).unwrap_or(Value::Null);
    (status, body, head.to_ascii_lowercase())
}

/// A contact paired on both sides, as the own side stores it.
fn paired_contact(name: &str, now: u64) -> StoredContact {
    let relay = RelayUrl::new("https://relay.example").unwrap();
    let address = |fill| Address {
        relay,
        mailbox: [fill; 32],
        send_token: [fill + 1; 32],
    };
    let (pending, invitation) = PendingInvitation::create(address(1), None, now, &mut OsRng);
    let (own, hello) = accept(&invitation, address(3), now, &mut OsRng).unwrap();
    let mut peer = ContactState::new(pending.receive_hello(&hello, now).unwrap());
    let mut own = ContactState::new(own);
    own.receive(&peer.contact.reveal(now, &mut OsRng).unwrap(), now)
        .unwrap();
    let [hello_ok, _] = peer.confirm(now, &mut OsRng).unwrap();
    own.receive(&hello_ok, now).unwrap();
    assert!(own.is_paired());
    StoredContact {
        name: name.to_owned(),
        own_relay: relay,
        read_token: [9; 32],
        state: own,
    }
}

fn token_of(store: &JsonFile) -> String {
    let token = store.load().unwrap().reports.token.unwrap();
    token.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn status_before_and_after_setup() {
    let h = Harness::allowed().await;
    let (status, body, _) = h.call("GET", "/api/status", "").await;
    assert_eq!(status, 200);
    assert_eq!(body["setup"], "none");
    assert_eq!(body["default_relay"], "https://relay.example");
    for key in ["join", "device", "state", "token", "http"] {
        assert!(body[key].is_null(), "{key}: {body}");
    }
    assert_eq!(body["contacts"], serde_json::json!([]));
    assert_eq!(
        body["socket"],
        h.dir.path().join("run/lz.sock").display().to_string()
    );

    h.set_up();
    let now = lz_client::ctx::system_now();
    h.store()
        .update(|device: &mut Device| {
            device
                .set_state(Reason::new("asleep").unwrap(), 60, now)
                .map_err(std::io::Error::other)
        })
        .unwrap();
    h.shared.ensure_token().unwrap();
    let (status, body, _) = h.call("GET", "/api/status", "").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["setup"], "ready");
    assert!(body["join"].is_null());
    assert_eq!(body["device"]["others"], serde_json::json!([]));
    assert!(body["device"]["name"].is_string());
    assert_eq!(body["state"]["reason"], "asleep");
    assert_eq!(body["state"]["until"], now + 3600);
    let text = body["state"]["text"].as_str().unwrap();
    assert!(
        text.starts_with("Reported state: ") && text.contains("asleep"),
        "{text}"
    );
    assert_eq!(body["token"], token_of(&h.store()));
    assert_eq!(body["token"].as_str().unwrap().len(), 64);
    let anna = &body["contacts"][0];
    assert_eq!(anna["name"], "Anna B");
    assert_eq!(anna["paired"], true);
    assert_eq!(anna["reports_allowed"], false);
    assert_eq!(anna["report_delay_minutes"], 15);
    assert!(anna["incoming"].is_null());
}

#[tokio::test]
async fn texts_files_and_headers() {
    let h = Harness::allowed().await;
    let (status, body, _) = h.call("GET", "/api/texts", "").await;
    assert_eq!(status, 200);
    assert_eq!(body["lang"], "en");
    assert!(body["texts"].is_object());
    let (status, _, head) = h.call("GET", "/", "").await;
    assert_eq!(status, 200);
    assert!(head.contains("content-type: text/html"));
    assert!(head.contains("content-security-policy: default-src 'self'; img-src 'self' data:"));
    assert!(head.contains("x-content-type-options: nosniff"));
    assert!(head.contains("referrer-policy: no-referrer"));
    assert!(
        h.call("GET", "/page.js", "")
            .await
            .2
            .contains("text/javascript")
    );
    assert_eq!(h.call("GET", "/other", "").await.0, 404);
    assert_eq!(h.call("POST", "/api/status", "").await.0, 405);
    let big = format!(r#"{{"name": "{}"}}"#, "x".repeat(5000));
    assert_eq!(h.call("POST", "/api/join", &big).await.0, 413);
}

#[test]
fn the_allow_list_is_exact() {
    let supervisor = IpAddr::V4(Ipv4Addr::new(172, 30, 32, 2));
    let allow = [supervisor];
    assert!(allowed(&allow, supervisor));
    assert!(allowed(
        &allow,
        IpAddr::V6(Ipv4Addr::new(172, 30, 32, 2).to_ipv6_mapped())
    ));
    assert!(!allowed(&allow, IpAddr::V4(Ipv4Addr::new(172, 30, 32, 3))));
    assert!(!allowed(&allow, IpAddr::V4(Ipv4Addr::LOCALHOST)));
    assert!(!allowed(&allow, IpAddr::V6(Ipv6Addr::LOCALHOST)));
    assert!(!allowed(&[], supervisor));
}

#[tokio::test]
async fn other_peers_are_refused() {
    let h = Harness::start(vec![IpAddr::V4(Ipv4Addr::new(172, 30, 32, 2))]).await;
    let mut stream = TcpStream::connect(h.page).await.unwrap();
    // The connection is closed without an answer; writing may fail.
    let _ = stream
        .write_all(b"GET /api/status HTTP/1.1\r\nHost: x\r\n\r\n")
        .await;
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer).await;
    assert!(answer.is_empty());
}

#[tokio::test]
async fn a_renewed_token_applies_at_once() {
    let h = Harness::allowed().await;
    let set = r#"{"reason": "asleep", "minutes": 10}"#;
    // Before setup nothing is accepted.
    assert_eq!(h.socket("GET", &"00".repeat(32), "").await, 401);
    h.set_up();
    h.shared.ensure_token().unwrap();
    let old = token_of(&h.store());
    assert_eq!(h.socket("POST", &old, set).await, 204);

    let (status, body, _) = h.call("POST", "/api/token/renew", "").await;
    assert_eq!(status, 200);
    let new = body["token"].as_str().unwrap().to_owned();
    assert_ne!(new, old);
    assert_eq!(new, token_of(&h.store()));
    assert_eq!(h.socket("POST", &old, set).await, 401);
    assert_eq!(h.socket("POST", &new, set).await, 204);
    assert_eq!(h.socket("DELETE", &new, "").await, 204);
}

#[tokio::test]
async fn the_token_is_created_once() {
    let h = Harness::allowed().await;
    h.set_up();
    h.shared.ensure_token().unwrap();
    let first = token_of(&h.store());
    h.shared.ensure_token().unwrap();
    assert_eq!(token_of(&h.store()), first);
}

#[tokio::test]
async fn reports_can_be_allowed_per_contact() {
    let h = Harness::allowed().await;
    let path = "/api/contacts/Anna%20B/reports";
    let body = r#"{"allowed": true, "delay_minutes": 30}"#;
    assert_eq!(h.call("PUT", path, body).await.0, 409);
    h.set_up();
    let (status, answer, _) = h.call("PUT", path, body).await;
    assert_eq!(status, 204, "{answer}");
    let (_, status_body, _) = h.call("GET", "/api/status", "").await;
    assert_eq!(status_body["contacts"][0]["reports_allowed"], true);
    assert_eq!(status_body["contacts"][0]["report_delay_minutes"], 30);

    let (status, answer, _) = h.call("PUT", "/api/contacts/Nobody/reports", body).await;
    assert_eq!(status, 400);
    assert!(answer["error"].as_str().unwrap().contains("Nobody"));
    let (status, _, _) = h
        .call("PUT", path, r#"{"allowed": true, "delay_minutes": 0}"#)
        .await;
    assert_eq!(status, 400);
    assert_eq!(h.call("PUT", path, "not json").await.0, 400);
    assert_eq!(h.call("GET", path, "").await.0, 405);
}

#[test]
fn names_are_url_decoded() {
    assert_eq!(percent_decode("Anna%20B").as_deref(), Some("Anna B"));
    assert_eq!(percent_decode("J%C3%BCrgen").as_deref(), Some("Jürgen"));
    assert_eq!(percent_decode("a%2Fb").as_deref(), Some("a/b"));
    assert_eq!(percent_decode("bad%2"), None);
    assert_eq!(percent_decode("%ff"), None);
}

#[tokio::test]
async fn the_state_can_be_cleared() {
    let h = Harness::allowed().await;
    assert_eq!(h.call("DELETE", "/api/state", "").await.0, 409);
    h.set_up();
    let now = lz_client::ctx::system_now();
    h.store()
        .update(|device: &mut Device| {
            device
                .set_state(Reason::new("asleep").unwrap(), 60, now)
                .map_err(std::io::Error::other)
        })
        .unwrap();
    assert_eq!(h.call("DELETE", "/api/state", "").await.0, 204);
    assert!(h.store().load().unwrap().current_state(now).is_none());
    let (_, body, _) = h.call("GET", "/api/status", "").await;
    assert!(body["state"].is_null());
}

#[tokio::test]
async fn joining_is_refused_with_a_state_file() {
    let h = Harness::allowed().await;
    h.set_up();
    let (status, body, _) = h
        .call(
            "POST",
            "/api/join",
            r#"{"relay": "https://relay.example", "name": "HA"}"#,
        )
        .await;
    assert_eq!(status, 409);
    assert!(body["error"].as_str().unwrap().contains("already a device"));
    // Nothing runs: answering is refused, cancelling always works.
    assert_eq!(
        h.call("POST", "/api/join/answer", r#"{"matches": true}"#)
            .await
            .0,
        409
    );
    assert_eq!(h.call("POST", "/api/join/cancel", "").await.0, 204);
    let (_, body, _) = h.call("GET", "/api/status", "").await;
    assert_eq!(body["setup"], "ready");
    assert!(body["join"].is_null());
}

#[tokio::test]
async fn joining_needs_a_valid_body() {
    let h = Harness::allowed().await;
    assert_eq!(h.call("POST", "/api/join", r#"{"name": 5}"#).await.0, 400);
    assert_eq!(h.call("POST", "/api/join", "").await.0, 400);
    let (_, body, _) = h.call("GET", "/api/status", "").await;
    assert_eq!(body["setup"], "none");
}

#[tokio::test]
async fn a_running_join_shows_its_steps_and_is_cancelled() {
    let h = Harness::allowed().await;
    // A port nobody listens on: the flow keeps waiting for the relay.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let body = format!(r#"{{"relay": "{relay}", "name": "Home Assistant"}}"#);
    assert_eq!(h.call("POST", "/api/join", &body).await.0, 202);
    assert_eq!(h.call("POST", "/api/join", &body).await.0, 409);
    let mut last = Value::Null;
    for _ in 0..100 {
        last = h.call("GET", "/api/status", "").await.1;
        if last["join"]["code"].is_string() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(last["setup"], "joining", "{last}");
    let join = &last["join"];
    assert_eq!(join["step"], "device_code");
    assert!(join["code"].as_str().unwrap().starts_with("LZD"));
    let width = join["qr"]["width"].as_u64().unwrap();
    assert_eq!(
        join["qr"]["dark"].as_array().unwrap().len() as u64,
        width * width
    );
    assert!(join["error"].is_null());
    let progress = join["progress"].as_array().unwrap();
    assert!(
        progress
            .iter()
            .all(|line| !line.as_str().unwrap().contains("LZD"))
    );

    for _ in 0..2 {
        assert_eq!(h.call("POST", "/api/join/cancel", "").await.0, 204);
        let (_, body, _) = h.call("GET", "/api/status", "").await;
        assert_eq!(body["setup"], "none");
        assert!(body["join"].is_null());
    }
    assert!(!h.shared.state.exists());
    // A new flow may start right away.
    assert_eq!(h.call("POST", "/api/join", &body).await.0, 202);
    assert_eq!(h.call("POST", "/api/join/cancel", "").await.0, 204);
}
