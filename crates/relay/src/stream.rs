//! `GET /v1/stream`: delivery over a WebSocket.
//!
//! A connection subscribes to any number of mailboxes. After `subscribed` it
//! receives all waiting messages above the reader's last acknowledgement and
//! then every new one as it arrives. Each subscription remembers the highest
//! sequence number sent, so a message is sent at most once per connection.

use std::collections::HashMap;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;

use crate::api::access;
use crate::hub::{ConnId, Event, Hub};
use crate::ids::{self, Hash, MailboxId, ReaderId};
use crate::limits::{MAX_FRAME_LEN, MAX_SUBSCRIPTIONS_PER_CONNECTION};
use crate::store::Access;
use crate::{ApiError, AppState};

/// Upgrades the request to a WebSocket.
pub async fn stream(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(MAX_FRAME_LEN)
        .max_frame_size(MAX_FRAME_LEN)
        .on_upgrade(move |socket| async move {
            let _ = Connection::new(state, socket).run().await;
        })
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
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

struct Subscription {
    reader: ReaderId,
    /// `SHA-256` of the token it was opened with; checked again on every
    /// delivery and acknowledgement, so that a revoked token reads nothing
    /// new.
    token_hash: Hash,
    /// Highest sequence number sent on this connection.
    sent: i64,
}

/// Removes the connection's subscriptions from the hub however it ends.
struct HubGuard<'a> {
    hub: &'a Hub,
    conn: ConnId,
}

impl Drop for HubGuard<'_> {
    fn drop(&mut self) {
        self.hub.remove_connection(self.conn);
    }
}

struct Connection {
    state: AppState,
    socket: WebSocket,
    conn: ConnId,
    tx: mpsc::UnboundedSender<(MailboxId, Event)>,
    rx: mpsc::UnboundedReceiver<(MailboxId, Event)>,
    subscriptions: HashMap<MailboxId, Subscription>,
}

/// The socket is gone; stop serving the connection.
struct Closed;

impl Connection {
    fn new(state: AppState, socket: WebSocket) -> Self {
        let conn = state.0.hub.next_conn_id();
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            state,
            socket,
            conn,
            tx,
            rx,
            subscriptions: HashMap::new(),
        }
    }

    async fn run(mut self) -> Result<(), Closed> {
        let state = self.state.clone();
        let _guard = HubGuard {
            hub: &state.0.hub,
            conn: self.conn,
        };
        loop {
            tokio::select! {
                incoming = self.socket.recv() => match incoming {
                    Some(Ok(Message::Text(text))) => self.handle_frame(text.as_str()).await?,
                    Some(Ok(Message::Binary(_))) => self.send_error(None, ApiError::BadRequest).await?,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return Ok(()),
                },
                Some((mailbox, event)) = self.rx.recv() => self.handle_event(mailbox, event).await?,
            }
        }
    }

    async fn handle_frame(&mut self, text: &str) -> Result<(), Closed> {
        let Ok(frame) = serde_json::from_str::<ClientFrame>(text) else {
            return self.send_error(None, ApiError::BadRequest).await;
        };
        match frame {
            ClientFrame::Subscribe {
                mailbox,
                token,
                reader,
            } => self.subscribe(&mailbox, &token, &reader).await,
            ClientFrame::Ack { mailbox, seq } => self.ack(&mailbox, seq).await,
        }
    }

    async fn subscribe(
        &mut self,
        mailbox_text: &str,
        token: &str,
        reader: &str,
    ) -> Result<(), Closed> {
        let (Some(mailbox), Some(token), Some(reader)) = (
            ids::decode::<{ lz_wire::MAILBOX_ID_LEN }>(mailbox_text),
            ids::decode::<{ crate::limits::TOKEN_LEN }>(token),
            ids::decode::<{ crate::limits::READER_ID_LEN }>(reader),
        ) else {
            return self
                .send_error(Some(mailbox_text), ApiError::BadRequest)
                .await;
        };
        if self.subscriptions.len() >= MAX_SUBSCRIPTIONS_PER_CONNECTION
            && !self.subscriptions.contains_key(&mailbox)
        {
            return self
                .send_error(Some(mailbox_text), ApiError::BadRequest)
                .await;
        }
        let token_hash = ids::sha256(&token);
        let now = self.state.now();
        let opened = self
            .state
            .db(move |s| s.open_subscription(&mailbox, &reader, &token_hash, now))
            .await
            .and_then(access);
        match opened {
            Ok(last_ack) => {
                // Register before reading the backlog, so nothing that
                // arrives in between is missed.
                self.state
                    .0
                    .hub
                    .subscribe(mailbox, self.conn, reader, self.tx.clone());
                self.subscriptions.insert(
                    mailbox,
                    Subscription {
                        reader,
                        token_hash,
                        sent: last_ack,
                    },
                );
                self.send(json!({ "op": "subscribed", "mailbox": mailbox_text }))
                    .await?;
                self.deliver(mailbox).await
            }
            Err(error) => {
                if self.subscriptions.remove(&mailbox).is_some() {
                    self.state.0.hub.unsubscribe(&mailbox, self.conn);
                }
                self.send_error(Some(mailbox_text), error).await
            }
        }
    }

    async fn ack(&mut self, mailbox_text: &str, seq: u64) -> Result<(), Closed> {
        let Some(mailbox) = ids::decode::<{ lz_wire::MAILBOX_ID_LEN }>(mailbox_text) else {
            return self
                .send_error(Some(mailbox_text), ApiError::BadRequest)
                .await;
        };
        let Some(sub) = self.subscriptions.get(&mailbox) else {
            return self
                .send_error(Some(mailbox_text), ApiError::BadRequest)
                .await;
        };
        let (reader, token_hash) = (sub.reader, sub.token_hash);
        let seq = i64::try_from(seq).unwrap_or(i64::MAX);
        let now = self.state.now();
        match self
            .state
            .db(move |s| s.ack(&mailbox, &reader, &token_hash, seq, now))
            .await
        {
            Ok(Access::Ok(())) => Ok(()),
            Ok(denied) => self.refused(&mailbox, denied).await,
            Err(error) => self.send_error(Some(mailbox_text), error).await,
        }
    }

    async fn handle_event(&mut self, mailbox: MailboxId, event: Event) -> Result<(), Closed> {
        let Some(own_reader) = self.subscriptions.get(&mailbox).map(|s| s.reader) else {
            return Ok(());
        };
        let text = ids::encode(&mailbox);
        match event {
            Event::Message => self.deliver(mailbox).await,
            Event::MailboxDeleted => {
                self.drop_subscription(&mailbox);
                self.send_error(Some(&text), ApiError::UnknownMailbox).await
            }
            Event::ReaderRemoved(reader) if reader == own_reader => {
                self.drop_subscription(&mailbox);
                self.send_error(Some(&text), ApiError::UnknownReader).await
            }
            Event::ReaderRemoved(_) => Ok(()),
            Event::AccessChanged => {
                let Some(sub) = self.subscriptions.get(&mailbox) else {
                    return Ok(());
                };
                let (reader, token_hash) = (sub.reader, sub.token_hash);
                match self
                    .state
                    .db(move |s| s.check_subscription(&mailbox, &reader, &token_hash))
                    .await
                {
                    Ok(Access::Ok(())) => Ok(()),
                    Ok(denied) => self.refused(&mailbox, denied).await,
                    Err(error) => self.send_error(Some(&text), error).await,
                }
            }
        }
    }

    /// Ends a subscription that is no longer valid and tells the client why.
    async fn refused<T>(&mut self, mailbox: &MailboxId, denied: Access<T>) -> Result<(), Closed> {
        let error = match access(denied) {
            Ok(_) => return Ok(()),
            Err(error) => error,
        };
        self.drop_subscription(mailbox);
        self.send_error(Some(&ids::encode(mailbox)), error).await
    }

    /// Sends every stored message above what this connection already sent.
    async fn deliver(&mut self, mailbox: MailboxId) -> Result<(), Closed> {
        let Some((reader, token_hash, sent)) = self
            .subscriptions
            .get(&mailbox)
            .map(|s| (s.reader, s.token_hash, s.sent))
        else {
            return Ok(());
        };
        let messages = match self
            .state
            .db(move |s| s.messages_for(&mailbox, &reader, &token_hash, sent))
            .await
        {
            Ok(Access::Ok(messages)) => messages,
            Ok(denied) => return self.refused(&mailbox, denied).await,
            Err(error) => return self.send_error(Some(&ids::encode(&mailbox)), error).await,
        };
        let text = ids::encode(&mailbox);
        for (seq, data) in messages {
            self.send(json!({
                "op": "message",
                "mailbox": text,
                "seq": seq,
                "data": ids::encode(&data),
            }))
            .await?;
            if let Some(sub) = self.subscriptions.get_mut(&mailbox) {
                sub.sent = sub.sent.max(seq);
            }
        }
        Ok(())
    }

    fn drop_subscription(&mut self, mailbox: &MailboxId) {
        self.subscriptions.remove(mailbox);
        self.state.0.hub.unsubscribe(mailbox, self.conn);
    }

    async fn send_error(&mut self, mailbox: Option<&str>, error: ApiError) -> Result<(), Closed> {
        let frame = match mailbox {
            Some(mailbox) => json!({ "op": "error", "mailbox": mailbox, "code": error.code() }),
            None => json!({ "op": "error", "code": error.code() }),
        };
        self.send(frame).await
    }

    async fn send(&mut self, frame: serde_json::Value) -> Result<(), Closed> {
        self.socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .map_err(|_| Closed)
    }
}
