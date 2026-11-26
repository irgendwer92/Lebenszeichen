//! Integration tests: the relay runs in-process on a random port with a
//! temporary SQLite file and a manual clock. No test leaves the machine; wake
//! signals are recorded instead of sent.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use lz_relay::ids::{self, MailboxId};
use lz_relay::push::{PushPolicy, Waker};
use lz_relay::store::{SCHEMA_VERSION, Store};
use lz_relay::{AppState, ManualClock};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const ACCESS_TOKEN: [u8; 32] = [9; 32];
const START: i64 = 1_800_000_000;
const ENVELOPE: [u8; lz_wire::ENVELOPE_LEN] = [0x42; lz_wire::ENVELOPE_LEN];

#[derive(Default)]
struct RecordingWaker(Mutex<Vec<String>>);

impl Waker for RecordingWaker {
    fn wake(&self, url: String) {
        self.0.lock().unwrap().push(url);
    }
}

impl RecordingWaker {
    fn take(&self) -> Vec<String> {
        let mut urls = std::mem::take(&mut *self.0.lock().unwrap());
        urls.sort();
        urls
    }
}

struct Relay {
    base: String,
    ws_url: String,
    clock: Arc<ManualClock>,
    state: AppState,
    waker: Arc<RecordingWaker>,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
    /// Runs the server of a relay that can be stopped, see
    /// [`Relay::start_stoppable`].
    server: Option<ServerRuntime>,
}

/// A runtime that is shut down without waiting, also when dropped inside
/// a test.
struct ServerRuntime(Option<tokio::runtime::Runtime>);

impl Drop for ServerRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

struct Mailbox {
    id: String,
    read: String,
    send: String,
}

impl Mailbox {
    fn raw_id(&self) -> MailboxId {
        ids::decode(&self.id).unwrap()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn token() -> ([u8; 32], String) {
    let raw: [u8; 32] = ids::random();
    (raw, ids::encode(&raw))
}

fn reader_id(n: u8) -> String {
    ids::encode(&[n; 16])
}

impl Relay {
    async fn start() -> Self {
        Self::start_in(tempfile::tempdir().unwrap()).await
    }

    /// Starts on the database `relay.sqlite` in `dir`, which may exist.
    async fn start_in(dir: tempfile::TempDir) -> Self {
        Self::start_at(dir, "127.0.0.1:0", false).await
    }

    /// Starts a relay whose server runs on a runtime of its own, so that
    /// [`Relay::stop`] ends it with all its connections, like a restart of
    /// the process. `address` may be that of an earlier relay.
    async fn start_stoppable(dir: tempfile::TempDir, address: &str) -> Self {
        Self::start_at(dir, address, true).await
    }

    async fn start_at(dir: tempfile::TempDir, address: &str, own_runtime: bool) -> Self {
        let store = Store::open(&dir.path().join("relay.sqlite")).unwrap();
        let clock = Arc::new(ManualClock::new(START));
        let waker = Arc::new(RecordingWaker::default());
        let state = AppState::new(
            store,
            &ACCESS_TOKEN,
            PushPolicy::default(),
            clock.clone(),
            waker.clone(),
        );
        let (addr, server) = if own_runtime {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let (bound, listener) = {
                let address = address.to_owned();
                runtime
                    .spawn(async move { tokio::net::TcpListener::bind(address).await })
                    .await
                    .unwrap()
                    .map(|listener| (listener.local_addr().unwrap(), listener))
                    .unwrap()
            };
            let addr = bound;
            runtime.spawn(lz_relay::serve(listener, state.clone()));
            (addr, Some(ServerRuntime(Some(runtime))))
        } else {
            let listener = tokio::net::TcpListener::bind(address).await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(lz_relay::serve(listener, state.clone()));
            (addr, None)
        };
        Self {
            base: format!("http://{addr}/v1"),
            ws_url: format!("ws://{addr}/v1/stream"),
            clock,
            state,
            waker,
            http: reqwest::Client::new(),
            _dir: dir,
            server,
        }
    }

    /// Stops the server and closes every connection. Returns the directory
    /// of the database and the address, to start on again.
    fn stop(mut self) -> (tempfile::TempDir, String) {
        let address = self
            .base
            .trim_start_matches("http://")
            .trim_end_matches("/v1");
        let address = address.to_owned();
        drop(self.server.take().expect("started with start_stoppable"));
        (self._dir, address)
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        bearer: Option<&str>,
        body: Option<reqwest::Body>,
    ) -> (StatusCode, Value) {
        let mut request = self.http.request(method, format!("{}{path}", self.base));
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        let json = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap()
        };
        (status, json)
    }

    async fn create_mailbox_with(
        &self,
        bearer: Option<&str>,
    ) -> (StatusCode, Value, String, String) {
        let (read_raw, read) = token();
        let (send_raw, send) = token();
        let body = json!({
            "read_token_hash": hex(&ids::sha256(&read_raw)),
            "send_token_hash": hex(&ids::sha256(&send_raw)),
        });
        let (status, json) = self
            .request(
                reqwest::Method::POST,
                "/mailboxes",
                bearer,
                Some(body.to_string().into()),
            )
            .await;
        (status, json, read, send)
    }

    async fn mailbox(&self) -> Mailbox {
        let (status, json, read, send) = self
            .create_mailbox_with(Some(&ids::encode(&ACCESS_TOKEN)))
            .await;
        assert_eq!(status, StatusCode::CREATED);
        Mailbox {
            id: json["mailbox"].as_str().unwrap().to_owned(),
            read,
            send,
        }
    }

    async fn put_reader(
        &self,
        mailbox: &Mailbox,
        reader: &str,
        push_url: Option<&str>,
    ) -> (StatusCode, Value) {
        let body = json!({ "push_url": push_url }).to_string();
        self.request(
            reqwest::Method::PUT,
            &format!("/mailboxes/{}/readers/{reader}", mailbox.id),
            Some(&mailbox.read),
            Some(body.into()),
        )
        .await
    }

    async fn post(&self, mailbox: &Mailbox, data: Vec<u8>) -> (StatusCode, Value) {
        self.request(
            reqwest::Method::POST,
            &format!("/mailboxes/{}/messages", mailbox.id),
            Some(&mailbox.send),
            Some(data.into()),
        )
        .await
    }

    fn waiting(&self, mailbox: &Mailbox) -> usize {
        self.state
            .store()
            .waiting_messages(&mailbox.raw_id())
            .unwrap()
    }

    async fn connect(&self) -> Client {
        let (ws, _) = tokio_tungstenite::connect_async(&self.ws_url)
            .await
            .unwrap();
        Client(ws)
    }
}

struct Client(WebSocketStream<MaybeTlsStream<TcpStream>>);

impl Client {
    async fn send(&mut self, frame: Value) {
        self.0.send(Message::text(frame.to_string())).await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(5), self.0.next())
                .await
                .expect("no frame within 5 s")
                .expect("connection closed")
                .unwrap();
            if let Message::Text(text) = message {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }

    /// Asserts that no frame arrives for a short while.
    async fn expect_silence(&mut self) {
        let result = tokio::time::timeout(Duration::from_millis(300), self.0.next()).await;
        assert!(result.is_err(), "unexpected frame: {result:?}");
    }

    async fn subscribe(&mut self, mailbox: &Mailbox, reader: &str) -> Value {
        self.send(json!({ "op": "subscribe", "mailbox": mailbox.id, "token": mailbox.read, "reader": reader }))
            .await;
        self.recv().await
    }

    async fn ack(&mut self, mailbox: &Mailbox, seq: u64) {
        self.send(json!({ "op": "ack", "mailbox": mailbox.id, "seq": seq }))
            .await;
    }

    async fn expect_message(&mut self, mailbox: &Mailbox, seq: u64, data: &[u8]) {
        let frame = self.recv().await;
        assert_eq!(
            frame,
            json!({ "op": "message", "mailbox": mailbox.id, "seq": seq, "data": ids::encode(data) })
        );
    }
}

fn subscribed(mailbox: &Mailbox) -> Value {
    json!({ "op": "subscribed", "mailbox": mailbox.id })
}

fn ws_error(mailbox: &Mailbox, code: &str) -> Value {
    json!({ "op": "error", "mailbox": mailbox.id, "code": code })
}

fn error(code: &str) -> Value {
    json!({ "error": code })
}

/// Distinct envelope contents, so order and identity can be checked.
fn envelope(n: u8) -> Vec<u8> {
    let mut data = ENVELOPE.to_vec();
    data[0] = n;
    data
}

#[tokio::test]
async fn health() {
    let relay = Relay::start().await;
    let (status, json) = relay
        .request(reqwest::Method::GET, "/health", None, None)
        .await;
    assert_eq!(
        (status, json),
        (StatusCode::OK, json!({ "features": ["reader_tokens"] }))
    );
}

#[tokio::test]
async fn mailbox_with_access_token_and_voucher() {
    let relay = Relay::start().await;
    let access = ids::encode(&ACCESS_TOKEN);

    // Access token.
    let (status, json, _, _) = relay.create_mailbox_with(Some(&access)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        ids::decode::<32>(json["mailbox"].as_str().unwrap()).map(|_| ()),
        Some(())
    );

    // Vouchers need the access token.
    let (status, json) = relay
        .request(reqwest::Method::POST, "/vouchers", None, None)
        .await;
    assert_eq!(
        (status, json),
        (StatusCode::UNAUTHORIZED, error("unauthorized"))
    );
    let (_, wrong) = token();
    let (status, _) = relay
        .request(reqwest::Method::POST, "/vouchers", Some(&wrong), None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, json) = relay
        .request(reqwest::Method::POST, "/vouchers", Some(&access), None)
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(json["expires_in"], 600);
    let voucher = json["voucher"].as_str().unwrap().to_owned();
    assert_eq!(voucher.len(), 43);

    // A voucher creates exactly one mailbox.
    let (status, json, _, _) = relay.create_mailbox_with(Some(&voucher)).await;
    assert_eq!(status, StatusCode::CREATED, "{json}");
    let (status, json, _, _) = relay.create_mailbox_with(Some(&voucher)).await;
    assert_eq!(
        (status, json),
        (StatusCode::UNAUTHORIZED, error("unauthorized"))
    );

    // A voucher is not an access token.
    let (status, _) = relay
        .request(reqwest::Method::POST, "/vouchers", Some(&voucher), None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // An expired voucher is refused.
    let (_, json) = relay
        .request(reqwest::Method::POST, "/vouchers", Some(&access), None)
        .await;
    let voucher = json["voucher"].as_str().unwrap().to_owned();
    relay.clock.advance(600);
    let (status, _, _, _) = relay.create_mailbox_with(Some(&voucher)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Without a token, with garbage, and with an unreadable body.
    let (status, _, _, _) = relay.create_mailbox_with(None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _, _) = relay.create_mailbox_with(Some("not-a-token")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for body in [
        "{".to_owned(),
        json!({ "read_token_hash": "00" }).to_string(),
        json!({ "read_token_hash": "zz".repeat(32), "send_token_hash": "00".repeat(32) })
            .to_string(),
    ] {
        let (status, json) = relay
            .request(
                reqwest::Method::POST,
                "/mailboxes",
                Some(&access),
                Some(body.into()),
            )
            .await;
        assert_eq!(
            (status, json),
            (StatusCode::BAD_REQUEST, error("bad_request"))
        );
    }
}

#[tokio::test]
async fn token_checks() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    let other = relay.mailbox().await;
    let messages = format!("/mailboxes/{}/messages", mailbox.id);

    // Sending needs the send token of this mailbox.
    for bearer in [
        None,
        Some(mailbox.read.as_str()),
        Some(other.send.as_str()),
        Some("x"),
    ] {
        let (status, json) = relay
            .request(
                reqwest::Method::POST,
                &messages,
                bearer,
                Some(ENVELOPE.to_vec().into()),
            )
            .await;
        assert_eq!(
            (status, json),
            (StatusCode::UNAUTHORIZED, error("unauthorized"))
        );
    }
    // Reader and mailbox management needs the read token.
    let (status, _) = relay
        .request(
            reqwest::Method::PUT,
            &format!("/mailboxes/{}/readers/{}", mailbox.id, reader_id(1)),
            Some(&mailbox.send),
            Some(r#"{"push_url":null}"#.into()),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = relay
        .request(
            reqwest::Method::DELETE,
            &format!("/mailboxes/{}", mailbox.id),
            Some(&mailbox.send),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        relay.post(&mailbox, ENVELOPE.to_vec()).await.0,
        StatusCode::ACCEPTED
    );

    // Unknown and malformed mailbox identifiers.
    let unknown = ids::encode(&[1u8; 32]);
    let (status, json) = relay
        .request(
            reqwest::Method::POST,
            &format!("/mailboxes/{unknown}/messages"),
            Some(&mailbox.send),
            Some(ENVELOPE.to_vec().into()),
        )
        .await;
    assert_eq!(
        (status, json),
        (StatusCode::NOT_FOUND, error("unknown_mailbox"))
    );
    let (status, json) = relay
        .request(
            reqwest::Method::POST,
            "/mailboxes/short/messages",
            Some(&mailbox.send),
            Some(ENVELOPE.to_vec().into()),
        )
        .await;
    assert_eq!(
        (status, json),
        (StatusCode::BAD_REQUEST, error("bad_request"))
    );
}

#[tokio::test]
async fn messages_are_exactly_one_envelope() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    for len in [
        0,
        1,
        lz_wire::ENVELOPE_LEN - 1,
        lz_wire::ENVELOPE_LEN + 1,
        lz_wire::MAX_MESSAGE_BYTES,
        lz_wire::MAX_MESSAGE_BYTES + 1,
        2048,
    ] {
        let (status, json) = relay.post(&mailbox, vec![0; len]).await;
        assert_eq!(
            (status, json),
            (StatusCode::BAD_REQUEST, error("bad_request")),
            "{len} bytes"
        );
    }
    assert_eq!(relay.waiting(&mailbox), 0);
    assert_eq!(
        relay.post(&mailbox, ENVELOPE.to_vec()).await.0,
        StatusCode::ACCEPTED
    );
    assert_eq!(relay.waiting(&mailbox), 1);
    // Refused bodies do not count towards the rate limit.
    for _ in 1..60 {
        assert_eq!(
            relay.post(&mailbox, vec![0; 1]).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        relay.post(&mailbox, ENVELOPE.to_vec()).await.0,
        StatusCode::ACCEPTED
    );
}

#[tokio::test]
async fn rate_limit_and_full_mailbox() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    let reader = reader_id(1);
    relay.put_reader(&mailbox, &reader, None).await;

    // Without acknowledgements, the mailbox is full after 32 messages; the
    // rate limit allows 60 per hour.
    for n in 1..=32 {
        assert_eq!(
            relay.post(&mailbox, envelope(n)).await.0,
            StatusCode::ACCEPTED
        );
    }
    assert_eq!(relay.waiting(&mailbox), 32);
    // Refused messages do not count against the rate limit.
    for _ in 0..15 {
        assert_eq!(
            relay.post(&mailbox, ENVELOPE.to_vec()).await,
            (StatusCode::TOO_MANY_REQUESTS, error("mailbox_full"))
        );
    }

    // Once acknowledged, there is room again, up to 60 accepted per hour.
    let mut client = relay.connect().await;
    assert_eq!(
        client.subscribe(&mailbox, &reader).await,
        subscribed(&mailbox)
    );
    for n in 1..=32u8 {
        client
            .expect_message(&mailbox, n.into(), &envelope(n))
            .await;
    }
    client.ack(&mailbox, 32).await;
    for n in 33..=60u8 {
        assert_eq!(
            relay.post(&mailbox, envelope(n)).await.0,
            StatusCode::ACCEPTED
        );
        client
            .expect_message(&mailbox, n.into(), &envelope(n))
            .await;
        client.ack(&mailbox, n.into()).await;
    }
    assert_eq!(
        relay.post(&mailbox, ENVELOPE.to_vec()).await,
        (StatusCode::TOO_MANY_REQUESTS, error("rate_limited"))
    );

    // The limit is per mailbox.
    let other = relay.mailbox().await;
    assert_eq!(
        relay.post(&other, ENVELOPE.to_vec()).await.0,
        StatusCode::ACCEPTED
    );

    // An hour later, the mailbox accepts messages again.
    relay.clock.advance(3600);
    assert_eq!(
        relay.post(&mailbox, ENVELOPE.to_vec()).await.0,
        StatusCode::ACCEPTED
    );
}

#[tokio::test]
async fn delivery_in_order_and_deletion_after_all_readers_acked() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    let (a, b) = (reader_id(1), reader_id(2));
    assert_eq!(
        relay.put_reader(&mailbox, &a, None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        relay.put_reader(&mailbox, &b, None).await.0,
        StatusCode::NO_CONTENT
    );
    for n in 1..=3 {
        assert_eq!(
            relay.post(&mailbox, envelope(n)).await.0,
            StatusCode::ACCEPTED
        );
    }

    let mut client_a = relay.connect().await;
    assert_eq!(client_a.subscribe(&mailbox, &a).await, subscribed(&mailbox));
    for n in 1..=3 {
        client_a
            .expect_message(&mailbox, n as u64, &envelope(n))
            .await;
    }
    client_a.expect_silence().await;
    client_a.ack(&mailbox, 2).await;

    let mut client_b = relay.connect().await;
    assert_eq!(client_b.subscribe(&mailbox, &b).await, subscribed(&mailbox));
    for n in 1..=3 {
        client_b
            .expect_message(&mailbox, n as u64, &envelope(n))
            .await;
    }
    // Only A has acknowledged so far: nothing is deleted.
    assert_eq!(relay.waiting(&mailbox), 3);

    client_b.ack(&mailbox, 3).await;
    client_b.expect_silence().await;
    assert_eq!(relay.waiting(&mailbox), 1);

    // A reconnects and gets only what it has not acknowledged.
    drop(client_a);
    let mut client_a = relay.connect().await;
    assert_eq!(client_a.subscribe(&mailbox, &a).await, subscribed(&mailbox));
    client_a.expect_message(&mailbox, 3, &envelope(3)).await;
    client_a.ack(&mailbox, 3).await;
    client_a.expect_silence().await;
    assert_eq!(relay.waiting(&mailbox), 0);

    // Acknowledging ahead of the last message does not swallow later ones.
    client_a.ack(&mailbox, 100).await;
    client_b.ack(&mailbox, 100).await;
    client_a.expect_silence().await;
    client_b.expect_silence().await;
    assert_eq!(
        relay.post(&mailbox, envelope(4)).await.0,
        StatusCode::ACCEPTED
    );
    client_a.expect_message(&mailbox, 4, &envelope(4)).await;
    client_b.expect_message(&mailbox, 4, &envelope(4)).await;
    assert_eq!(relay.waiting(&mailbox), 1);

    // Removing the reader that has not acknowledged releases the message.
    client_a.ack(&mailbox, 4).await;
    client_a.expect_silence().await;
    assert_eq!(relay.waiting(&mailbox), 1);
    let (status, _) = relay
        .request(
            reqwest::Method::DELETE,
            &format!("/mailboxes/{}/readers/{b}", mailbox.id),
            Some(&mailbox.read),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(client_b.recv().await, ws_error(&mailbox, "unknown_reader"));
    assert_eq!(relay.waiting(&mailbox), 0);
}

#[tokio::test]
async fn live_delivery_and_wake_signals() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    let other = relay.mailbox().await;
    let (a, b, c) = (reader_id(1), reader_id(2), reader_id(3));
    relay
        .put_reader(&mailbox, &a, Some("https://push.example.org/a"))
        .await;
    relay
        .put_reader(&mailbox, &b, Some("https://push.example.org/b"))
        .await;
    relay.put_reader(&mailbox, &c, None).await;
    relay
        .put_reader(&other, &a, Some("https://push.example.org/other"))
        .await;

    let mut client = relay.connect().await;
    assert_eq!(client.subscribe(&mailbox, &a).await, subscribed(&mailbox));
    assert_eq!(client.subscribe(&other, &a).await, subscribed(&other));
    client.expect_silence().await;

    assert_eq!(
        relay.post(&mailbox, envelope(7)).await.0,
        StatusCode::ACCEPTED
    );
    client.expect_message(&mailbox, 1, &envelope(7)).await;
    // A is connected, C has no push URL: only B is woken.
    assert_eq!(
        relay.waker.take(),
        vec!["https://push.example.org/b".to_owned()]
    );

    assert_eq!(
        relay.post(&other, envelope(8)).await.0,
        StatusCode::ACCEPTED
    );
    client.expect_message(&other, 1, &envelope(8)).await;
    assert!(relay.waker.take().is_empty());

    // After the connection is gone, A is woken again.
    drop(client);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        relay.post(&mailbox, envelope(9)).await.0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        relay.waker.take(),
        vec![
            "https://push.example.org/a".to_owned(),
            "https://push.example.org/b".to_owned()
        ]
    );
}

#[tokio::test]
async fn deleted_mailbox_is_unknown() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    let reader = reader_id(1);
    relay.put_reader(&mailbox, &reader, None).await;
    relay.post(&mailbox, ENVELOPE.to_vec()).await;
    let mut client = relay.connect().await;
    assert_eq!(
        client.subscribe(&mailbox, &reader).await,
        subscribed(&mailbox)
    );
    client.expect_message(&mailbox, 1, &ENVELOPE).await;

    let (status, _) = relay
        .request(
            reqwest::Method::DELETE,
            &format!("/mailboxes/{}", mailbox.id),
            Some(&mailbox.read),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(client.recv().await, ws_error(&mailbox, "unknown_mailbox"));

    let (status, json) = relay.post(&mailbox, ENVELOPE.to_vec()).await;
    assert_eq!(
        (status, json),
        (StatusCode::NOT_FOUND, error("unknown_mailbox"))
    );
    let (status, json) = relay.put_reader(&mailbox, &reader, None).await;
    assert_eq!(
        (status, json),
        (StatusCode::NOT_FOUND, error("unknown_mailbox"))
    );
    assert_eq!(
        client.subscribe(&mailbox, &reader).await,
        ws_error(&mailbox, "unknown_mailbox")
    );
    assert_eq!(relay.waiting(&mailbox), 0);
}

#[tokio::test]
async fn readers_and_push_urls() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    let reader = reader_id(1);
    for url in [
        "http://push.example.org/a",
        "https://127.0.0.1/a",
        "https://192.168.1.10/a",
        "https://[::1]/a",
        "https://[fe80::1]/a",
        "https://[fd00::1]/a",
        "https://localhost/a",
        "not a url",
    ] {
        let (status, json) = relay.put_reader(&mailbox, &reader, Some(url)).await;
        assert_eq!(
            (status, json),
            (StatusCode::BAD_REQUEST, error("bad_request")),
            "{url}"
        );
    }
    let long = format!("https://push.example.org/{}", "a".repeat(2000));
    assert_eq!(
        relay.put_reader(&mailbox, &reader, Some(&long)).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        relay.put_reader(&mailbox, "short", None).await.0,
        StatusCode::BAD_REQUEST
    );

    // Not registered yet.
    let mut client = relay.connect().await;
    assert_eq!(
        client.subscribe(&mailbox, &reader).await,
        ws_error(&mailbox, "unknown_reader")
    );
    let (status, json) = relay
        .request(
            reqwest::Method::DELETE,
            &format!("/mailboxes/{}/readers/{reader}", mailbox.id),
            Some(&mailbox.read),
            None,
        )
        .await;
    assert_eq!(
        (status, json),
        (StatusCode::NOT_FOUND, error("unknown_reader"))
    );

    assert_eq!(
        relay
            .put_reader(&mailbox, &reader, Some("https://push.example.org/a"))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        relay.put_reader(&mailbox, &reader, None).await.0,
        StatusCode::NO_CONTENT
    );

    // Subscribing needs the read token.
    client
        .send(json!({ "op": "subscribe", "mailbox": mailbox.id, "token": mailbox.send, "reader": reader }))
        .await;
    assert_eq!(client.recv().await, ws_error(&mailbox, "unauthorized"));
    // Unreadable frames and acks without subscription.
    client.send(json!({ "op": "dance" })).await;
    assert_eq!(
        client.recv().await,
        json!({ "op": "error", "code": "bad_request" })
    );
    client.ack(&mailbox, 1).await;
    assert_eq!(client.recv().await, ws_error(&mailbox, "bad_request"));
    assert_eq!(
        client.subscribe(&mailbox, &reader).await,
        subscribed(&mailbox)
    );

    // At most 16 readers.
    for n in 2..=16 {
        assert_eq!(
            relay.put_reader(&mailbox, &reader_id(n), None).await.0,
            StatusCode::NO_CONTENT
        );
    }
    assert_eq!(
        relay.put_reader(&mailbox, &reader_id(17), None).await.0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn expiry() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    let idle = relay.mailbox().await;

    // Without readers a message waits for up to 24 hours.
    relay.post(&mailbox, ENVELOPE.to_vec()).await;
    relay.clock.advance(24 * 3600 - 1);
    relay.state.cleanup().await.unwrap();
    assert_eq!(relay.waiting(&mailbox), 1);
    relay.clock.advance(1);
    let removed = relay.state.cleanup().await.unwrap();
    assert_eq!(removed.messages, 1);
    assert_eq!(relay.waiting(&mailbox), 0);

    // A mailbox that is fetched stays; one that is not goes after 90 days.
    let reader = reader_id(1);
    relay.put_reader(&mailbox, &reader, None).await;
    for _ in 0..3 {
        relay.clock.advance(40 * 86_400);
        let mut client = relay.connect().await;
        assert_eq!(
            client.subscribe(&mailbox, &reader).await,
            subscribed(&mailbox)
        );
        relay.state.cleanup().await.unwrap();
    }
    assert_eq!(
        relay.post(&mailbox, ENVELOPE.to_vec()).await.0,
        StatusCode::ACCEPTED
    );
    let (status, json) = relay.post(&idle, ENVELOPE.to_vec()).await;
    assert_eq!(
        (status, json),
        (StatusCode::NOT_FOUND, error("unknown_mailbox"))
    );
}

// --- Several devices (M5): derived mailboxes and handovers ---

fn labeled(label: &str, data: &[u8]) -> [u8; 32] {
    ids::sha256(&[label.as_bytes(), data].concat())
}

fn mailbox_from(status: StatusCode, json: Value, read: String, send: String) -> Mailbox {
    assert_eq!(status, StatusCode::CREATED, "{json}");
    Mailbox {
        id: json["mailbox"].as_str().unwrap().to_owned(),
        read,
        send,
    }
}

impl Relay {
    async fn voucher_mailbox(&self) -> Mailbox {
        let (status, json) = self
            .request(
                reqwest::Method::POST,
                "/vouchers",
                Some(&ids::encode(&ACCESS_TOKEN)),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, json, read, send) = self
            .create_mailbox_with(Some(json["voucher"].as_str().unwrap()))
            .await;
        mailbox_from(status, json, read, send)
    }

    async fn derive(&self, parent: &Mailbox) -> (StatusCode, Value, String, String) {
        self.create_mailbox_with(Some(&parent.read)).await
    }

    async fn delete(&self, mailbox: &Mailbox) -> StatusCode {
        self.request(
            reqwest::Method::DELETE,
            &format!("/mailboxes/{}", mailbox.id),
            Some(&mailbox.read),
            None,
        )
        .await
        .0
    }

    async fn put_handover(
        &self,
        id: &[u8; 32],
        bearer: &str,
        fetch_hash: Option<&[u8; 32]>,
        reply_hash: Option<&[u8; 32]>,
        body: Vec<u8>,
    ) -> (StatusCode, Value) {
        let mut request = self
            .http
            .put(format!("{}/handovers/{}", self.base, ids::encode(id)))
            .bearer_auth(bearer)
            .header("Content-Type", "application/octet-stream")
            .body(body);
        if let Some(hash) = fetch_hash {
            request = request.header("Lz-Fetch-Hash", hex(hash));
        }
        if let Some(hash) = reply_hash {
            request = request.header("Lz-Reply-Hash", hex(hash));
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        let json = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap()
        };
        (status, json)
    }

    /// `GET` of a handover slot: the body on success, else the error code.
    async fn get_handover(
        &self,
        id: &[u8; 32],
        bearer: &str,
    ) -> Result<Vec<u8>, (StatusCode, Value)> {
        let response = self
            .http
            .get(format!("{}/handovers/{}", self.base, ids::encode(id)))
            .bearer_auth(bearer)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.bytes().await.unwrap();
        if status == StatusCode::OK {
            Ok(bytes.to_vec())
        } else {
            Err((status, serde_json::from_slice(&bytes).unwrap()))
        }
    }

    fn handovers(&self) -> i64 {
        self.sql_count("SELECT COUNT(*) FROM handovers")
    }

    fn sql_count(&self, sql: &str) -> i64 {
        let conn = rusqlite::Connection::open(self._dir.path().join("relay.sqlite")).unwrap();
        conn.query_row(sql, [], |row| row.get(0)).unwrap()
    }
}

#[tokio::test]
async fn derived_mailboxes_up_to_ten() {
    let relay = Relay::start().await;
    let parent = relay.voucher_mailbox().await;
    let mut children = Vec::new();
    for _ in 0..10 {
        let (status, json, read, send) = relay.derive(&parent).await;
        children.push(mailbox_from(status, json, read, send));
    }
    let (status, json, _, _) = relay.derive(&parent).await;
    assert_eq!(
        (status, json),
        (StatusCode::TOO_MANY_REQUESTS, error("mailbox_quota"))
    );

    // Deleting one frees its place, once.
    assert_eq!(
        relay.delete(&children.pop().unwrap()).await,
        StatusCode::NO_CONTENT
    );
    let (status, _, _, _) = relay.derive(&parent).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _, _, _) = relay.derive(&parent).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // The limit is per voucher mailbox.
    let other = relay.voucher_mailbox().await;
    let (status, _, _, _) = relay.derive(&other).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn derived_mailboxes_are_limited_over_their_lifetime() {
    let relay = Relay::start().await;
    let parent = relay.voucher_mailbox().await;
    // Deleting a derived mailbox frees its place among the ten at a time,
    // but not in the lifetime budget of the voucher mailbox.
    for _ in 0..lz_relay::limits::MAX_DERIVED_MAILBOXES_EVER {
        let (status, json, read, send) = relay.derive(&parent).await;
        let child = mailbox_from(status, json, read, send);
        assert_eq!(relay.delete(&child).await, StatusCode::NO_CONTENT);
    }
    assert_eq!(
        relay.sql_count("SELECT COUNT(*) FROM mailboxes WHERE origin = 2"),
        0
    );
    let (status, json, _, _) = relay.derive(&parent).await;
    assert_eq!(
        (status, json),
        (StatusCode::TOO_MANY_REQUESTS, error("mailbox_quota"))
    );
    // Another voucher family has its own budget.
    let other = relay.voucher_mailbox().await;
    let (status, _, _, _) = relay.derive(&other).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn derived_mailboxes_in_parallel_never_exceed_ten() {
    let relay = Relay::start().await;
    let parent = relay.voucher_mailbox().await;
    let results = futures_util::future::join_all((0..24).map(|_| relay.derive(&parent))).await;
    let created = results
        .iter()
        .filter(|r| r.0 == StatusCode::CREATED)
        .count();
    let refused = results
        .iter()
        .filter(|r| (r.0, &r.1) == (StatusCode::TOO_MANY_REQUESTS, &error("mailbox_quota")))
        .count();
    assert_eq!((created, refused), (10, 14));
    assert_eq!(
        relay.sql_count("SELECT COUNT(*) FROM mailboxes WHERE origin = 2"),
        10
    );
}

#[tokio::test]
async fn only_voucher_mailboxes_derive() {
    let relay = Relay::start().await;

    // Neither a mailbox of the access token nor a derived one can derive.
    let own = relay.mailbox().await;
    let (status, json, _, _) = relay.derive(&own).await;
    assert_eq!(
        (status, json),
        (StatusCode::UNAUTHORIZED, error("unauthorized"))
    );
    let parent = relay.voucher_mailbox().await;
    let (status, json, read, send) = relay.derive(&parent).await;
    let child = mailbox_from(status, json, read, send);
    let (status, _, _, _) = relay.derive(&child).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A send token is no read token.
    let (status, _, _, _) = relay.create_mailbox_with(Some(&parent.send)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A derived mailbox stays when its parent goes, and cannot derive then.
    let (status, json, read, send) = relay.derive(&parent).await;
    let sibling = mailbox_from(status, json, read, send);
    assert_eq!(relay.delete(&parent).await, StatusCode::NO_CONTENT);
    assert_eq!(
        relay.post(&child, ENVELOPE.to_vec()).await.0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        relay.post(&sibling, ENVELOPE.to_vec()).await.0,
        StatusCode::ACCEPTED
    );
    for mailbox in [&parent, &child, &sibling] {
        let (status, _, _, _) = relay.derive(mailbox).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn derived_mailbox_works_normally() {
    let relay = Relay::start().await;
    let parent = relay.voucher_mailbox().await;
    let (status, json, read, send) = relay.derive(&parent).await;
    let mailbox = mailbox_from(status, json, read, send);
    let reader = reader_id(1);
    assert_eq!(
        relay.put_reader(&mailbox, &reader, None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        relay.post(&mailbox, envelope(1)).await.0,
        StatusCode::ACCEPTED
    );
    let mut client = relay.connect().await;
    assert_eq!(
        client.subscribe(&mailbox, &reader).await,
        subscribed(&mailbox)
    );
    client.expect_message(&mailbox, 1, &envelope(1)).await;
    assert_eq!(
        relay.post(&mailbox, envelope(2)).await.0,
        StatusCode::ACCEPTED
    );
    client.expect_message(&mailbox, 2, &envelope(2)).await;
    client.ack(&mailbox, 2).await;
    client.expect_silence().await;
    assert_eq!(relay.waiting(&mailbox), 0);
}

#[tokio::test]
async fn handover_put_and_get_once() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    let (fetch_raw, fetch) = token();
    let fetch_hash = ids::sha256(&fetch_raw);
    let slot: [u8; 32] = ids::random();
    let content = vec![7u8; 1000];

    // Without a relay token, or with a token the relay does not know.
    let (_, stranger) = token();
    for bearer in [stranger.as_str(), mailbox.send.as_str(), "x"] {
        let (status, json) = relay
            .put_handover(&slot, bearer, Some(&fetch_hash), None, content.clone())
            .await;
        assert_eq!(
            (status, json),
            (StatusCode::UNAUTHORIZED, error("unauthorized"))
        );
    }
    // Unreadable headers and identifiers.
    let (status, _) = relay
        .request(
            reqwest::Method::PUT,
            "/handovers/short",
            Some(&mailbox.read),
            Some(content.clone().into()),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let response = relay
        .http
        .put(format!("{}/handovers/{}", relay.base, ids::encode(&slot)))
        .bearer_auth(&mailbox.read)
        .header("Lz-Fetch-Hash", "00")
        .body(content.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let (status, _) = relay
        .put_handover(&slot, &mailbox.read, Some(&fetch_hash), None, Vec::new())
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, json) = relay
        .put_handover(
            &slot,
            &mailbox.read,
            Some(&fetch_hash),
            None,
            content.clone(),
        )
        .await;
    assert_eq!((status, json), (StatusCode::CREATED, Value::Null));
    let (status, json) = relay
        .put_handover(
            &slot,
            &mailbox.read,
            Some(&fetch_hash),
            None,
            content.clone(),
        )
        .await;
    assert_eq!(
        (status, json),
        (StatusCode::CONFLICT, error("handover_exists"))
    );

    // Fetching needs the fetch token, not the hash or the relay token.
    for bearer in [mailbox.read.as_str(), &ids::encode(&fetch_hash), &stranger] {
        assert_eq!(
            relay.get_handover(&slot, bearer).await,
            Err((StatusCode::UNAUTHORIZED, error("unauthorized")))
        );
    }
    assert_eq!(relay.get_handover(&slot, &fetch).await, Ok(content));
    assert_eq!(
        relay.get_handover(&slot, &fetch).await,
        Err((StatusCode::NOT_FOUND, error("unknown_handover")))
    );
    // Unknown wins over a wrong token.
    assert_eq!(
        relay.get_handover(&slot, &stranger).await,
        Err((StatusCode::NOT_FOUND, error("unknown_handover")))
    );

    // The access token authorizes as well, and the size limit is 64 KiB.
    let access = ids::encode(&ACCESS_TOKEN);
    let big: [u8; 32] = ids::random();
    let (status, json) = relay
        .put_handover(
            &big,
            &access,
            Some(&fetch_hash),
            None,
            vec![1; 64 * 1024 + 1],
        )
        .await;
    assert_eq!(
        (status, json),
        (StatusCode::PAYLOAD_TOO_LARGE, error("too_large"))
    );
    let (status, _) = relay
        .put_handover(&big, &access, Some(&fetch_hash), None, vec![1; 64 * 1024])
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        relay.get_handover(&big, &fetch).await,
        Ok(vec![1; 64 * 1024])
    );
    assert_eq!(relay.handovers(), 0);
}

#[tokio::test]
async fn handover_expiry() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    let (fetch_raw, fetch) = token();
    let fetch_hash = ids::sha256(&fetch_raw);
    let (_, stranger) = token();
    let slot: [u8; 32] = ids::random();

    relay
        .put_handover(&slot, &mailbox.read, Some(&fetch_hash), None, vec![1])
        .await;
    relay.clock.advance(599);
    assert_eq!(
        relay.get_handover(&slot, &stranger).await,
        Err((StatusCode::UNAUTHORIZED, error("unauthorized")))
    );
    relay.clock.advance(1);
    assert_eq!(
        relay.get_handover(&slot, &fetch).await,
        Err((StatusCode::NOT_FOUND, error("unknown_handover")))
    );
    // The identifier is free again.
    let (status, _) = relay
        .put_handover(&slot, &mailbox.read, Some(&fetch_hash), None, vec![2])
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // The periodic cleanup deletes expired slots and reservations.
    relay.clock.advance(300);
    let (reply_raw, _) = token();
    let other: [u8; 32] = ids::random();
    relay
        .put_handover(
            &other,
            &mailbox.read,
            Some(&fetch_hash),
            Some(&ids::sha256(&reply_raw)),
            vec![3],
        )
        .await;
    relay.clock.advance(299);
    assert_eq!(relay.state.cleanup().await.unwrap().handovers, 0);
    assert_eq!(relay.handovers(), 3);
    relay.clock.advance(1);
    assert_eq!(relay.state.cleanup().await.unwrap().handovers, 1);
    relay.clock.advance(299);
    assert_eq!(relay.state.cleanup().await.unwrap().handovers, 0);
    relay.clock.advance(1);
    assert_eq!(relay.state.cleanup().await.unwrap().handovers, 2);
    assert_eq!(relay.handovers(), 0);
}

#[tokio::test]
async fn handover_limits() {
    let relay = Relay::start().await;
    let (fetch_raw, fetch) = token();
    let fetch_hash = ids::sha256(&fetch_raw);
    let put = |bearer: String, reply: Option<[u8; 32]>| {
        let relay = &relay;
        async move {
            let slot: [u8; 32] = ids::random();
            let result = relay
                .put_handover(&slot, &bearer, Some(&fetch_hash), reply.as_ref(), vec![1])
                .await;
            (slot, result)
        }
    };

    // Two per token; a reserved reveal slot does not count here.
    let mailbox = relay.mailbox().await;
    let reply = ids::sha256(&token().0);
    let (first, result) = put(mailbox.read.clone(), Some(reply)).await;
    assert_eq!(result.0, StatusCode::CREATED);
    assert_eq!(
        put(mailbox.read.clone(), None).await.1.0,
        StatusCode::CREATED
    );
    assert_eq!(
        put(mailbox.read.clone(), None).await.1,
        (StatusCode::TOO_MANY_REQUESTS, error("handovers_quota"))
    );
    // Fetching one frees its place.
    assert_eq!(relay.get_handover(&first, &fetch).await, Ok(vec![1]));
    assert_eq!(
        put(mailbox.read.clone(), None).await.1.0,
        StatusCode::CREATED
    );
    assert_eq!(
        put(mailbox.read.clone(), None).await.1.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    let access = ids::encode(&ACCESS_TOKEN);
    for _ in 0..2 {
        assert_eq!(put(access.clone(), None).await.1.0, StatusCode::CREATED);
    }
    assert_eq!(
        put(access.clone(), None).await.1.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    // 2 + 1 reserved + 2 = 5 slots; fill up to 63 with more tokens.
    for _ in 0..29 {
        let mailbox = relay.mailbox().await;
        for _ in 0..2 {
            assert_eq!(
                put(mailbox.read.clone(), None).await.1.0,
                StatusCode::CREATED
            );
        }
    }
    assert_eq!(relay.handovers(), 63);

    // A slot with a reservation needs two places.
    let last = relay.mailbox().await;
    assert_eq!(
        put(last.read.clone(), Some(ids::sha256(&token().0)))
            .await
            .1,
        (StatusCode::SERVICE_UNAVAILABLE, error("handovers_full"))
    );
    let (slot, result) = put(last.read.clone(), None).await;
    assert_eq!(result.0, StatusCode::CREATED);
    assert_eq!(
        put(last.read.clone(), None).await.1,
        (StatusCode::SERVICE_UNAVAILABLE, error("handovers_full"))
    );
    assert_eq!(relay.handovers(), 64);

    // Expired slots make room.
    assert_eq!(relay.get_handover(&slot, &fetch).await, Ok(vec![1]));
    assert_eq!(put(last.read.clone(), None).await.1.0, StatusCode::CREATED);
    relay.clock.advance(600);
    assert_eq!(put(last.read.clone(), None).await.1.0, StatusCode::CREATED);
    assert_eq!(relay.handovers(), 1);
}

impl Relay {
    /// `PUT` of a new handover slot with one byte of content.
    async fn put_slot(&self, bearer: &str, with_reveal: bool) -> (StatusCode, Value) {
        let slot: [u8; 32] = ids::random();
        let fetch_hash = ids::sha256(&token().0);
        let reply_hash = with_reveal.then(|| ids::sha256(&token().0));
        self.put_handover(
            &slot,
            bearer,
            Some(&fetch_hash),
            reply_hash.as_ref(),
            vec![1],
        )
        .await
    }

    async fn derived(&self, parent: &Mailbox) -> Mailbox {
        let (status, json, read, send) = self.derive(parent).await;
        mailbox_from(status, json, read, send)
    }
}

#[tokio::test]
async fn voucher_family_shares_one_handover_quota() {
    let relay = Relay::start().await;

    // The voucher mailbox and its derived mailboxes share two slots; a
    // reserved reveal slot does not count here.
    let guest = relay.voucher_mailbox().await;
    let first = relay.derived(&guest).await;
    let second = relay.derived(&guest).await;
    assert_eq!(
        relay.put_slot(&guest.read, true).await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        relay.put_slot(&first.read, false).await.0,
        StatusCode::CREATED
    );
    for bearer in [&guest.read, &first.read, &second.read] {
        assert_eq!(
            relay.put_slot(bearer, false).await,
            (StatusCode::TOO_MANY_REQUESTS, error("handovers_quota"))
        );
    }
    // A derived mailbox keeps its family when the voucher mailbox is gone.
    assert_eq!(relay.delete(&guest).await, StatusCode::NO_CONTENT);
    assert_eq!(
        relay.put_slot(&second.read, false).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );

    // Another voucher family and the owner have their own quotas.
    let other = relay.voucher_mailbox().await;
    assert_eq!(
        relay.put_slot(&other.read, false).await.0,
        StatusCode::CREATED
    );
    let own = relay.mailbox().await;
    assert_eq!(
        relay.put_slot(&own.read, false).await.0,
        StatusCode::CREATED
    );
    let access = ids::encode(&ACCESS_TOKEN);
    assert_eq!(relay.put_slot(&access, false).await.0, StatusCode::CREATED);
}

#[tokio::test]
async fn voucher_families_cannot_fill_the_relay() {
    use lz_relay::limits::{MAX_GUEST_HANDOVERS, MAX_HANDOVERS};
    let relay = Relay::start().await;

    // Guests together hold at most their share of the relay's slots,
    // reserved reveal slots included: here each family has two slots with a
    // reservation each.
    for _ in 0..MAX_GUEST_HANDOVERS / 4 {
        let guest = relay.voucher_mailbox().await;
        for _ in 0..2 {
            assert_eq!(
                relay.put_slot(&guest.read, true).await.0,
                StatusCode::CREATED
            );
        }
    }
    assert_eq!(relay.handovers(), MAX_GUEST_HANDOVERS as i64);
    let late = relay.voucher_mailbox().await;
    assert_eq!(
        relay.put_slot(&late.read, false).await,
        (StatusCode::SERVICE_UNAVAILABLE, error("handovers_full"))
    );

    // The owner still has the rest, to add its own devices.
    let access = ids::encode(&ACCESS_TOKEN);
    assert_eq!(relay.put_slot(&access, true).await.0, StatusCode::CREATED);
    assert_eq!(relay.put_slot(&access, false).await.0, StatusCode::CREATED);
    let mut free = MAX_HANDOVERS - MAX_GUEST_HANDOVERS - 3;
    while free > 0 {
        let own = relay.mailbox().await;
        for _ in 0..free.min(2) {
            assert_eq!(
                relay.put_slot(&own.read, false).await.0,
                StatusCode::CREATED
            );
            free -= 1;
        }
    }
    assert_eq!(relay.handovers(), MAX_HANDOVERS as i64);
    let own = relay.mailbox().await;
    assert_eq!(
        relay.put_slot(&own.read, false).await,
        (StatusCode::SERVICE_UNAVAILABLE, error("handovers_full"))
    );
}

#[tokio::test]
async fn reveal_slot() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    let (fetch_raw, fetch) = token();
    let fetch_hash = ids::sha256(&fetch_raw);
    let (reply_raw, reply) = token();
    let reply_hash = ids::sha256(&reply_raw);
    let challenge: [u8; 32] = ids::random();
    let reveal = labeled("lebenszeichen-v1-reveal", &challenge);
    assert_eq!(reveal, ids::reveal_slot(&challenge));

    // Without a reservation there is no reveal slot.
    assert_eq!(
        relay
            .put_handover(&reveal, &reply, None, None, vec![5; 32])
            .await,
        (StatusCode::NOT_FOUND, error("unknown_handover"))
    );

    let (status, _) = relay
        .put_handover(
            &challenge,
            &mailbox.read,
            Some(&fetch_hash),
            Some(&reply_hash),
            vec![1; 64],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // Reserved: not for an ordinary PUT, and nothing to fetch yet.
    let (_, other) = token();
    assert_eq!(
        relay
            .put_handover(&reveal, &other, Some(&fetch_hash), None, vec![5; 32])
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let access = ids::encode(&ACCESS_TOKEN);
    assert_eq!(
        relay
            .put_handover(&reveal, &access, Some(&fetch_hash), None, vec![5; 32])
            .await,
        (StatusCode::CONFLICT, error("handover_exists"))
    );
    assert_eq!(
        relay.get_handover(&reveal, &reply).await,
        Err((StatusCode::NOT_FOUND, error("unknown_handover")))
    );

    // Only the reply token, only exactly 32 bytes, without hash headers.
    for bearer in [other.as_str(), mailbox.read.as_str(), fetch.as_str()] {
        assert_eq!(
            relay
                .put_handover(&reveal, bearer, None, None, vec![5; 32])
                .await,
            (StatusCode::UNAUTHORIZED, error("unauthorized"))
        );
    }
    for len in [0, 31, 33, 64 * 1024] {
        assert_eq!(
            relay
                .put_handover(&reveal, &reply, None, None, vec![5; len])
                .await,
            (StatusCode::BAD_REQUEST, error("bad_request")),
            "{len}"
        );
    }
    assert_eq!(
        relay
            .put_handover(&reveal, &reply, None, Some(&reply_hash), vec![5; 32])
            .await
            .0,
        StatusCode::BAD_REQUEST
    );

    // The reservation survives fetching the challenge slot.
    assert_eq!(
        relay.get_handover(&challenge, &fetch).await,
        Ok(vec![1; 64])
    );
    assert_eq!(
        relay
            .put_handover(&reveal, &reply, None, None, vec![5; 32])
            .await,
        (StatusCode::CREATED, Value::Null)
    );
    assert_eq!(
        relay
            .put_handover(&reveal, &reply, None, None, vec![6; 32])
            .await,
        (StatusCode::CONFLICT, error("handover_exists"))
    );
    assert_eq!(
        relay.get_handover(&reveal, &fetch).await,
        Err((StatusCode::UNAUTHORIZED, error("unauthorized")))
    );
    assert_eq!(relay.get_handover(&reveal, &reply).await, Ok(vec![5; 32]));
    assert_eq!(
        relay.get_handover(&reveal, &reply).await,
        Err((StatusCode::NOT_FOUND, error("unknown_handover")))
    );
    // Once gone, it is not reserved any more.
    assert_eq!(
        relay
            .put_handover(&reveal, &reply, None, None, vec![5; 32])
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    // The reservation lasts 10 minutes from the reserving PUT.
    let challenge: [u8; 32] = ids::random();
    let reveal = ids::reveal_slot(&challenge);
    relay
        .put_handover(
            &challenge,
            &mailbox.read,
            Some(&fetch_hash),
            Some(&reply_hash),
            vec![1],
        )
        .await;
    relay.clock.advance(300);
    assert_eq!(
        relay
            .put_handover(&reveal, &reply, None, None, vec![5; 32])
            .await
            .0,
        StatusCode::CREATED
    );
    relay.clock.advance(300);
    assert_eq!(
        relay.get_handover(&reveal, &reply).await,
        Err((StatusCode::NOT_FOUND, error("unknown_handover")))
    );
}

#[tokio::test]
async fn occupied_reveal_identifier_fails_the_whole_put() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    let (fetch_raw, fetch) = token();
    let fetch_hash = ids::sha256(&fetch_raw);
    let reply_hash = ids::sha256(&token().0);
    let challenge: [u8; 32] = ids::random();

    // Someone stored an ordinary slot under the reveal identifier.
    let access = ids::encode(&ACCESS_TOKEN);
    let (status, _) = relay
        .put_handover(
            &ids::reveal_slot(&challenge),
            &access,
            Some(&fetch_hash),
            None,
            vec![9],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        relay
            .put_handover(
                &challenge,
                &mailbox.read,
                Some(&fetch_hash),
                Some(&reply_hash),
                vec![1],
            )
            .await,
        (StatusCode::CONFLICT, error("handover_exists"))
    );
    assert_eq!(
        relay.get_handover(&challenge, &fetch).await,
        Err((StatusCode::NOT_FOUND, error("unknown_handover")))
    );
    assert_eq!(relay.handovers(), 1);

    // A reservation blocks a second reserving PUT the same way.
    let challenge: [u8; 32] = ids::random();
    relay
        .put_handover(
            &challenge,
            &mailbox.read,
            Some(&fetch_hash),
            Some(&reply_hash),
            vec![1],
        )
        .await;
    assert_eq!(relay.get_handover(&challenge, &fetch).await, Ok(vec![1]));
    assert_eq!(
        relay
            .put_handover(
                &challenge,
                &mailbox.read,
                Some(&fetch_hash),
                Some(&reply_hash),
                vec![1],
            )
            .await,
        (StatusCode::CONFLICT, error("handover_exists"))
    );
}

/// Adding a device as in `docs/wire-format.md`, between a new device N and an
/// existing device E without access token, through HTTP only.
#[tokio::test]
async fn adding_a_device_through_handover_slots() {
    let relay = Relay::start().await;
    // E was invited with a voucher.
    let own = relay.voucher_mailbox().await;

    // N: handover key, fetch token and n; the device code carries the
    // public key, the fetch hash and the commitment.
    let public_key: [u8; 32] = ids::random();
    let (fetch_raw, fetch) = token();
    let n: [u8; 32] = ids::random();
    let code_fetch_hash = ids::sha256(&fetch_raw);
    let commitment = labeled("lebenszeichen-v1-commit", &n);

    // E: challenge and reply token into the challenge slot.
    let c: [u8; 32] = ids::random();
    let (t_raw, t) = token();
    let challenge_slot = labeled("lebenszeichen-v1-challenge", &public_key);
    let (status, _) = relay
        .put_handover(
            &challenge_slot,
            &own.read,
            Some(&code_fetch_hash),
            Some(&ids::sha256(&t_raw)),
            [c, t_raw].concat(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // N fetches the challenge, then reveals n with the reply token.
    let fetched = relay.get_handover(&challenge_slot, &fetch).await.unwrap();
    let (got_c, got_t) = fetched.split_at(32);
    assert_eq!(got_c, c);
    let reveal_slot = labeled("lebenszeichen-v1-reveal", &challenge_slot);
    let (status, _) = relay
        .put_handover(&reveal_slot, &ids::encode(got_t), None, None, n.to_vec())
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // E fetches n and checks the commitment.
    let revealed = relay.get_handover(&reveal_slot, &t).await.unwrap();
    assert_eq!(labeled("lebenszeichen-v1-commit", &revealed), commitment);

    // E creates the sync mailbox from its own voucher mailbox and stores the
    // handover.
    let (status, json, read, send) = relay.derive(&own).await;
    let sync = mailbox_from(status, json, read, send);
    let reader_n = reader_id(2);
    assert_eq!(
        relay.put_reader(&sync, &reader_n, None).await.0,
        StatusCode::NO_CONTENT
    );
    let sealed: Vec<u8> = (0..40_000u32).map(|i| i as u8).collect();
    let handover_slot = labeled("lebenszeichen-v1-handover", &public_key);
    let (status, _) = relay
        .put_handover(
            &handover_slot,
            &own.read,
            Some(&code_fetch_hash),
            None,
            sealed.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // N fetches the handover, once, and reads the sync mailbox.
    assert_eq!(relay.get_handover(&handover_slot, &fetch).await, Ok(sealed));
    assert_eq!(
        relay.get_handover(&handover_slot, &fetch).await,
        Err((StatusCode::NOT_FOUND, error("unknown_handover")))
    );
    assert_eq!(relay.post(&sync, envelope(1)).await.0, StatusCode::ACCEPTED);
    let mut client = relay.connect().await;
    assert_eq!(client.subscribe(&sync, &reader_n).await, subscribed(&sync));
    client.expect_message(&sync, 1, &envelope(1)).await;
    assert_eq!(relay.handovers(), 0);
}

/// The schema before `user_version` was used, as in the relay up to M4.
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

fn schema_of(path: &std::path::Path) -> (i64, Vec<String>) {
    let conn = rusqlite::Connection::open(path).unwrap();
    let version = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let mut stmt = conn
        .prepare("SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")
        .unwrap();
    let sql = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    (version, sql)
}

#[tokio::test]
async fn migration_from_version_0() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("relay.sqlite");
    let (read_raw, read) = token();
    let (send_raw, send) = token();
    let old_id: [u8; 32] = ids::random();
    let other_id: [u8; 32] = ids::random();
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA_V0).unwrap();
        let day = START.div_euclid(86_400);
        for (id, read_hash) in [
            (old_id, ids::sha256(&read_raw)),
            (other_id, ids::sha256(&[3; 32])),
        ] {
            conn.execute(
                "INSERT INTO mailboxes VALUES (?1, ?2, ?3, 2, ?4)",
                rusqlite::params![&id[..], &read_hash[..], &ids::sha256(&send_raw)[..], day],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO readers VALUES (?1, ?2, NULL, 0)",
            rusqlite::params![&old_id[..], &[1u8; 16][..]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages VALUES (?1, 1, ?2, ?3)",
            rusqlite::params![&old_id[..], START, &envelope(1)],
        )
        .unwrap();
        assert_eq!(schema_of(&path).0, 0);
    }

    let relay = Relay::start_in(dir).await;
    let (version, migrated) = schema_of(&path);
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(
        relay.sql_count("SELECT COUNT(*) FROM mailboxes WHERE origin = 1 AND parent IS NULL"),
        2
    );

    // The data is intact, and the old mailbox counts as a voucher mailbox.
    let old = Mailbox {
        id: ids::encode(&old_id),
        read,
        send,
    };
    let reader = reader_id(1);
    let mut client = relay.connect().await;
    assert_eq!(client.subscribe(&old, &reader).await, subscribed(&old));
    client.expect_message(&old, 1, &envelope(1)).await;
    assert_eq!(relay.post(&old, envelope(2)).await.0, StatusCode::ACCEPTED);
    client.expect_message(&old, 2, &envelope(2)).await;
    let (status, json, read, send) = relay.derive(&old).await;
    let child = mailbox_from(status, json, read, send);
    let (status, _, _, _) = relay.derive(&child).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Opening again changes nothing.
    drop(Store::open(&path).unwrap());
    assert_eq!(schema_of(&path), (SCHEMA_VERSION, migrated.clone()));
    assert_eq!(relay.sql_count("SELECT COUNT(*) FROM mailboxes"), 3);
    assert_eq!(relay.sql_count("SELECT COUNT(*) FROM messages"), 2);

    // A fresh database ends up identical.
    let fresh = tempfile::tempdir().unwrap();
    let fresh_path = fresh.path().join("relay.sqlite");
    drop(Store::open(&fresh_path).unwrap());
    assert_eq!(schema_of(&fresh_path), (SCHEMA_VERSION, migrated));
}

/// Version 0 to 1 on top of [`SCHEMA_V0`], as in the relay from M5 to M8.
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

#[tokio::test]
async fn migration_from_version_1() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("relay.sqlite");
    let (read_raw, read) = token();
    let (send_raw, send) = token();
    let (fetch_raw, fetch) = token();
    let parent_id: [u8; 32] = ids::random();
    let slot: [u8; 32] = ids::random();
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA_V0).unwrap();
        conn.execute_batch(MIGRATION_V1).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        let day = START.div_euclid(86_400);
        conn.execute(
            "INSERT INTO mailboxes VALUES (?1, ?2, ?3, 1, ?4, 1, NULL)",
            rusqlite::params![
                &parent_id[..],
                &ids::sha256(&read_raw)[..],
                &ids::sha256(&send_raw)[..],
                day
            ],
        )
        .unwrap();
        // Three derived mailboxes of the voucher mailbox, one of the owner.
        for origin in [2, 2, 2, 0] {
            let id: [u8; 32] = ids::random();
            let parent = (origin == 2).then_some(&parent_id[..]);
            conn.execute(
                "INSERT INTO mailboxes VALUES (?1, ?2, ?2, 1, ?3, ?4, ?5)",
                rusqlite::params![&id[..], &ids::sha256(&id)[..], day, origin, parent],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO handovers VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            rusqlite::params![
                &slot[..],
                &[7u8; 10][..],
                &ids::sha256(&fetch_raw)[..],
                &ids::sha256(&read_raw)[..],
                START + 600
            ],
        )
        .unwrap();
    }

    let relay = Relay::start_in(dir).await;
    let (version, migrated) = schema_of(&path);
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(
        relay.sql_count("SELECT derived_total FROM mailboxes WHERE origin = 1"),
        3
    );
    assert_eq!(
        relay.sql_count("SELECT SUM(derived_total) FROM mailboxes WHERE origin <> 1"),
        0
    );
    // The waiting slot is intact and counts as the owner's.
    assert_eq!(
        relay.sql_count("SELECT COUNT(*) FROM handovers WHERE family IS NULL"),
        1
    );
    assert_eq!(relay.get_handover(&slot, &fetch).await, Ok(vec![7; 10]));

    // The voucher mailbox derives on, up to ten at a time.
    let parent = Mailbox {
        id: ids::encode(&parent_id),
        read,
        send,
    };
    for _ in 0..7 {
        let (status, _, _, _) = relay.derive(&parent).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (status, _, _, _) = relay.derive(&parent).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        relay.sql_count("SELECT derived_total FROM mailboxes WHERE origin = 1"),
        10
    );

    // A fresh database ends up identical.
    let fresh = tempfile::tempdir().unwrap();
    let fresh_path = fresh.path().join("relay.sqlite");
    drop(Store::open(&fresh_path).unwrap());
    assert_eq!(schema_of(&fresh_path), (SCHEMA_VERSION, migrated));
}

#[tokio::test]
async fn newer_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("relay.sqlite");
    drop(Store::open(&path).unwrap());
    rusqlite::Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    assert!(Store::open(&path).is_err());
}

#[tokio::test]
async fn waiting_messages_survive_a_restart_and_a_listener_reconnects() {
    let relay = Relay::start_stoppable(tempfile::tempdir().unwrap(), "127.0.0.1:0").await;
    let mailbox = relay.mailbox().await;
    let voucher = relay.voucher_mailbox().await;
    let reader = reader_id(1);
    let idle = reader_id(2);
    relay.put_reader(&mailbox, &reader, None).await;
    // A second reader that never connects keeps messages waiting.
    relay.put_reader(&mailbox, &idle, None).await;
    for n in 1..=2 {
        assert_eq!(
            relay.post(&mailbox, envelope(n)).await.0,
            StatusCode::ACCEPTED
        );
    }

    // The listener has message 1 acknowledged, message 2 received but not.
    let mut listener = relay.connect().await;
    assert_eq!(
        listener.subscribe(&mailbox, &reader).await,
        subscribed(&mailbox)
    );
    listener.expect_message(&mailbox, 1, &envelope(1)).await;
    listener.expect_message(&mailbox, 2, &envelope(2)).await;
    listener.ack(&mailbox, 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The relay goes away. The connection ends and posting fails.
    let url = relay.base.clone();
    let (dir, address) = relay.stop();
    let ended = tokio::time::timeout(Duration::from_secs(5), listener.0.next())
        .await
        .expect("the connection stayed open");
    assert!(
        !matches!(ended, Some(Ok(Message::Text(_)))),
        "unexpected frame: {ended:?}"
    );
    let down = reqwest::Client::new()
        .post(format!("{url}/mailboxes/{}/messages", mailbox.id))
        .bearer_auth(&mailbox.send)
        .body(envelope(3))
        .send()
        .await;
    assert!(down.is_err(), "the stopped relay answered");

    // Same database, same address.
    let relay = Relay::start_stoppable(dir, &address).await;
    assert_eq!(relay.base, url);
    assert_eq!(relay.waiting(&mailbox), 2);
    // The mailbox, its tokens and the readers are still there, and
    // sequence numbers go on.
    assert_eq!(
        relay.post(&mailbox, envelope(3)).await.0,
        StatusCode::ACCEPTED
    );

    // The listener reconnects without registering again, and gets what it
    // had not acknowledged, including what arrived in between.
    let mut listener = relay.connect().await;
    assert_eq!(
        listener.subscribe(&mailbox, &reader).await,
        subscribed(&mailbox)
    );
    listener.expect_message(&mailbox, 2, &envelope(2)).await;
    listener.expect_message(&mailbox, 3, &envelope(3)).await;
    listener.expect_silence().await;
    // Live delivery works again.
    assert_eq!(
        relay.post(&mailbox, envelope(4)).await.0,
        StatusCode::ACCEPTED
    );
    listener.expect_message(&mailbox, 4, &envelope(4)).await;
    // The idle reader still gets everything from the start.
    let mut other = relay.connect().await;
    assert_eq!(other.subscribe(&mailbox, &idle).await, subscribed(&mailbox));
    for n in 1..=4 {
        other.expect_message(&mailbox, n as u64, &envelope(n)).await;
    }

    // Acknowledgements after the restart count as before.
    listener.ack(&mailbox, 4).await;
    other.ack(&mailbox, 4).await;
    other.expect_silence().await;
    assert_eq!(relay.waiting(&mailbox), 0);

    // A voucher mailbox still derives, and a wrong token is still refused.
    let (status, ..) = relay.derive(&voucher).await;
    assert_eq!(status, StatusCode::CREATED);
    let wrong = Mailbox {
        id: mailbox.id.clone(),
        read: mailbox.read.clone(),
        send: mailbox.read.clone(),
    };
    assert_eq!(
        relay.post(&wrong, envelope(5)).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn messages_expire_by_their_arrival_time_across_a_restart() {
    let relay = Relay::start_stoppable(tempfile::tempdir().unwrap(), "127.0.0.1:0").await;
    let mailbox = relay.mailbox().await;
    relay.put_reader(&mailbox, &reader_id(1), None).await;
    relay.post(&mailbox, envelope(1)).await;
    let (dir, address) = relay.stop();

    // The new process starts at the same time as the old one, then the
    // clock runs on: the 24 hours count from arrival, not from the start.
    let relay = Relay::start_stoppable(dir, &address).await;
    assert_eq!(relay.waiting(&mailbox), 1);
    relay.clock.advance(23 * 3600);
    let mut client = relay.connect().await;
    assert_eq!(
        client.subscribe(&mailbox, &reader_id(1)).await,
        subscribed(&mailbox)
    );
    client.expect_message(&mailbox, 1, &envelope(1)).await;
}

// --- Read token per reader (M10) ---

/// A token with its `b64url` form and the hex of its hash.
struct Tok {
    text: String,
    hash: String,
}

fn tok() -> Tok {
    let (raw, text) = token();
    Tok {
        text,
        hash: hex(&ids::sha256(&raw)),
    }
}

impl Relay {
    /// Creates a mailbox in the new form, with `reader` as its first full
    /// reader. The returned [`Mailbox`] carries the reader's token as `read`.
    async fn create_reader_mailbox(
        &self,
        bearer: &str,
        reader: &str,
    ) -> (StatusCode, Value, Mailbox) {
        let own = tok();
        let send = tok();
        let body = json!({
            "reader": reader,
            "reader_token_hash": own.hash,
            "send_token_hash": send.hash,
        });
        let (status, json) = self
            .request(
                reqwest::Method::POST,
                "/mailboxes",
                Some(bearer),
                Some(body.to_string().into()),
            )
            .await;
        let id = json["mailbox"].as_str().unwrap_or_default().to_owned();
        (
            status,
            json,
            Mailbox {
                id,
                read: own.text,
                send: send.text,
            },
        )
    }

    async fn reader_mailbox(&self, reader: &str) -> Mailbox {
        let (status, json, mailbox) = self
            .create_reader_mailbox(&ids::encode(&ACCESS_TOKEN), reader)
            .await;
        assert_eq!(status, StatusCode::CREATED, "{json}");
        mailbox
    }

    async fn put_reader_with(
        &self,
        mailbox: &Mailbox,
        reader: &str,
        bearer: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        self.request(
            reqwest::Method::PUT,
            &format!("/mailboxes/{}/readers/{reader}", mailbox.id),
            Some(bearer),
            Some(body.to_string().into()),
        )
        .await
    }

    async fn delete_reader_with(
        &self,
        mailbox: &Mailbox,
        reader: &str,
        bearer: &str,
    ) -> (StatusCode, Value) {
        self.request(
            reqwest::Method::DELETE,
            &format!("/mailboxes/{}/readers/{reader}", mailbox.id),
            Some(bearer),
            None,
        )
        .await
    }

    async fn delete_with(&self, mailbox: &Mailbox, bearer: &str) -> (StatusCode, Value) {
        self.request(
            reqwest::Method::DELETE,
            &format!("/mailboxes/{}", mailbox.id),
            Some(bearer),
            None,
        )
        .await
    }

    async fn drop_shared(&self, mailbox: &Mailbox, bearer: &str) -> (StatusCode, Value) {
        self.request(
            reqwest::Method::DELETE,
            &format!("/mailboxes/{}/read-token", mailbox.id),
            Some(bearer),
            None,
        )
        .await
    }

    async fn set_send_token(
        &self,
        mailbox: &Mailbox,
        bearer: &str,
        hash: &str,
    ) -> (StatusCode, Value) {
        self.request(
            reqwest::Method::PUT,
            &format!("/mailboxes/{}/send-token", mailbox.id),
            Some(bearer),
            Some(json!({ "send_token_hash": hash }).to_string().into()),
        )
        .await
    }

    /// Creates a mailbox in the old form with `bearer`.
    async fn derive_with(&self, bearer: &str) -> StatusCode {
        self.create_mailbox_with(Some(bearer)).await.0
    }

    async fn voucher(&self) -> String {
        let (status, json) = self
            .request(
                reqwest::Method::POST,
                "/vouchers",
                Some(&ids::encode(&ACCESS_TOKEN)),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
        json["voucher"].as_str().unwrap().to_owned()
    }
}

impl Client {
    async fn subscribe_with(&mut self, mailbox: &Mailbox, token: &str, reader: &str) -> Value {
        self.send(
            json!({ "op": "subscribe", "mailbox": mailbox.id, "token": token, "reader": reader }),
        )
        .await;
        self.recv().await
    }
}

#[tokio::test]
async fn mailbox_in_the_new_form() {
    let relay = Relay::start().await;
    let access = ids::encode(&ACCESS_TOKEN);
    let (full, other) = (reader_id(1), reader_id(2));
    let mailbox = relay.reader_mailbox(&full).await;

    // The first reader subscribes with its own token.
    let mut client = relay.connect().await;
    assert_eq!(
        client.subscribe(&mailbox, &full).await,
        subscribed(&mailbox)
    );
    relay.post(&mailbox, envelope(1)).await;
    client.expect_message(&mailbox, 1, &envelope(1)).await;

    // Without a shared token, a new reader needs a token of its own.
    assert_eq!(
        relay
            .put_reader_with(&mailbox, &other, &mailbox.read, json!({ "push_url": null }))
            .await,
        (StatusCode::BAD_REQUEST, error("bad_request"))
    );
    let own = tok();
    assert_eq!(
        relay
            .put_reader_with(
                &mailbox,
                &other,
                &mailbox.read,
                json!({ "token_hash": own.hash })
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let mut second = relay.connect().await;
    assert_eq!(
        second.subscribe_with(&mailbox, &own.text, &other).await,
        subscribed(&mailbox)
    );
    second.expect_message(&mailbox, 1, &envelope(1)).await;
    // A token is only good for its own reader.
    assert_eq!(
        second.subscribe_with(&mailbox, &own.text, &full).await,
        ws_error(&mailbox, "unauthorized")
    );

    // Unreadable bodies: both forms, neither, a bad reader or hash.
    let hash = tok().hash;
    for body in [
        json!({ "read_token_hash": hash, "reader": full, "reader_token_hash": hash, "send_token_hash": hash }),
        json!({ "send_token_hash": hash }),
        json!({ "reader": full, "send_token_hash": hash }),
        json!({ "reader": "short", "reader_token_hash": hash, "send_token_hash": hash }),
        json!({ "reader": full, "reader_token_hash": "00", "send_token_hash": hash }),
    ] {
        assert_eq!(
            relay
                .request(
                    reqwest::Method::POST,
                    "/mailboxes",
                    Some(&access),
                    Some(body.to_string().into()),
                )
                .await,
            (StatusCode::BAD_REQUEST, error("bad_request")),
            "{body}"
        );
    }
    // The new form needs the same authorization as the old.
    let (status, _, _) = relay.create_reader_mailbox(&tok().text, &full).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let voucher = relay.voucher().await;
    let (status, _, _) = relay.create_reader_mailbox(&voucher, &full).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn own_token_replaces_the_shared_token_for_its_reader() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    let (a, b) = (reader_id(1), reader_id(2));
    relay
        .put_reader(&mailbox, &a, Some("https://push.example.org/a"))
        .await;
    relay.put_reader(&mailbox, &b, None).await;
    let mut client = relay.connect().await;
    assert_eq!(client.subscribe(&mailbox, &a).await, subscribed(&mailbox));

    // A gives itself a token with the shared token; the push URL stays,
    // and the live subscription with the shared token ends.
    let own = tok();
    assert_eq!(
        relay
            .put_reader_with(
                &mailbox,
                &a,
                &mailbox.read,
                json!({ "token_hash": own.hash })
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(client.recv().await, ws_error(&mailbox, "unauthorized"));
    relay.post(&mailbox, envelope(1)).await;
    client.expect_silence().await;
    assert_eq!(
        relay.waker.take(),
        vec!["https://push.example.org/a".to_owned()]
    );

    // The shared token no longer subscribes as A; A's own token does.
    assert_eq!(
        client.subscribe(&mailbox, &a).await,
        ws_error(&mailbox, "unauthorized")
    );
    assert_eq!(
        client.subscribe_with(&mailbox, &own.text, &a).await,
        subscribed(&mailbox)
    );
    client.expect_message(&mailbox, 1, &envelope(1)).await;
    client.ack(&mailbox, 1).await;
    client.expect_silence().await;
    // Setting the same token again changes nothing.
    assert_eq!(
        relay
            .put_reader_with(&mailbox, &a, &own.text, json!({ "token_hash": own.hash }))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    client.expect_silence().await;

    // B is still a legacy reader.
    let mut legacy = relay.connect().await;
    assert_eq!(legacy.subscribe(&mailbox, &b).await, subscribed(&mailbox));
    legacy.expect_message(&mailbox, 1, &envelope(1)).await;
    // A token hash that the shared token or another reader has is refused.
    let shared_hash = hex(&ids::sha256(&ids::decode::<32>(&mailbox.read).unwrap()));
    for hash in [&own.hash, &shared_hash] {
        assert_eq!(
            relay
                .put_reader_with(&mailbox, &b, &mailbox.read, json!({ "token_hash": hash }))
                .await,
            (StatusCode::BAD_REQUEST, error("bad_request"))
        );
    }
    // Unknown roles and bad hashes too.
    for body in [json!({ "role": "admin" }), json!({ "token_hash": "zz" })] {
        assert_eq!(
            relay
                .put_reader_with(&mailbox, &b, &mailbox.read, body)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    legacy.expect_silence().await;

    // With its own token, A may change its push URL and token.
    let newer = tok();
    assert_eq!(
        relay
            .put_reader_with(
                &mailbox,
                &a,
                &own.text,
                json!({ "push_url": null, "token_hash": newer.hash })
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(client.recv().await, ws_error(&mailbox, "unauthorized"));
    assert_eq!(
        client.subscribe_with(&mailbox, &own.text, &a).await,
        ws_error(&mailbox, "unauthorized")
    );
    assert_eq!(
        client.subscribe_with(&mailbox, &newer.text, &a).await,
        subscribed(&mailbox)
    );
    relay.post(&mailbox, envelope(2)).await;
    client.expect_message(&mailbox, 2, &envelope(2)).await;
    assert!(relay.waker.take().is_empty());
}

#[tokio::test]
async fn satellite_reads_but_manages_nothing() {
    let relay = Relay::start().await;
    let (full, sat, other) = (reader_id(1), reader_id(2), reader_id(3));
    let voucher = relay.voucher().await;
    let (status, _, mailbox) = relay.create_reader_mailbox(&voucher, &full).await;
    assert_eq!(status, StatusCode::CREATED);
    let enrolment = tok();
    assert_eq!(
        relay
            .put_reader_with(
                &mailbox,
                &sat,
                &mailbox.read,
                json!({ "token_hash": enrolment.hash, "role": "satellite" })
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );

    // It replaces the enrolment token with its own, and may set its push URL.
    let own = tok();
    assert_eq!(
        relay
            .put_reader_with(
                &mailbox,
                &sat,
                &enrolment.text,
                json!({ "token_hash": own.hash, "push_url": "https://push.example.org/s" })
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let sat_token = own.text;

    // It subscribes and acknowledges.
    relay.post(&mailbox, envelope(1)).await;
    let mut client = relay.connect().await;
    assert_eq!(
        client.subscribe_with(&mailbox, &sat_token, &sat).await,
        subscribed(&mailbox)
    );
    client.expect_message(&mailbox, 1, &envelope(1)).await;
    client.ack(&mailbox, 1).await;
    client.expect_silence().await;

    // Everything else is refused.
    let unauthorized = (StatusCode::UNAUTHORIZED, error("unauthorized"));
    let hash = tok().hash;
    for (reader, body) in [
        (&other, json!({ "token_hash": hash })),
        (&full, json!({ "push_url": null })),
        (&full, json!({ "token_hash": hash })),
        (&sat, json!({ "role": "full" })),
        (&sat, json!({ "role": "satellite" })),
    ] {
        assert_eq!(
            relay
                .put_reader_with(&mailbox, reader, &sat_token, body.clone())
                .await,
            unauthorized,
            "{body}"
        );
    }
    assert_eq!(
        relay.delete_reader_with(&mailbox, &full, &sat_token).await,
        unauthorized
    );
    assert_eq!(relay.delete_with(&mailbox, &sat_token).await, unauthorized);
    assert_eq!(relay.drop_shared(&mailbox, &sat_token).await, unauthorized);
    assert_eq!(
        relay.set_send_token(&mailbox, &sat_token, &hash).await,
        unauthorized
    );
    let mut probe = relay.connect().await;
    assert_eq!(
        probe.subscribe_with(&mailbox, &sat_token, &full).await,
        ws_error(&mailbox, "unauthorized")
    );
    assert_eq!(
        relay.derive_with(&sat_token).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        relay.put_slot(&sat_token, false).await,
        unauthorized.clone()
    );
    // The full reader of the same voucher mailbox may do both.
    assert_eq!(relay.derive_with(&mailbox.read).await, StatusCode::CREATED);
    assert_eq!(
        relay.put_slot(&mailbox.read, false).await.0,
        StatusCode::CREATED
    );

    // It may leave, with its own token.
    assert_eq!(
        relay.delete_reader_with(&mailbox, &sat, &sat_token).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(client.recv().await, ws_error(&mailbox, "unknown_reader"));
    assert_eq!(relay.sql_count("SELECT COUNT(*) FROM readers"), 1);
}

#[tokio::test]
async fn shared_token_and_full_reader_rights() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    let shared = mailbox.read.clone();
    let (full, legacy, sat) = (reader_id(1), reader_id(2), reader_id(3));
    let unauthorized = (StatusCode::UNAUTHORIZED, error("unauthorized"));

    // The shared token registers readers with a token and a role.
    let own = tok();
    assert_eq!(
        relay
            .put_reader_with(
                &mailbox,
                &full,
                &shared,
                json!({ "token_hash": own.hash, "role": "full" })
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    relay.put_reader(&mailbox, &legacy, None).await;
    let sat_token = tok();
    assert_eq!(
        relay
            .put_reader_with(
                &mailbox,
                &sat,
                &own.text,
                json!({ "token_hash": sat_token.hash, "role": "satellite" })
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    // It cannot drop itself or set the send token.
    assert_eq!(relay.drop_shared(&mailbox, &shared).await, unauthorized);
    let new_send = tok();
    assert_eq!(
        relay
            .set_send_token(&mailbox, &shared, &new_send.hash)
            .await,
        unauthorized
    );

    // A full reader sets the send token.
    assert_eq!(
        relay.set_send_token(&mailbox, &own.text, "zz").await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        relay
            .set_send_token(&mailbox, &own.text, &new_send.hash)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(relay.post(&mailbox, envelope(1)).await, unauthorized);
    let renewed = Mailbox {
        id: mailbox.id.clone(),
        read: own.text.clone(),
        send: new_send.text.clone(),
    };
    assert_eq!(
        relay.post(&renewed, envelope(1)).await.0,
        StatusCode::ACCEPTED
    );

    // A full reader drops the shared token: the legacy reader is locked out,
    // live and later, and the shared token authorizes nothing any more.
    let mut client = relay.connect().await;
    assert_eq!(
        client.subscribe(&mailbox, &legacy).await,
        subscribed(&mailbox)
    );
    client.expect_message(&mailbox, 1, &envelope(1)).await;
    assert_eq!(
        relay.drop_shared(&mailbox, &own.text).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(client.recv().await, ws_error(&mailbox, "unauthorized"));
    relay.post(&renewed, envelope(2)).await;
    client.expect_silence().await;
    assert_eq!(
        client.subscribe(&mailbox, &legacy).await,
        ws_error(&mailbox, "unauthorized")
    );
    assert_eq!(
        relay.put_reader(&mailbox, &legacy, None).await,
        unauthorized.clone()
    );
    assert_eq!(
        relay.put_reader(&mailbox, &reader_id(9), None).await,
        unauthorized.clone()
    );
    assert_eq!(
        relay.delete_reader_with(&mailbox, &full, &shared).await,
        unauthorized
    );
    assert_eq!(relay.delete_with(&mailbox, &shared).await, unauthorized);
    assert_eq!(relay.put_slot(&shared, false).await, unauthorized.clone());
    // Dropping again is fine.
    assert_eq!(
        relay.drop_shared(&mailbox, &own.text).await.0,
        StatusCode::NO_CONTENT
    );
    // The full reader and the satellite read on.
    let mut sat_client = relay.connect().await;
    assert_eq!(
        sat_client
            .subscribe_with(&mailbox, &sat_token.text, &sat)
            .await,
        subscribed(&mailbox)
    );
    sat_client.expect_message(&mailbox, 1, &envelope(1)).await;
    sat_client.expect_message(&mailbox, 2, &envelope(2)).await;

    // A full reader revokes the legacy reader and finally deletes the
    // mailbox.
    assert_eq!(
        relay
            .delete_reader_with(&mailbox, &legacy, &own.text)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        relay.delete_with(&mailbox, &own.text).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        sat_client.recv().await,
        ws_error(&mailbox, "unknown_mailbox")
    );

    // The shared token deletes a mailbox, as before.
    let other = relay.mailbox().await;
    assert_eq!(relay.delete(&other).await, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn revoked_reader_is_locked_out_and_learns_it() {
    let relay = Relay::start().await;
    let (full, removed) = (reader_id(1), reader_id(2));
    let mailbox = relay.reader_mailbox(&full).await;
    let own = tok();
    relay
        .put_reader_with(
            &mailbox,
            &removed,
            &mailbox.read,
            json!({ "token_hash": own.hash }),
        )
        .await;
    let mut client = relay.connect().await;
    assert_eq!(
        client.subscribe_with(&mailbox, &own.text, &removed).await,
        subscribed(&mailbox)
    );

    // Revoking ends the live subscription.
    assert_eq!(
        relay
            .delete_reader_with(&mailbox, &removed, &mailbox.read)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(client.recv().await, ws_error(&mailbox, "unknown_reader"));
    relay.post(&mailbox, envelope(1)).await;
    client.expect_silence().await;

    // Mailbox first, then reader, then token: the revoked device learns it
    // was removed, whatever token it shows.
    let unknown = Mailbox {
        id: ids::encode(&[7u8; 32]),
        read: own.text.clone(),
        send: String::new(),
    };
    assert_eq!(
        client.subscribe_with(&unknown, &own.text, &removed).await,
        ws_error(&unknown, "unknown_mailbox")
    );
    for token in [&own.text, &tok().text] {
        assert_eq!(
            client.subscribe_with(&mailbox, token, &removed).await,
            ws_error(&mailbox, "unknown_reader")
        );
    }
    assert_eq!(
        client.subscribe_with(&mailbox, &tok().text, &full).await,
        ws_error(&mailbox, "unauthorized")
    );
    client.ack(&mailbox, 1).await;
    assert_eq!(client.recv().await, ws_error(&mailbox, "bad_request"));

    // It cannot register itself again, nor leave.
    let unauthorized = (StatusCode::UNAUTHORIZED, error("unauthorized"));
    assert_eq!(
        relay
            .put_reader_with(
                &mailbox,
                &removed,
                &own.text,
                json!({ "token_hash": own.hash })
            )
            .await,
        unauthorized
    );
    assert_eq!(
        relay
            .delete_reader_with(&mailbox, &removed, &own.text)
            .await,
        unauthorized
    );
    assert_eq!(relay.put_slot(&own.text, false).await, unauthorized.clone());
    assert_eq!(relay.derive_with(&own.text).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn replaced_token_ends_reading_with_the_old_one() {
    let relay = Relay::start().await;
    let (full, other) = (reader_id(1), reader_id(2));
    let mailbox = relay.reader_mailbox(&full).await;
    let old = tok();
    relay
        .put_reader_with(
            &mailbox,
            &other,
            &mailbox.read,
            json!({ "token_hash": old.hash }),
        )
        .await;
    let mut client = relay.connect().await;
    assert_eq!(
        client.subscribe_with(&mailbox, &old.text, &other).await,
        subscribed(&mailbox)
    );
    relay.post(&mailbox, envelope(1)).await;
    client.expect_message(&mailbox, 1, &envelope(1)).await;

    // A full reader replaces the other reader's token.
    let new = tok();
    relay
        .put_reader_with(
            &mailbox,
            &other,
            &mailbox.read,
            json!({ "token_hash": new.hash }),
        )
        .await;
    assert_eq!(client.recv().await, ws_error(&mailbox, "unauthorized"));
    relay.post(&mailbox, envelope(2)).await;
    client.expect_silence().await;
    assert_eq!(
        client.subscribe_with(&mailbox, &new.text, &other).await,
        subscribed(&mailbox)
    );
    client.expect_message(&mailbox, 1, &envelope(1)).await;
    client.expect_message(&mailbox, 2, &envelope(2)).await;
}

#[tokio::test]
async fn quotas_hold_with_reader_tokens() {
    let relay = Relay::start().await;
    let full = reader_id(1);
    let unauthorized = (StatusCode::UNAUTHORIZED, error("unauthorized"));

    // A voucher mailbox with a shared token and two full readers: all three
    // tokens derive from the same ten.
    let parent = relay.voucher_mailbox().await;
    let (first, second) = (tok(), tok());
    for (reader, own) in [(&full, &first), (&reader_id(2), &second)] {
        relay
            .put_reader_with(
                &parent,
                reader,
                &parent.read,
                json!({ "token_hash": own.hash }),
            )
            .await;
    }
    for bearer in [&parent.read, &first.text, &second.text]
        .iter()
        .cycle()
        .take(10)
    {
        assert_eq!(relay.derive_with(bearer).await, StatusCode::CREATED);
    }
    for bearer in [&parent.read, &first.text, &second.text] {
        assert_eq!(
            relay.create_mailbox_with(Some(bearer)).await.1,
            error("mailbox_quota")
        );
    }

    // They share the family's two handover slots, and the per-token limit
    // still holds per token.
    assert_eq!(
        relay.put_slot(&first.text, true).await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        relay.put_slot(&second.text, false).await.0,
        StatusCode::CREATED
    );
    for bearer in [&parent.read, &first.text, &second.text] {
        assert_eq!(
            relay.put_slot(bearer, false).await,
            (StatusCode::TOO_MANY_REQUESTS, error("handovers_quota"))
        );
    }

    // A satellite's token counts nowhere, it is refused.
    let sat = tok();
    relay
        .put_reader_with(
            &parent,
            &reader_id(3),
            &first.text,
            json!({ "token_hash": sat.hash, "role": "satellite" }),
        )
        .await;
    assert_eq!(relay.put_slot(&sat.text, false).await, unauthorized.clone());
    assert_eq!(relay.derive_with(&sat.text).await, StatusCode::UNAUTHORIZED);

    // A full reader of a mailbox in the new form, created with the access
    // token, belongs to the owner and has the per-token limit only.
    let own = relay.reader_mailbox(&full).await;
    for _ in 0..2 {
        assert_eq!(
            relay.put_slot(&own.read, false).await.0,
            StatusCode::CREATED
        );
    }
    assert_eq!(
        relay.put_slot(&own.read, false).await,
        (StatusCode::TOO_MANY_REQUESTS, error("handovers_quota"))
    );
    // It cannot derive: only voucher mailboxes do.
    assert_eq!(relay.derive_with(&own.read).await, StatusCode::UNAUTHORIZED);
    // A full reader of a voucher mailbox in the new form derives.
    let voucher = relay.voucher().await;
    let (status, _, guest) = relay.create_reader_mailbox(&voucher, &full).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(relay.derive_with(&guest.read).await, StatusCode::CREATED);

    // At most 16 readers, with tokens or without.
    let mailbox = relay.reader_mailbox(&full).await;
    for n in 2..=16 {
        assert_eq!(
            relay
                .put_reader_with(
                    &mailbox,
                    &reader_id(n),
                    &mailbox.read,
                    json!({ "token_hash": tok().hash })
                )
                .await
                .0,
            StatusCode::NO_CONTENT
        );
    }
    assert_eq!(
        relay
            .put_reader_with(
                &mailbox,
                &reader_id(17),
                &mailbox.read,
                json!({ "token_hash": tok().hash })
            )
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

impl Relay {
    async fn revoke_readers(
        &self,
        mailbox: &Mailbox,
        bearer: &str,
        keep: &[&String],
        drop_shared: bool,
    ) -> (StatusCode, Value) {
        let body = json!({ "keep": keep, "drop_shared_token": drop_shared });
        self.request(
            reqwest::Method::POST,
            &format!("/mailboxes/{}/revoke-readers", mailbox.id),
            Some(bearer),
            Some(body.to_string().into()),
        )
        .await
    }
}

#[tokio::test]
async fn revoking_all_unknown_readers_removes_hidden_ones() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    let shared = mailbox.read.clone();
    let (caller, kept, removed, by_shared, by_full, sat) = (
        reader_id(1),
        reader_id(2),
        reader_id(3),
        reader_id(4),
        reader_id(5),
        reader_id(6),
    );
    let tokens: Vec<Tok> = (0..6).map(|_| tok()).collect();
    // The caller and a kept device with their own tokens, the removed
    // device R with its own token.
    for (reader, own) in [
        (&caller, &tokens[0]),
        (&kept, &tokens[1]),
        (&removed, &tokens[2]),
    ] {
        relay
            .put_reader_with(&mailbox, reader, &shared, json!({ "token_hash": own.hash }))
            .await;
    }
    // R plants hidden readers: one with the shared token, one with its own
    // full token, and a satellite.
    relay
        .put_reader_with(
            &mailbox,
            &by_shared,
            &shared,
            json!({ "token_hash": tokens[3].hash }),
        )
        .await;
    relay
        .put_reader_with(
            &mailbox,
            &by_full,
            &tokens[2].text,
            json!({ "token_hash": tokens[4].hash }),
        )
        .await;
    relay
        .put_reader_with(
            &mailbox,
            &sat,
            &tokens[2].text,
            json!({ "token_hash": tokens[5].hash, "role": "satellite" }),
        )
        .await;
    assert_eq!(relay.sql_count("SELECT COUNT(*) FROM readers"), 6);

    // Live subscriptions of all of them, and a message only the caller acks.
    let mut clients = Vec::new();
    for (reader, own) in [
        (&kept, &tokens[1]),
        (&removed, &tokens[2]),
        (&by_shared, &tokens[3]),
        (&by_full, &tokens[4]),
        (&sat, &tokens[5]),
    ] {
        let mut client = relay.connect().await;
        assert_eq!(
            client.subscribe_with(&mailbox, &own.text, reader).await,
            subscribed(&mailbox)
        );
        clients.push(client);
    }
    relay.post(&mailbox, envelope(1)).await;
    for client in &mut clients {
        client.expect_message(&mailbox, 1, &envelope(1)).await;
    }
    clients[0].ack(&mailbox, 1).await;
    clients[0].expect_silence().await;
    let mut own_client = relay.connect().await;
    assert_eq!(
        own_client
            .subscribe_with(&mailbox, &tokens[0].text, &caller)
            .await,
        subscribed(&mailbox)
    );
    own_client.expect_message(&mailbox, 1, &envelope(1)).await;
    own_client.ack(&mailbox, 1).await;
    own_client.expect_silence().await;
    assert_eq!(relay.waiting(&mailbox), 1);

    // Refused: the shared token, a satellite, a bad body.
    let unauthorized = (StatusCode::UNAUTHORIZED, error("unauthorized"));
    assert_eq!(
        relay.revoke_readers(&mailbox, &shared, &[], true).await,
        unauthorized
    );
    assert_eq!(
        relay
            .revoke_readers(&mailbox, &tokens[5].text, &[], true)
            .await,
        unauthorized
    );
    for body in [
        json!({ "keep": [] }),
        json!({ "keep": ["short"], "drop_shared_token": true }),
        json!({ "keep": (0..17).map(reader_id).collect::<Vec<_>>(), "drop_shared_token": true }),
    ] {
        assert_eq!(
            relay
                .request(
                    reqwest::Method::POST,
                    &format!("/mailboxes/{}/revoke-readers", mailbox.id),
                    Some(&tokens[0].text),
                    Some(body.to_string().into()),
                )
                .await,
            (StatusCode::BAD_REQUEST, error("bad_request")),
            "{body}"
        );
    }
    assert_eq!(relay.sql_count("SELECT COUNT(*) FROM readers"), 6);

    // The caller keeps only `kept`, without naming itself, and drops the
    // shared token in the same step.
    let (status, json) = relay
        .revoke_readers(&mailbox, &tokens[0].text, &[&kept], true)
        .await;
    assert_eq!(status, StatusCode::OK);
    let mut revoked: Vec<String> = json["revoked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    revoked.sort();
    let mut expected = vec![
        removed.clone(),
        by_shared.clone(),
        by_full.clone(),
        sat.clone(),
    ];
    expected.sort();
    assert_eq!(revoked, expected);
    assert_eq!(relay.sql_count("SELECT COUNT(*) FROM readers"), 2);
    // Their subscriptions end, and the message no longer waits for them.
    for client in &mut clients[1..] {
        assert_eq!(client.recv().await, ws_error(&mailbox, "unknown_reader"));
    }
    clients[0].expect_silence().await;
    own_client.expect_silence().await;
    assert_eq!(relay.waiting(&mailbox), 0);

    // None of them gets back in.
    for (reader, own) in [
        (&removed, &tokens[2]),
        (&by_shared, &tokens[3]),
        (&by_full, &tokens[4]),
        (&sat, &tokens[5]),
    ] {
        assert_eq!(
            clients[1].subscribe_with(&mailbox, &own.text, reader).await,
            ws_error(&mailbox, "unknown_reader")
        );
        assert_eq!(
            relay
                .put_reader_with(
                    &mailbox,
                    reader,
                    &own.text,
                    json!({ "token_hash": own.hash })
                )
                .await,
            unauthorized
        );
    }
    assert_eq!(
        relay
            .put_reader_with(
                &mailbox,
                &reader_id(9),
                &shared,
                json!({ "token_hash": tok().hash })
            )
            .await,
        unauthorized
    );

    // Kept readers read on; a repeat revokes nothing, and the caller is
    // kept even when it names nobody.
    relay.post(&mailbox, envelope(2)).await;
    clients[0].expect_message(&mailbox, 2, &envelope(2)).await;
    own_client.expect_message(&mailbox, 2, &envelope(2)).await;
    let (status, json) = relay
        .revoke_readers(&mailbox, &tokens[0].text, &[&kept], false)
        .await;
    assert_eq!((status, json), (StatusCode::OK, json!({ "revoked": [] })));
    let (status, json) = relay
        .revoke_readers(&mailbox, &tokens[0].text, &[], false)
        .await;
    assert_eq!(
        (status, json),
        (StatusCode::OK, json!({ "revoked": [kept] }))
    );
    assert_eq!(
        clients[0].recv().await,
        ws_error(&mailbox, "unknown_reader")
    );
    own_client.expect_silence().await;
    assert_eq!(relay.sql_count("SELECT COUNT(*) FROM readers"), 1);
}

#[tokio::test]
async fn revoking_readers_can_keep_the_shared_token() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    let (caller, legacy, other) = (reader_id(1), reader_id(2), reader_id(3));
    let own = tok();
    relay
        .put_reader_with(
            &mailbox,
            &caller,
            &mailbox.read,
            json!({ "token_hash": own.hash }),
        )
        .await;
    relay.put_reader(&mailbox, &legacy, None).await;
    relay.put_reader(&mailbox, &other, None).await;
    let mut client = relay.connect().await;
    assert_eq!(
        client.subscribe(&mailbox, &legacy).await,
        subscribed(&mailbox)
    );

    let (status, json) = relay
        .revoke_readers(&mailbox, &own.text, &[&legacy, &reader_id(9)], false)
        .await;
    assert_eq!(
        (status, json),
        (StatusCode::OK, json!({ "revoked": [other] }))
    );
    // The legacy reader stays subscribed with the shared token.
    client.expect_silence().await;
    relay.post(&mailbox, envelope(1)).await;
    client.expect_message(&mailbox, 1, &envelope(1)).await;
    assert_eq!(relay.sql_count("SELECT COUNT(*) FROM readers"), 2);

    // Unknown mailbox.
    let unknown = Mailbox {
        id: ids::encode(&[7u8; 32]),
        read: String::new(),
        send: String::new(),
    };
    assert_eq!(
        relay.revoke_readers(&unknown, &own.text, &[], true).await,
        (StatusCode::NOT_FOUND, error("unknown_mailbox"))
    );
}

/// Version 1 to 2 on top of [`MIGRATION_V1`], as in the relay of M9.
const MIGRATION_V2: &str = "
ALTER TABLE mailboxes ADD COLUMN derived_total INTEGER NOT NULL DEFAULT 0;
UPDATE mailboxes SET derived_total =
    (SELECT COUNT(*) FROM mailboxes AS child WHERE child.parent = mailboxes.id)
    WHERE origin = 1;
ALTER TABLE handovers ADD COLUMN family BLOB;
CREATE INDEX handovers_family ON handovers(family);
";

#[tokio::test]
async fn migration_from_version_2() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("relay.sqlite");
    let (read_raw, read) = token();
    let (send_raw, send) = token();
    let (fetch_raw, fetch) = token();
    let parent_id: [u8; 32] = ids::random();
    let child_id: [u8; 32] = ids::random();
    let slot: [u8; 32] = ids::random();
    let day = START.div_euclid(86_400);
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(SCHEMA_V0).unwrap();
        conn.execute_batch(MIGRATION_V1).unwrap();
        conn.execute_batch(MIGRATION_V2).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.execute(
            "INSERT INTO mailboxes VALUES (?1, ?2, ?3, 3, ?4, 1, NULL, 1)",
            rusqlite::params![
                &parent_id[..],
                &ids::sha256(&read_raw)[..],
                &ids::sha256(&send_raw)[..],
                day
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mailboxes VALUES (?1, ?2, ?2, 1, ?3, 2, ?4, 0)",
            rusqlite::params![
                &child_id[..],
                &ids::sha256(&child_id)[..],
                day,
                &parent_id[..]
            ],
        )
        .unwrap();
        for (reader, url) in [(1u8, Some("https://push.example.org/a")), (2, None)] {
            conn.execute(
                "INSERT INTO readers VALUES (?1, ?2, ?3, 1)",
                rusqlite::params![&parent_id[..], &[reader; 16][..], url],
            )
            .unwrap();
        }
        for seq in [1, 2] {
            conn.execute(
                "INSERT INTO messages VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![&parent_id[..], seq, START, &envelope(seq as u8)],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO handovers VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
            rusqlite::params![
                &slot[..],
                &[7u8; 10][..],
                &ids::sha256(&fetch_raw)[..],
                &ids::sha256(&read_raw)[..],
                START + 600,
                &parent_id[..]
            ],
        )
        .unwrap();
    }

    let relay = Relay::start_in(dir).await;
    let (version, migrated) = schema_of(&path);
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 3);
    // Everything is kept; the readers are legacy full readers.
    assert_eq!(relay.sql_count("SELECT COUNT(*) FROM mailboxes"), 2);
    assert_eq!(
        relay.sql_count(
            "SELECT COUNT(*) FROM readers WHERE token_hash IS NULL AND role = 1 AND last_ack = 1"
        ),
        2
    );
    assert_eq!(relay.sql_count("SELECT COUNT(*) FROM messages"), 2);
    assert_eq!(
        relay.sql_count("SELECT derived_total FROM mailboxes WHERE origin = 1"),
        1
    );
    assert_eq!(
        relay.sql_count("SELECT COUNT(*) FROM pragma_foreign_key_check"),
        0
    );
    assert_eq!(relay.get_handover(&slot, &fetch).await, Ok(vec![7; 10]));

    // The old read token is the shared token: legacy readers subscribe,
    // receive what they have not acknowledged and are woken.
    let parent = Mailbox {
        id: ids::encode(&parent_id),
        read,
        send,
    };
    let mut client = relay.connect().await;
    assert_eq!(
        client.subscribe(&parent, &reader_id(2)).await,
        subscribed(&parent)
    );
    client.expect_message(&parent, 2, &envelope(2)).await;
    assert_eq!(
        relay.post(&parent, envelope(3)).await.0,
        StatusCode::ACCEPTED
    );
    client.expect_message(&parent, 3, &envelope(3)).await;
    assert_eq!(
        relay.waker.take(),
        vec!["https://push.example.org/a".to_owned()]
    );
    // The voucher mailbox derives on, and the derived one counts.
    assert_eq!(relay.derive_with(&parent.read).await, StatusCode::CREATED);
    assert_eq!(
        relay.sql_count("SELECT derived_total FROM mailboxes WHERE origin = 1"),
        2
    );

    // Foreign keys are on again: deleting the mailbox takes its readers
    // and messages with it.
    assert_eq!(relay.delete(&parent).await, StatusCode::NO_CONTENT);
    assert_eq!(relay.sql_count("SELECT COUNT(*) FROM readers"), 0);
    assert_eq!(relay.sql_count("SELECT COUNT(*) FROM messages"), 0);
    assert_eq!(client.recv().await, ws_error(&parent, "unknown_mailbox"));

    // Opening again changes nothing, and a fresh database ends up identical.
    drop(Store::open(&path).unwrap());
    assert_eq!(schema_of(&path), (SCHEMA_VERSION, migrated.clone()));
    let fresh = tempfile::tempdir().unwrap();
    let fresh_path = fresh.path().join("relay.sqlite");
    drop(Store::open(&fresh_path).unwrap());
    assert_eq!(schema_of(&fresh_path), (SCHEMA_VERSION, migrated));
}
