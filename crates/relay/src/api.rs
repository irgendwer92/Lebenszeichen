//! HTTP handlers of the relay.

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::hub::Event;
use crate::ids::{self, HandoverId, Hash, MailboxId, ReaderId, Token};
use crate::limits::{MAX_HANDOVER_BYTES, MAX_JSON_BODY, REVEAL_BYTES, VOUCHER_TTL_SECS};
use crate::store::{
    CreateMailbox as Created, Grant, MailboxAuth, NewHandover, PostMessage, PutHandover, PutReader,
    PutReveal, TakeHandover,
};

/// Header with the hash of the fetch token of a handover slot.
const FETCH_HASH: &str = "lz-fetch-hash";

/// Header with the hash of the reply token; reserves the reveal slot.
const REPLY_HASH: &str = "lz-reply-hash";
use crate::{ApiError, AppState};

/// `GET /v1/health`
pub async fn health() -> StatusCode {
    StatusCode::OK
}

/// `POST /v1/vouchers`, with the access token.
pub async fn create_voucher(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let token = bearer(&headers)?;
    if !is_access_token(&state, &token) {
        return Err(ApiError::Unauthorized);
    }
    let voucher: Token = ids::random();
    let hash = ids::sha256(&voucher);
    let expires_at = state.now() + VOUCHER_TTL_SECS;
    state
        .db(move |s| s.insert_voucher(&hash, expires_at))
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "voucher": ids::encode(&voucher),
            "expires_in": VOUCHER_TTL_SECS,
        })),
    ))
}

#[derive(Deserialize)]
struct CreateMailbox {
    read_token_hash: String,
    send_token_hash: String,
}

/// `POST /v1/mailboxes`, with the access token, a voucher or the read token
/// of a voucher mailbox, tried in this order.
pub async fn create_mailbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<impl IntoResponse, ApiError> {
    let token = bearer(&headers)?;
    let request: CreateMailbox = read_json(body).await?;
    let auth = MailboxAuth {
        read_token_hash: ids::decode_hex_hash(&request.read_token_hash)
            .ok_or(ApiError::BadRequest)?,
        send_token_hash: ids::decode_hex_hash(&request.send_token_hash)
            .ok_or(ApiError::BadRequest)?,
    };
    let grant = if is_access_token(&state, &token) {
        Grant::AccessToken
    } else {
        Grant::Token(ids::sha256(&token))
    };
    let id: MailboxId = ids::random();
    let now = state.now();
    match state
        .db(move |s| s.create_mailbox(&id, &auth, grant, now))
        .await?
    {
        Created::Created(_) => {}
        Created::Unauthorized => return Err(ApiError::Unauthorized),
        Created::Quota => return Err(ApiError::MailboxQuota),
    }
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "mailbox": ids::encode(&id) })),
    ))
}

/// `DELETE /v1/mailboxes/{mailbox}`, with the read token.
pub async fn delete_mailbox(
    State(state): State<AppState>,
    Path(mailbox): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let mailbox = parse_mailbox(&mailbox)?;
    authorize(&state, &headers, &mailbox, |a| a.read_token_hash).await?;
    state.db(move |s| s.delete_mailbox(&mailbox)).await?;
    state.0.hub.notify(&mailbox, Event::MailboxDeleted);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct PutReaderBody {
    push_url: Option<String>,
}

/// `PUT /v1/mailboxes/{mailbox}/readers/{reader}`, with the read token.
pub async fn put_reader(
    State(state): State<AppState>,
    Path((mailbox, reader)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, ApiError> {
    let mailbox = parse_mailbox(&mailbox)?;
    let reader = parse_reader(&reader)?;
    authorize(&state, &headers, &mailbox, |a| a.read_token_hash).await?;
    let request: PutReaderBody = read_json(body).await?;
    if let Some(url) = &request.push_url {
        state
            .0
            .push_policy
            .validate(url)
            .map_err(|_| ApiError::BadRequest)?;
    }
    let now = state.now();
    let outcome = state
        .db(move |s| s.put_reader(&mailbox, &reader, request.push_url.as_deref(), now))
        .await?;
    match outcome {
        PutReader::Ok => Ok(StatusCode::NO_CONTENT),
        PutReader::UnknownMailbox => Err(ApiError::UnknownMailbox),
        PutReader::TooManyReaders => Err(ApiError::BadRequest),
    }
}

/// `DELETE /v1/mailboxes/{mailbox}/readers/{reader}`, with the read token.
pub async fn delete_reader(
    State(state): State<AppState>,
    Path((mailbox, reader)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let mailbox = parse_mailbox(&mailbox)?;
    let reader = parse_reader(&reader)?;
    authorize(&state, &headers, &mailbox, |a| a.read_token_hash).await?;
    if !state
        .db(move |s| s.delete_reader(&mailbox, &reader))
        .await?
    {
        return Err(ApiError::UnknownReader);
    }
    state.0.hub.notify(&mailbox, Event::ReaderRemoved(reader));
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/mailboxes/{mailbox}/messages`, with the send token. The body is
/// stored as it is.
pub async fn post_message(
    State(state): State<AppState>,
    Path(mailbox): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, ApiError> {
    let mailbox = parse_mailbox(&mailbox)?;
    authorize(&state, &headers, &mailbox, |a| a.send_token_hash).await?;
    let data = axum::body::to_bytes(body, lz_wire::MAX_MESSAGE_BYTES)
        .await
        .map_err(|_| ApiError::TooLarge)?;
    if data.is_empty() {
        return Err(ApiError::BadRequest);
    }

    let now = state.now();
    if !state.limiter().try_acquire(&mailbox, now) {
        return Err(ApiError::RateLimited);
    }
    let outcome = state
        .db(move |s| s.post_message(&mailbox, &data, now))
        .await;
    match outcome {
        Ok(PostMessage::Stored(_)) => {}
        other => {
            state.limiter().release(&mailbox, now);
            return Err(match other {
                Ok(PostMessage::Full) => ApiError::MailboxFull,
                Ok(_) => ApiError::UnknownMailbox,
                Err(e) => e,
            });
        }
    }

    state.0.hub.notify(&mailbox, Event::Message);
    wake_readers(&state, mailbox).await;
    Ok(StatusCode::ACCEPTED)
}

/// `PUT /v1/handovers/{handover}`: a handover slot with the access token or
/// any read token of this relay, or, without `Lz-Fetch-Hash`, a reserved
/// reveal slot with the reply token.
pub async fn put_handover(
    State(state): State<AppState>,
    Path(handover): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, ApiError> {
    let id = parse_handover(&handover)?;
    let token = bearer(&headers)?;
    let fetch_hash = hash_header(&headers, FETCH_HASH)?;
    let reply_hash = hash_header(&headers, REPLY_HASH)?;
    let now = state.now();

    let Some(fetch_hash) = fetch_hash else {
        if reply_hash.is_some() {
            return Err(ApiError::BadRequest);
        }
        // A longer body is reported only after the token check.
        let data = axum::body::to_bytes(body, REVEAL_BYTES).await.ok();
        let outcome = state
            .db(move |s| s.put_reveal(&id, &token, data.as_deref(), now))
            .await?;
        return match outcome {
            PutReveal::Stored => Ok(StatusCode::CREATED),
            PutReveal::Unknown => Err(ApiError::UnknownHandover),
            PutReveal::Exists => Err(ApiError::HandoverExists),
            PutReveal::Unauthorized => Err(ApiError::Unauthorized),
            PutReveal::BadLength => Err(ApiError::BadRequest),
        };
    };

    let auth_hash = ids::sha256(&token);
    if !is_access_token(&state, &token) && !state.db(move |s| s.is_read_token(&auth_hash)).await? {
        return Err(ApiError::Unauthorized);
    }
    let data = axum::body::to_bytes(body, MAX_HANDOVER_BYTES)
        .await
        .map_err(|_| ApiError::TooLarge)?;
    if data.is_empty() {
        return Err(ApiError::BadRequest);
    }
    let outcome = state
        .db(move |s| {
            let slot = NewHandover {
                data: &data,
                fetch_hash,
                auth_hash,
                reply_hash,
            };
            s.put_handover(&id, &slot, now)
        })
        .await?;
    match outcome {
        PutHandover::Stored => Ok(StatusCode::CREATED),
        PutHandover::Exists => Err(ApiError::HandoverExists),
        PutHandover::Quota => Err(ApiError::HandoversQuota),
        PutHandover::Full => Err(ApiError::HandoversFull),
    }
}

/// `GET /v1/handovers/{handover}`, with the fetch token, for a reveal slot
/// the reply token. The slot is deleted on success.
pub async fn get_handover(
    State(state): State<AppState>,
    Path(handover): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let id = parse_handover(&handover)?;
    let token = bearer(&headers)?;
    let now = state.now();
    match state.db(move |s| s.take_handover(&id, &token, now)).await? {
        TakeHandover::Data(data) => {
            Ok(([(header::CONTENT_TYPE, "application/octet-stream")], data))
        }
        TakeHandover::Unknown => Err(ApiError::UnknownHandover),
        TakeHandover::Unauthorized => Err(ApiError::Unauthorized),
    }
}

/// Sends a wake signal to every reader with a push URL that has no open
/// subscription to the mailbox.
async fn wake_readers(state: &AppState, mailbox: MailboxId) {
    let Ok(targets) = state.db(move |s| s.push_targets(&mailbox)).await else {
        return;
    };
    if targets.is_empty() {
        return;
    }
    let subscribed = state.0.hub.subscribed_readers(&mailbox);
    for (reader, url) in targets {
        if !subscribed.contains(&reader) {
            state.0.waker.wake(url);
        }
    }
}

/// Reads the bearer token from the `Authorization` header.
fn bearer(headers: &HeaderMap) -> Result<Token, ApiError> {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::Unauthorized)?;
    let token = value
        .strip_prefix("Bearer ")
        .ok_or(ApiError::Unauthorized)?;
    ids::decode(token.trim()).ok_or(ApiError::Unauthorized)
}

fn is_access_token(state: &AppState, token: &Token) -> bool {
    ids::sha256(token).ct_eq(&state.0.access_token_hash).into()
}

/// Checks the bearer token against the mailbox. An unknown mailbox wins over
/// a wrong token, so a removed client learns that its mailbox is gone.
async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    mailbox: &MailboxId,
    which: fn(&MailboxAuth) -> Hash,
) -> Result<(), ApiError> {
    let token = bearer(headers)?;
    let id = *mailbox;
    let auth = state
        .db(move |s| s.mailbox_auth(&id))
        .await?
        .ok_or(ApiError::UnknownMailbox)?;
    if ids::token_matches(&token, &which(&auth)) {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

fn parse_mailbox(text: &str) -> Result<MailboxId, ApiError> {
    ids::decode(text).ok_or(ApiError::BadRequest)
}

fn parse_handover(text: &str) -> Result<HandoverId, ApiError> {
    ids::decode(text).ok_or(ApiError::BadRequest)
}

/// Reads an optional header with 64 hex digits.
fn hash_header(headers: &HeaderMap, name: &str) -> Result<Option<Hash>, ApiError> {
    headers
        .get(name)
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|text| ids::decode_hex_hash(text.trim()))
                .ok_or(ApiError::BadRequest)
        })
        .transpose()
}

fn parse_reader(text: &str) -> Result<ReaderId, ApiError> {
    ids::decode(text).ok_or(ApiError::BadRequest)
}

async fn read_json<T: serde::de::DeserializeOwned>(body: Body) -> Result<T, ApiError> {
    let bytes: Bytes = axum::body::to_bytes(body, MAX_JSON_BODY)
        .await
        .map_err(|_| ApiError::BadRequest)?;
    serde_json::from_slice(&bytes).map_err(|_| ApiError::BadRequest)
}
