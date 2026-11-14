//! The relay interface over HTTP(S) and WebSocket.
//!
//! Plain `http://` is allowed for local tests; in production the relay sits
//! behind a reverse proxy with TLS.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use lz_core::wire::{MailboxId, RelayUrl, Token};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use rustls::ClientConfig;
use rustls_platform_verifier::BuilderVerifierExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::Connector;
use tokio_tungstenite::tungstenite::Message;

use super::{
    Connection, Delivery, ReaderId, Relay, RelayError, ServerFrame, b64, from_b64, token_hash,
};

/// Time limit for a single HTTP call.
const TIMEOUT: Duration = Duration::from_secs(20);

/// A ping keeps idle stream connections alive through proxies and reveals
/// dead ones.
const PING_INTERVAL: Duration = Duration::from_secs(30);

/// Relays reached over the network.
#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    /// The same TLS settings for the stream connection.
    tls: Arc<ClientConfig>,
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
        })
    }

    fn call(&self, method: Method, relay: &RelayUrl, path: &str) -> RequestBuilder {
        self.client
            .request(method, format!("{}/v1/{path}", base(relay)))
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
    let response = request.send().await.map_err(|e| with_causes(&e))?;
    let status = response.status();
    if status == expected {
        return Ok(response);
    }
    let code = response
        .json::<ErrorBody>()
        .await
        .map(|body| body.error)
        .unwrap_or_default();
    if status == StatusCode::NOT_FOUND && code == "unknown_mailbox" {
        return Err(RelayError::UnknownMailbox);
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
    read_token_hash: String,
    send_token_hash: String,
}

#[derive(Deserialize)]
struct MailboxBody {
    mailbox: String,
}

#[derive(Serialize)]
struct ReaderBody<'a> {
    push_url: Option<&'a str>,
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

    async fn create_mailbox(
        &self,
        relay: &RelayUrl,
        authorisation: &Token,
        read_token: &Token,
        send_token: &Token,
    ) -> Result<MailboxId, RelayError> {
        let request = self
            .call(Method::POST, relay, "mailboxes")
            .bearer_auth(b64(authorisation))
            .json(&NewMailbox {
                read_token_hash: token_hash(read_token),
                send_token_hash: token_hash(send_token),
            });
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
        push_url: Option<&str>,
    ) -> Result<(), RelayError> {
        let path = format!("mailboxes/{}/readers/{}", b64(mailbox), b64(reader));
        let request = self
            .call(Method::PUT, relay, &path)
            .bearer_auth(b64(read_token))
            .json(&ReaderBody { push_url });
        send(request, StatusCode::NO_CONTENT).await.map(drop)
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

    async fn connect(
        &self,
        relay: &RelayUrl,
        deliveries: mpsc::UnboundedSender<Delivery>,
    ) -> Result<Box<dyn Connection>, RelayError> {
        let base = base(relay);
        let url = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}/v1/stream")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}/v1/stream")
        } else {
            return Err(transport("Adresse beginnt nicht mit http:// oder https://"));
        };
        let connector = Connector::Rustls(self.tls.clone());
        let connect =
            tokio_tungstenite::connect_async_tls_with_config(url, None, false, Some(connector));
        let (socket, _) = tokio::time::timeout(TIMEOUT, connect)
            .await
            .map_err(|_| transport("Zeitüberschreitung"))?
            .map_err(transport)?;
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
            .map_err(|_| transport("Verbindung getrennt"))
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

    use super::{ClientFrame, Http, parse_frame};
    use crate::relay::{Delivery, Relay, RelayError, ServerFrame, b64, token_hash};

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
                .create_mailbox(&relay, &[1; 32], &[2; 32], &token)
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
}
