//! The relay interface over HTTP(S) and WebSocket.
//!
//! Plain `http://` is allowed only to the own computer (`localhost` and
//! loopback addresses), for local tests: an address from someone else (an
//! invitation, a `hello`) could otherwise make the client send its tokens
//! unencrypted, or reach into the local network. In production the relay
//! sits behind a reverse proxy with TLS.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use futures_util::{SinkExt, StreamExt};
use lz_core::wire::{Hash, MailboxId, RelayUrl, Token};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use rustls::ClientConfig;
use rustls_platform_verifier::BuilderVerifierExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::Connector;
use tokio_tungstenite::tungstenite::Message;

use super::{
    Connection, Delivery, Features, MailboxRead, ReaderChange, ReaderId, Relay, RelayError,
    ServerFrame, b64, from_b64, hex, token_hash,
};

/// Header with the hash of the fetch token of a handover slot.
const FETCH_HASH: &str = "Lz-Fetch-Hash";

/// Header with the hash of the reply token; reserves the reveal slot.
const REPLY_HASH: &str = "Lz-Reply-Hash";

/// Time limit for a single HTTP call.
const TIMEOUT: Duration = Duration::from_secs(20);

/// How long a relay that lists no `reader_tokens` is believed; a relay
/// that lists it is believed for the life of the process, since relays are
/// not downgraded.
const NO_FEATURES_TTL: Duration = Duration::from_secs(10 * 60);

/// A ping keeps idle stream connections alive through proxies and reveals
/// dead ones.
const PING_INTERVAL: Duration = Duration::from_secs(30);

/// The relay's clock minus this device's, in seconds, from the `Date` header
/// of the last answer of any relay; [`UNKNOWN`] before the first. Shared by
/// the whole process, so that the window sees what the background fetch saw.
static CLOCK_OFFSET: AtomicI64 = AtomicI64::new(UNKNOWN);

const UNKNOWN: i64 = i64::MIN;

/// Notes the `Date` header of an answer of a relay, if it has a readable one.
fn observe_date(headers: &reqwest::header::HeaderMap) {
    if let Some(offset) = date_offset(headers, SystemTime::now()) {
        CLOCK_OFFSET.store(offset, Ordering::Relaxed);
    }
}

/// The relay's clock minus `now`, from a `Date` header.
fn date_offset(headers: &reqwest::header::HeaderMap, now: SystemTime) -> Option<i64> {
    let date = headers
        .get(reqwest::header::DATE)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| httpdate::parse_http_date(text).ok())?;
    Some(offset(date, now))
}

/// `relay - device` in whole seconds, negative if the device is ahead.
fn offset(relay: SystemTime, device: SystemTime) -> i64 {
    match relay.duration_since(device) {
        Ok(ahead) => i64::try_from(ahead.as_secs()).unwrap_or(i64::MAX),
        Err(behind) => -i64::try_from(behind.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

/// Relays reached over the network.
#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    /// The same TLS settings for the stream connection.
    tls: Arc<ClientConfig>,
    /// Features seen per relay, with the time they were asked.
    features: Arc<Mutex<HashMap<RelayUrl, (Features, Instant)>>>,
}

/// TLS with `ring`, certificates checked by the operating system: on Linux
/// against its certificate files, on Android by the system's own check,
/// including certificates a person installed. On Android this needs the
/// setup call of `rustls-platform-verifier` first; without it every
/// connection fails with an error.
fn tls() -> Result<ClientConfig, RelayError> {
    ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .and_then(|builder| builder.with_platform_verifier())
        .map(|builder| builder.with_no_client_auth())
        .map_err(transport)
}

impl Http {
    pub fn new() -> Result<Self, RelayError> {
        let tls = tls()?;
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .use_preconfigured_tls(tls.clone())
            .build()
            .map_err(transport)?;
        Ok(Self {
            client,
            tls: Arc::new(tls),
            features: Arc::default(),
        })
    }

    fn call(&self, method: Method, relay: &RelayUrl, path: &str) -> RequestBuilder {
        self.client
            .request(method, format!("{}/v1/{path}", base(relay)))
    }
}

/// `https`, or `http` to this computer only.
fn allowed_url(url: &reqwest::Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => url.host_str().is_some_and(|host| {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        }),
        _ => false,
    }
}

fn base(relay: &RelayUrl) -> &str {
    relay.as_str().trim_end_matches('/')
}

fn transport(error: impl std::fmt::Display) -> RelayError {
    RelayError::Transport(error.to_string())
}

/// Like [`transport`], with the causes: reqwest names only the step that
/// failed, the reason (such as an unknown certificate) is in the source.
fn with_causes(error: &(dyn std::error::Error + 'static)) -> RelayError {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    RelayError::Transport(text)
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

/// Sends the request and turns every status other than `expected` into an
/// error.
async fn send(request: RequestBuilder, expected: StatusCode) -> Result<Response, RelayError> {
    let allowed = request
        .try_clone()
        .and_then(|r| r.build().ok())
        .is_some_and(|r| allowed_url(r.url()));
    if !allowed {
        return Err(RelayError::BadAddress);
    }
    let response = request.send().await.map_err(|e| {
        let error = with_causes(&e);
        match error {
            // No connection: the request never left.
            RelayError::Transport(detail) if e.is_connect() => RelayError::Unreachable(detail),
            other => other,
        }
    })?;
    observe_date(response.headers());
    let status = response.status();
    if status == expected {
        return Ok(response);
    }
    let code = response
        .json::<ErrorBody>()
        .await
        .map(|body| body.error)
        .unwrap_or_default();
    match (status, code.as_str()) {
        (StatusCode::NOT_FOUND, "unknown_mailbox") => return Err(RelayError::UnknownMailbox),
        (StatusCode::NOT_FOUND, "unknown_handover") => return Err(RelayError::UnknownHandover),
        (StatusCode::CONFLICT, "handover_exists") => return Err(RelayError::HandoverExists),
        _ => {}
    }
    Err(RelayError::Refused {
        status: status.as_u16(),
        code,
    })
}

#[derive(Deserialize)]
struct VoucherBody {
    voucher: String,
}

#[derive(Serialize)]
struct NewMailbox {
    #[serde(skip_serializing_if = "Option::is_none")]
    read_token_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reader: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reader_token_hash: Option<String>,
    send_token_hash: String,
}

#[derive(Deserialize)]
struct HealthBody {
    #[serde(default)]
    features: Vec<String>,
}

/// The features in the body of `GET /v1/health`; an empty or unreadable
/// body (relays before M10) lists none.
fn parse_features(body: &[u8]) -> Features {
    let listed = serde_json::from_slice::<HealthBody>(body)
        .map(|b| b.features)
        .unwrap_or_default();
    Features {
        reader_tokens: listed.iter().any(|f| f == "reader_tokens"),
    }
}

#[derive(Serialize)]
struct RevokeBody {
    keep: Vec<String>,
    drop_shared_token: bool,
}

#[derive(Deserialize)]
struct RevokedBody {
    revoked: Vec<String>,
}

#[derive(Serialize)]
struct SendTokenBody {
    send_token_hash: String,
}

#[derive(Deserialize)]
struct MailboxBody {
    mailbox: String,
}

/// Fields that are `None` are left out; `push_url: Some(None)` is sent as
/// `null`.
#[derive(Serialize)]
struct ReaderBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    push_url: Option<Option<&'a str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'static str>,
}

impl<'a> From<ReaderChange<'a>> for ReaderBody<'a> {
    fn from(change: ReaderChange<'a>) -> Self {
        Self {
            push_url: change.push_url,
            token_hash: change.token.map(token_hash),
            role: change.role.map(super::Role::as_str),
        }
    }
}

impl Relay for Http {
    async fn create_voucher(
        &self,
        relay: &RelayUrl,
        access_token: &Token,
    ) -> Result<Token, RelayError> {
        let request = self
            .call(Method::POST, relay, "vouchers")
            .bearer_auth(b64(access_token));
        let body: VoucherBody = send(request, StatusCode::CREATED)
            .await?
            .json()
            .await
            .map_err(transport)?;
        from_b64(&body.voucher).ok_or_else(|| transport("unlesbarer Gutschein"))
    }

    async fn features(&self, relay: &RelayUrl) -> Result<Features, RelayError> {
        let known = self
            .features
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(relay)
            .copied();
        if let Some((features, at)) = known
            && (features.reader_tokens || at.elapsed() < NO_FEATURES_TTL)
        {
            return Ok(features);
        }
        let request = self.call(Method::GET, relay, "health");
        let body = send(request, StatusCode::OK)
            .await?
            .bytes()
            .await
            .map_err(transport)?;
        let features = parse_features(&body);
        self.features
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(*relay, (features, Instant::now()));
        Ok(features)
    }

    async fn create_mailbox(
        &self,
        relay: &RelayUrl,
        authorisation: &Token,
        read: MailboxRead<'_>,
        send_token: &Token,
    ) -> Result<MailboxId, RelayError> {
        let body = match read {
            MailboxRead::Shared(read_token) => NewMailbox {
                read_token_hash: Some(token_hash(read_token)),
                reader: None,
                reader_token_hash: None,
                send_token_hash: token_hash(send_token),
            },
            MailboxRead::Reader(reader, token) => NewMailbox {
                read_token_hash: None,
                reader: Some(b64(reader)),
                reader_token_hash: Some(token_hash(token)),
                send_token_hash: token_hash(send_token),
            },
        };
        let request = self
            .call(Method::POST, relay, "mailboxes")
            .bearer_auth(b64(authorisation))
            .json(&body);
        let body: MailboxBody = send(request, StatusCode::CREATED)
            .await?
            .json()
            .await
            .map_err(transport)?;
        from_b64(&body.mailbox).ok_or_else(|| transport("unlesbare Postfach-Kennung"))
    }

    async fn delete_mailbox(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        read_token: &Token,
    ) -> Result<(), RelayError> {
        let request = self
            .call(
                Method::DELETE,
                relay,
                &format!("mailboxes/{}", b64(mailbox)),
            )
            .bearer_auth(b64(read_token));
        send(request, StatusCode::NO_CONTENT).await.map(drop)
    }

    async fn put_reader(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        read_token: &Token,
        reader: &ReaderId,
        change: ReaderChange<'_>,
    ) -> Result<(), RelayError> {
        let path = format!("mailboxes/{}/readers/{}", b64(mailbox), b64(reader));
        let request = self
            .call(Method::PUT, relay, &path)
            .bearer_auth(b64(read_token))
            .json(&ReaderBody::from(change));
        send(request, StatusCode::NO_CONTENT).await.map(drop)
    }

    async fn drop_read_token(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        token: &Token,
    ) -> Result<(), RelayError> {
        let path = format!("mailboxes/{}/read-token", b64(mailbox));
        let request = self
            .call(Method::DELETE, relay, &path)
            .bearer_auth(b64(token));
        send(request, StatusCode::NO_CONTENT).await.map(drop)
    }

    async fn put_send_token(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        token: &Token,
        send_token: &Token,
    ) -> Result<(), RelayError> {
        let path = format!("mailboxes/{}/send-token", b64(mailbox));
        let request = self
            .call(Method::PUT, relay, &path)
            .bearer_auth(b64(token))
            .json(&SendTokenBody {
                send_token_hash: token_hash(send_token),
            });
        send(request, StatusCode::NO_CONTENT).await.map(drop)
    }

    async fn revoke_readers(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        token: &Token,
        keep: &[ReaderId],
        drop_shared_token: bool,
    ) -> Result<Vec<ReaderId>, RelayError> {
        let path = format!("mailboxes/{}/revoke-readers", b64(mailbox));
        let request = self
            .call(Method::POST, relay, &path)
            .bearer_auth(b64(token))
            .json(&RevokeBody {
                keep: keep.iter().map(|r| b64(r)).collect(),
                drop_shared_token,
            });
        let body: RevokedBody = send(request, StatusCode::OK)
            .await?
            .json()
            .await
            .map_err(transport)?;
        body.revoked
            .iter()
            .map(|text| from_b64(text).ok_or_else(|| transport("unreadable reader identifier")))
            .collect()
    }

    async fn delete_reader(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        read_token: &Token,
        reader: &ReaderId,
    ) -> Result<(), RelayError> {
        let path = format!("mailboxes/{}/readers/{}", b64(mailbox), b64(reader));
        let request = self
            .call(Method::DELETE, relay, &path)
            .bearer_auth(b64(read_token));
        send(request, StatusCode::NO_CONTENT).await.map(drop)
    }

    async fn put_slot(
        &self,
        relay: &RelayUrl,
        slot: &Hash,
        authorisation: &Token,
        fetch_hash: &Hash,
        reply_hash: Option<&Hash>,
        data: &[u8],
    ) -> Result<(), RelayError> {
        let mut request = self
            .call(Method::PUT, relay, &format!("handovers/{}", b64(slot)))
            .bearer_auth(b64(authorisation))
            .header(FETCH_HASH, hex(fetch_hash))
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(data.to_vec());
        if let Some(reply_hash) = reply_hash {
            request = request.header(REPLY_HASH, hex(reply_hash));
        }
        send(request, StatusCode::CREATED).await.map(drop)
    }

    async fn put_reveal(
        &self,
        relay: &RelayUrl,
        slot: &Hash,
        reply_token: &Token,
        value: &[u8; 32],
    ) -> Result<(), RelayError> {
        let request = self
            .call(Method::PUT, relay, &format!("handovers/{}", b64(slot)))
            .bearer_auth(b64(reply_token))
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(value.to_vec());
        send(request, StatusCode::CREATED).await.map(drop)
    }

    async fn get_slot(
        &self,
        relay: &RelayUrl,
        slot: &Hash,
        token: &Token,
    ) -> Result<Vec<u8>, RelayError> {
        let request = self
            .call(Method::GET, relay, &format!("handovers/{}", b64(slot)))
            .bearer_auth(b64(token));
        let body = send(request, StatusCode::OK)
            .await?
            .bytes()
            .await
            .map_err(transport)?;
        Ok(body.to_vec())
    }

    async fn deliver(
        &self,
        relay: &RelayUrl,
        mailbox: &MailboxId,
        send_token: &Token,
        envelope: &[u8],
    ) -> Result<(), RelayError> {
        let request = self
            .call(
                Method::POST,
                relay,
                &format!("mailboxes/{}/messages", b64(mailbox)),
            )
            .bearer_auth(b64(send_token))
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(envelope.to_vec());
        send(request, StatusCode::ACCEPTED).await.map(drop)
    }

    fn clock_offset(&self) -> Option<i64> {
        Some(CLOCK_OFFSET.load(Ordering::Relaxed)).filter(|&o| o != UNKNOWN)
    }

    async fn connect(
        &self,
        relay: &RelayUrl,
        deliveries: mpsc::UnboundedSender<Delivery>,
    ) -> Result<Box<dyn Connection>, RelayError> {
        let base = base(relay);
        if !reqwest::Url::parse(base).is_ok_and(|url| allowed_url(&url)) {
            return Err(RelayError::BadAddress);
        }
        let url = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}/v1/stream")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}/v1/stream")
        } else {
            return Err(RelayError::BadAddress);
        };
        let connector = Connector::Rustls(self.tls.clone());
        let connect =
            tokio_tungstenite::connect_async_tls_with_config(url, None, false, Some(connector));
        let (socket, response) = tokio::time::timeout(TIMEOUT, connect)
            .await
            .map_err(|_| RelayError::Timeout)?
            .map_err(transport)?;
        observe_date(response.headers());
        let (mut sink, mut stream) = socket.split();

        // Reads frames until the connection ends, then reports its end.
        let relay = *relay;
        let reader_deliveries = deliveries.clone();
        tokio::spawn(async move {
            while let Some(Ok(message)) = stream.next().await {
                let Message::Text(text) = message else {
                    continue;
                };
                if let Some(frame) = parse_frame(&text) {
                    let delivery = Delivery { relay, frame };
                    if reader_deliveries.send(delivery).is_err() {
                        return;
                    }
                }
            }
            let _ = reader_deliveries.send(Delivery {
                relay,
                frame: ServerFrame::Closed,
            });
        });

        // Writes frames from the connection handle and keeps pinging. Ends
        // when the handle is dropped.
        let (outgoing, mut frames) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let mut ping = tokio::time::interval(PING_INTERVAL);
            ping.tick().await;
            loop {
                let message = tokio::select! {
                    frame = frames.recv() => match frame {
                        Some(text) => Message::text(text),
                        None => break,
                    },
                    _ = ping.tick() => Message::Ping(Vec::new().into()),
                };
                if sink.send(message).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        Ok(Box::new(WebSocket { outgoing }))
    }
}

/// Frames from the client, as JSON text.
#[derive(Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum ClientFrame {
    Subscribe {
        mailbox: String,
        token: String,
        reader: String,
    },
    Ack {
        mailbox: String,
        seq: u64,
    },
}

/// Frames from the relay, as JSON text.
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum RelayFrame {
    Subscribed {
        mailbox: String,
    },
    Message {
        mailbox: String,
        seq: u64,
        data: String,
    },
    Error {
        #[serde(default)]
        mailbox: Option<String>,
        code: String,
    },
}

/// Reads a frame of the relay. Unknown or unreadable frames are skipped.
fn parse_frame(text: &str) -> Option<ServerFrame> {
    Some(match serde_json::from_str(text).ok()? {
        RelayFrame::Subscribed { mailbox } => ServerFrame::Subscribed {
            mailbox: from_b64(&mailbox)?,
        },
        RelayFrame::Message { mailbox, seq, data } => ServerFrame::Message {
            mailbox: from_b64(&mailbox)?,
            seq,
            data: base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, data)
                .ok()?,
        },
        RelayFrame::Error { mailbox, code } => ServerFrame::Error {
            mailbox: mailbox.as_deref().and_then(from_b64),
            code,
        },
    })
}

struct WebSocket {
    outgoing: mpsc::UnboundedSender<String>,
}

impl WebSocket {
    fn send(&self, frame: &ClientFrame) -> Result<(), RelayError> {
        let text = serde_json::to_string(frame).map_err(transport)?;
        self.outgoing
            .send(text)
            .map_err(|_| RelayError::Disconnected)
    }
}

impl Connection for WebSocket {
    fn subscribe(
        &mut self,
        mailbox: &MailboxId,
        read_token: &Token,
        reader: &ReaderId,
    ) -> Result<(), RelayError> {
        self.send(&ClientFrame::Subscribe {
            mailbox: b64(mailbox),
            token: b64(read_token),
            reader: b64(reader),
        })
    }

    fn ack(&mut self, mailbox: &MailboxId, seq: u64) -> Result<(), RelayError> {
        self.send(&ClientFrame::Ack {
            mailbox: b64(mailbox),
            seq,
        })
    }
}

#[cfg(test)]
mod tests {
    use futures_util::{SinkExt, StreamExt};
    use lz_core::wire::RelayUrl;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;
    use tokio_tungstenite::tungstenite::Message;

    use super::{ClientFrame, Http, parse_features, parse_frame};
    use crate::relay::{
        Delivery, MailboxRead, ReaderChange, Relay, RelayError, Role, ServerFrame, b64, token_hash,
    };

    /// Reads one HTTP request and answers it with `status` and `body`.
    /// Returns the request head and body.
    async fn answer_once(listener: &TcpListener, status: &str, body: &str) -> (String, Vec<u8>) {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut data = Vec::new();
        let head_end = loop {
            let mut buf = [0; 1024];
            let n = socket.read(&mut buf).await.unwrap();
            data.extend_from_slice(&buf[..n]);
            if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let head = String::from_utf8(data[..head_end].to_vec()).unwrap();
        let length: usize = head
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse().unwrap())
            })
            .unwrap_or(0);
        while data.len() < head_end + length {
            let mut buf = [0; 1024];
            let n = socket.read(&mut buf).await.unwrap();
            data.extend_from_slice(&buf[..n]);
        }
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        (head, data[head_end..].to_vec())
    }

    #[tokio::test]
    async fn http_calls_follow_the_interface() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = RelayUrl::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let http = Http::new().unwrap();
        let (mailbox, token) = ([3; 32], [4; 32]);

        let server = async {
            let body = format!(r#"{{"mailbox":"{}"}}"#, b64(&mailbox));
            let (head, body) = answer_once(&listener, "201 Created", &body).await;
            assert!(head.starts_with("POST /v1/mailboxes HTTP/1.1"), "{head}");
            assert!(head.contains(&format!("authorization: Bearer {}", b64(&[1; 32]))));
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["read_token_hash"], token_hash(&[2; 32]));
            assert_eq!(json["send_token_hash"], token_hash(&token));

            let (head, body) =
                answer_once(&listener, "404 Not Found", r#"{"error":"unknown_mailbox"}"#).await;
            let path = format!("POST /v1/mailboxes/{}/messages HTTP/1.1", b64(&mailbox));
            assert!(head.starts_with(&path), "{head}");
            assert!(head.contains("content-type: application/octet-stream"));
            assert_eq!(body, vec![7; 297]);

            answer_once(
                &listener,
                "429 Too Many Requests",
                r#"{"error":"rate_limited"}"#,
            )
            .await;
        };
        let client = async {
            let created = http
                .create_mailbox(&relay, &[1; 32], MailboxRead::Shared(&[2; 32]), &token)
                .await;
            assert_eq!(created, Ok(mailbox));
            let delivered = http.deliver(&relay, &mailbox, &token, &[7; 297]).await;
            assert_eq!(delivered, Err(RelayError::UnknownMailbox));
            let delivered = http.deliver(&relay, &mailbox, &token, &[7; 297]).await;
            assert_eq!(
                delivered,
                Err(RelayError::Refused {
                    status: 429,
                    code: "rate_limited".into()
                })
            );
        };
        tokio::join!(server, client);
    }

    #[test]
    fn features_come_from_health() {
        assert!(parse_features(br#"{"features":["reader_tokens"]}"#).reader_tokens);
        // Relays before M10 answer with an empty body.
        assert!(!parse_features(b"").reader_tokens);
        assert!(!parse_features(br#"{"features":[]}"#).reader_tokens);
        assert!(!parse_features(br#"{"features":["later"]}"#).reader_tokens);
        assert!(!parse_features(b"ok").reader_tokens);
    }

    #[tokio::test]
    async fn reader_token_calls_follow_the_interface() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = RelayUrl::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let http = Http::new().unwrap();
        let (mailbox, own, send, reader) = ([3; 32], [4; 32], [5; 32], [6; 16]);

        let server = async {
            let (head, _) =
                answer_once(&listener, "200 OK", r#"{"features":["reader_tokens"]}"#).await;
            assert!(head.starts_with("GET /v1/health HTTP/1.1"), "{head}");

            let body = format!(r#"{{"mailbox":"{}"}}"#, b64(&mailbox));
            let (head, body) = answer_once(&listener, "201 Created", &body).await;
            assert!(head.starts_with("POST /v1/mailboxes HTTP/1.1"), "{head}");
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                json,
                serde_json::json!({
                    "reader": b64(&reader),
                    "reader_token_hash": token_hash(&own),
                    "send_token_hash": token_hash(&send),
                })
            );

            // A missing field stays as it is; only what is set is sent.
            let (head, body) = answer_once(&listener, "204 No Content", "").await;
            let path = format!(
                "PUT /v1/mailboxes/{}/readers/{} HTTP/1.1",
                b64(&mailbox),
                b64(&reader)
            );
            assert!(head.starts_with(&path), "{head}");
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                json,
                serde_json::json!({"token_hash": token_hash(&send), "role": "satellite"})
            );
            let (_, body) = answer_once(&listener, "204 No Content", "").await;
            assert_eq!(body, br#"{"push_url":null}"#);

            let body = format!(r#"{{"revoked":["{}"]}}"#, b64(&[7; 16]));
            let (head, body) = answer_once(&listener, "200 OK", &body).await;
            let path = format!("POST /v1/mailboxes/{}/revoke-readers", b64(&mailbox));
            assert!(head.starts_with(&path), "{head}");
            assert!(head.contains(&format!("authorization: Bearer {}", b64(&own))));
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                json,
                serde_json::json!({"keep": [b64(&reader)], "drop_shared_token": true})
            );

            let (head, _) = answer_once(&listener, "204 No Content", "").await;
            let path = format!("DELETE /v1/mailboxes/{}/read-token", b64(&mailbox));
            assert!(head.starts_with(&path), "{head}");
            let (head, body) = answer_once(&listener, "204 No Content", "").await;
            let path = format!("PUT /v1/mailboxes/{}/send-token", b64(&mailbox));
            assert!(head.starts_with(&path), "{head}");
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["send_token_hash"], token_hash(&send));
        };
        let client = async {
            assert!(http.features(&relay).await.unwrap().reader_tokens);
            // Known from now on, without asking again.
            assert!(http.features(&relay).await.unwrap().reader_tokens);
            let created = http
                .create_mailbox(&relay, &[1; 32], MailboxRead::Reader(&reader, &own), &send)
                .await;
            assert_eq!(created, Ok(mailbox));
            let change = ReaderChange {
                token: Some(&send),
                role: Some(Role::Satellite),
                ..ReaderChange::default()
            };
            let put = http.put_reader(&relay, &mailbox, &own, &reader, change);
            assert_eq!(put.await, Ok(()));
            let put = http.put_reader(&relay, &mailbox, &own, &reader, ReaderChange::push(None));
            assert_eq!(put.await, Ok(()));
            let keep = [reader];
            let revoked = http.revoke_readers(&relay, &mailbox, &own, &keep, true);
            assert_eq!(revoked.await, Ok(vec![[7; 16]]));
            assert_eq!(http.drop_read_token(&relay, &mailbox, &own).await, Ok(()));
            let set = http.put_send_token(&relay, &mailbox, &own, &send);
            assert_eq!(set.await, Ok(()));
        };
        tokio::join!(server, client);
    }

    #[tokio::test]
    async fn handover_calls_follow_the_interface() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = RelayUrl::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let http = Http::new().unwrap();
        let (slot, token, fetch, reply) = ([1; 32], [2; 32], [3; 32], [4; 32]);

        let server = async {
            let (head, body) = answer_once(&listener, "201 Created", "{}").await;
            assert!(head.starts_with(&format!("PUT /v1/handovers/{} HTTP/1.1", b64(&slot))));
            assert!(head.contains(&format!("authorization: Bearer {}", b64(&token))));
            assert!(head.contains(&format!("lz-fetch-hash: {}", "03".repeat(32))));
            assert!(head.contains(&format!("lz-reply-hash: {}", "04".repeat(32))));
            assert_eq!(body, b"cd");
            let (head, _) =
                answer_once(&listener, "409 Conflict", r#"{"error":"handover_exists"}"#).await;
            assert!(!head.contains("lz-reply-hash"), "{head}");
            let (head, body) = answer_once(
                &listener,
                "404 Not Found",
                r#"{"error":"unknown_handover"}"#,
            )
            .await;
            assert!(head.starts_with("PUT /v1/handovers/"), "{head}");
            assert!(!head.contains("lz-fetch-hash"), "{head}");
            assert_eq!(body, vec![5; 32]);
            let (head, _) = answer_once(&listener, "200 OK", "ab").await;
            assert!(head.starts_with(&format!("GET /v1/handovers/{} HTTP/1.1", b64(&slot))));
            let (head, _) = answer_once(&listener, "204 No Content", "").await;
            let path = format!(
                "DELETE /v1/mailboxes/{}/readers/{}",
                b64(&slot),
                b64(&[6; 16])
            );
            assert!(head.starts_with(&path), "{head}");
        };
        let client = async {
            let put = http.put_slot(&relay, &slot, &token, &fetch, Some(&reply), b"cd");
            assert_eq!(put.await, Ok(()));
            let put = http.put_slot(&relay, &slot, &token, &fetch, None, b"cd");
            assert_eq!(put.await, Err(RelayError::HandoverExists));
            let reveal = http.put_reveal(&relay, &slot, &token, &[5; 32]);
            assert_eq!(reveal.await, Err(RelayError::UnknownHandover));
            assert_eq!(
                http.get_slot(&relay, &slot, &token).await,
                Ok(b"ab".to_vec())
            );
            let deleted = http.delete_reader(&relay, &slot, &token, &[6; 16]);
            assert_eq!(deleted.await, Ok(()));
        };
        tokio::join!(server, client);
    }

    #[tokio::test]
    async fn stream_subscribes_receives_and_acknowledges() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = RelayUrl::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let (mailbox, token, reader) = ([3; 32], [4; 32], [5; 16]);

        let server = async {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            let Some(Ok(Message::Text(text))) = ws.next().await else {
                panic!("no subscribe frame");
            };
            let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(frame["op"], "subscribe");
            assert_eq!(frame["mailbox"], b64(&mailbox));
            assert_eq!(frame["token"], b64(&token));
            assert_eq!(frame["reader"], b64(&reader));
            let subscribed = format!(r#"{{"op":"subscribed","mailbox":"{}"}}"#, b64(&mailbox));
            ws.send(Message::text(subscribed)).await.unwrap();
            let message = format!(
                r#"{{"op":"message","mailbox":"{}","seq":1,"data":"{}"}}"#,
                b64(&mailbox),
                b64(&[9; 297])
            );
            ws.send(Message::text(message)).await.unwrap();
            let Some(Ok(Message::Text(text))) = ws.next().await else {
                panic!("no ack frame");
            };
            assert_eq!(
                text.as_str(),
                format!(r#"{{"op":"ack","mailbox":"{}","seq":1}}"#, b64(&mailbox))
            );
            ws.close(None).await.unwrap();
        };
        let client = async {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let mut connection = Http::new().unwrap().connect(&relay, tx).await.unwrap();
            connection.subscribe(&mailbox, &token, &reader).unwrap();
            let mut next = async || rx.recv().await.map(|d: Delivery| d.frame);
            assert_eq!(next().await, Some(ServerFrame::Subscribed { mailbox }));
            assert_eq!(
                next().await,
                Some(ServerFrame::Message {
                    mailbox,
                    seq: 1,
                    data: vec![9; 297]
                })
            );
            connection.ack(&mailbox, 1).unwrap();
            assert_eq!(next().await, Some(ServerFrame::Closed));
        };
        tokio::join!(server, client);
    }

    #[test]
    fn frames_match_documentation() {
        let subscribe = ClientFrame::Subscribe {
            mailbox: "bQ".into(),
            token: "dA".into(),
            reader: "cg".into(),
        };
        assert_eq!(
            serde_json::to_string(&subscribe).unwrap(),
            r#"{"op":"subscribe","mailbox":"bQ","token":"dA","reader":"cg"}"#
        );
        let ack = ClientFrame::Ack {
            mailbox: "bQ".into(),
            seq: 17,
        };
        assert_eq!(
            serde_json::to_string(&ack).unwrap(),
            r#"{"op":"ack","mailbox":"bQ","seq":17}"#
        );

        let mailbox = [5; 32];
        let text = format!(
            r#"{{"op":"message","mailbox":"{}","seq":17,"data":"{}"}}"#,
            b64(&mailbox),
            b64(b"abc")
        );
        assert_eq!(
            parse_frame(&text),
            Some(ServerFrame::Message {
                mailbox,
                seq: 17,
                data: b"abc".to_vec()
            })
        );
        let text = format!(
            r#"{{"op":"error","mailbox":"{}","code":"unknown_mailbox"}}"#,
            b64(&mailbox)
        );
        assert_eq!(
            parse_frame(&text),
            Some(ServerFrame::Error {
                mailbox: Some(mailbox),
                code: "unknown_mailbox".into()
            })
        );
        assert_eq!(parse_frame(r#"{"op":"later"}"#), None);
    }

    #[test]
    fn offset_is_relay_minus_device() {
        use std::time::{Duration, UNIX_EPOCH};
        let device = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let ahead = device + Duration::from_secs(900);
        let behind = device - Duration::from_millis(900_500);
        assert_eq!(super::offset(ahead, device), 900);
        assert_eq!(super::offset(behind, device), -900);
        assert_eq!(super::offset(device, device), 0);
    }

    /// A `Date` header 15 minutes ahead gives the offset; none or an
    /// unreadable one gives nothing. (Through [`super::observe_date`] the
    /// offset is shared by the whole process, so tests check this part.)
    #[test]
    fn date_header_gives_the_clock_offset() {
        use reqwest::header::{DATE, HeaderMap, HeaderValue};
        use std::time::{Duration, SystemTime};
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let mut headers = HeaderMap::new();
        assert_eq!(super::date_offset(&headers, now), None);
        let date = httpdate::fmt_http_date(now + Duration::from_secs(900));
        headers.insert(DATE, HeaderValue::from_str(&date).unwrap());
        assert_eq!(super::date_offset(&headers, now), Some(900));
        headers.insert(DATE, HeaderValue::from_static("yesterday"));
        assert_eq!(super::date_offset(&headers, now), None);
    }

    #[test]
    fn clock_warning_from_five_minutes() {
        use crate::ctx::{clock_warning, clock_warning_text};
        use crate::i18n::Language;
        assert_eq!(clock_warning(None), None);
        assert_eq!(clock_warning(Some(300)), None);
        assert_eq!(clock_warning(Some(-300)), None);
        assert_eq!(clock_warning(Some(301)), Some(301));
        assert_eq!(clock_warning(Some(-900)), Some(-900));
        let t = Language::En.texts();
        assert!(clock_warning_text(t, -940).contains("more than 15 min ahead"));
        assert!(clock_warning_text(t, 301).contains("more than 5 min behind"));
    }

    #[test]
    fn plain_http_only_to_this_computer() {
        let ok = |text: &str| super::allowed_url(&reqwest::Url::parse(text).unwrap());
        assert!(ok("https://relay.example.org/v1/x"));
        assert!(ok("http://127.0.0.1:8080/v1/x"));
        assert!(ok("http://localhost:8080/v1/x"));
        assert!(ok("http://[::1]:8080/v1/x"));
        assert!(!ok("http://relay.example.org/v1/x"));
        assert!(!ok("http://192.168.1.2/v1/x"));
        assert!(!ok("ftp://relay.example.org/"));
    }

    #[tokio::test]
    async fn plain_http_elsewhere_is_refused_before_sending() {
        let http = Http::new().unwrap();
        let relay = RelayUrl::new("http://relay.example.org").unwrap();
        assert_eq!(
            http.create_voucher(&relay, &[0; 32]).await,
            Err(RelayError::BadAddress)
        );
        let (tx, _rx) = mpsc::unbounded_channel();
        assert!(matches!(
            http.connect(&relay, tx).await,
            Err(RelayError::BadAddress)
        ));
    }
}
