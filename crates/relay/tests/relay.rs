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
        Self::start_in(tempfile::tempdir().unwrap()).await
    }

    /// Starts on the database `relay.sqlite` in `dir`, which may exist.
    async fn start_in(dir: tempfile::TempDir) -> Self {
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
    assert_eq!(version, 1);
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
    assert_eq!(schema_of(&path), (1, migrated.clone()));
    assert_eq!(relay.sql_count("SELECT COUNT(*) FROM mailboxes"), 3);
    assert_eq!(relay.sql_count("SELECT COUNT(*) FROM messages"), 2);

    // A fresh database ends up identical.
    let fresh = tempfile::tempdir().unwrap();
    let fresh_path = fresh.path().join("relay.sqlite");
    drop(Store::open(&fresh_path).unwrap());
    assert_eq!(schema_of(&fresh_path), (1, migrated));
}

#[tokio::test]
async fn newer_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("relay.sqlite");
    drop(Store::open(&path).unwrap());
    rusqlite::Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 2)
        .unwrap();
    assert!(Store::open(&path).is_err());
}
