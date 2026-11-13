//! Integration tests: the relay runs in-process on a random port with a
//! temporary SQLite file and a manual clock. No test leaves the machine; wake
//! signals are recorded instead of sent.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use lz_relay::ids::{self, MailboxId};
use lz_relay::push::{PushPolicy, Waker};
use lz_relay::store::Store;
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
        let dir = tempfile::tempdir().unwrap();
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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(lz_relay::serve(listener, state.clone()));
        Self {
            base: format!("http://{addr}/v1"),
            ws_url: format!("ws://{addr}/v1/stream"),
            clock,
            state,
            waker,
            http: reqwest::Client::new(),
            _dir: dir,
        }
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
    let (status, _) = relay
        .request(reqwest::Method::GET, "/health", None, None)
        .await;
    assert_eq!(status, StatusCode::OK);
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
async fn message_size_limit() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    let (status, json) = relay
        .post(&mailbox, vec![0; lz_wire::MAX_MESSAGE_BYTES + 1])
        .await;
    assert_eq!(
        (status, json),
        (StatusCode::PAYLOAD_TOO_LARGE, error("too_large"))
    );
    let (status, json) = relay.post(&mailbox, vec![0; 2048]).await;
    assert_eq!(
        (status, json),
        (StatusCode::PAYLOAD_TOO_LARGE, error("too_large"))
    );
    assert_eq!(
        relay
            .post(&mailbox, vec![0; lz_wire::MAX_MESSAGE_BYTES])
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        relay.post(&mailbox, ENVELOPE.to_vec()).await.0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        relay.post(&mailbox, Vec::new()).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(relay.waiting(&mailbox), 2);
}

#[tokio::test]
async fn rate_limit_and_full_mailbox() {
    let relay = Relay::start().await;
    let mailbox = relay.mailbox().await;
    for _ in 0..6 {
        assert_eq!(
            relay.post(&mailbox, ENVELOPE.to_vec()).await.0,
            StatusCode::ACCEPTED
        );
    }
    let (status, json) = relay.post(&mailbox, ENVELOPE.to_vec()).await;
    assert_eq!(
        (status, json),
        (StatusCode::TOO_MANY_REQUESTS, error("rate_limited"))
    );

    // The limit is per mailbox.
    let other = relay.mailbox().await;
    assert_eq!(
        relay.post(&other, ENVELOPE.to_vec()).await.0,
        StatusCode::ACCEPTED
    );

    // An hour later two more fit, then the mailbox holds 8 and is full.
    relay.clock.advance(3600);
    for _ in 0..2 {
        assert_eq!(
            relay.post(&mailbox, ENVELOPE.to_vec()).await.0,
            StatusCode::ACCEPTED
        );
    }
    assert_eq!(relay.waiting(&mailbox), 8);
    let (status, json) = relay.post(&mailbox, ENVELOPE.to_vec()).await;
    assert_eq!(
        (status, json),
        (StatusCode::TOO_MANY_REQUESTS, error("mailbox_full"))
    );
    // A refused message does not count against the rate limit.
    for _ in 0..4 {
        assert_eq!(
            relay.post(&mailbox, ENVELOPE.to_vec()).await.1,
            error("mailbox_full")
        );
    }
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
