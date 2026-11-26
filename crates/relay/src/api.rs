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
use crate::limits::{
    MAX_HANDOVER_BYTES, MAX_JSON_BODY, MAX_READERS_PER_MAILBOX, REVEAL_BYTES, VOUCHER_TTL_SECS,
};
use crate::store::{
    Access, CreateMailbox as Created, Family, Grant, NewHandover, NewMailbox, PostMessage,
    Principal, PutHandover, PutReader, PutReveal, ReadAccess, ReaderChange, Role, TakeHandover,
};

/// Header with the hash of the fetch token of a handover slot.
const FETCH_HASH: &str = "lz-fetch-hash";

/// Header with the hash of the reply token; reserves the reveal slot.
const REPLY_HASH: &str = "lz-reply-hash";
use crate::{ApiError, AppState};

/// Features of this relay, listed by `GET /v1/health`.
const FEATURES: &[&str] = &["reader_tokens"];

/// `GET /v1/health`: `200` with the features of this relay. Relays before
/// M10 answer with an empty body.
pub async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "features": FEATURES }))
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

/// Body of `POST /v1/mailboxes`: either the old form with the shared token
/// (`read_token_hash`) or the new one with the first full reader (`reader`
/// and `reader_token_hash`), never both.
#[derive(Deserialize)]
struct CreateMailbox {
    read_token_hash: Option<String>,
    reader: Option<String>,
    reader_token_hash: Option<String>,
    send_token_hash: String,
}

impl CreateMailbox {
    fn parse(&self) -> Option<NewMailbox> {
        let read = match (&self.read_token_hash, &self.reader, &self.reader_token_hash) {
            (Some(shared), None, None) => ReadAccess::Shared(ids::decode_hex_hash(shared)?),
            (None, Some(reader), Some(token_hash)) => {
                ReadAccess::Reader(ids::decode(reader)?, ids::decode_hex_hash(token_hash)?)
            }
            _ => return None,
        };
        Some(NewMailbox {
            read,
            send_token_hash: ids::decode_hex_hash(&self.send_token_hash)?,
        })
    }
}

/// `POST /v1/mailboxes`, with the access token, a voucher or the shared
/// token or a full reader's token of a voucher mailbox, tried in this order.
pub async fn create_mailbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<impl IntoResponse, ApiError> {
    let token = bearer(&headers)?;
    let request: CreateMailbox = read_json(body).await?;
    let mailbox = request.parse().ok_or(ApiError::BadRequest)?;
    let grant = if is_access_token(&state, &token) {
        Grant::AccessToken
    } else {
        Grant::Token(ids::sha256(&token))
    };
    let id: MailboxId = ids::random();
    let now = state.now();
    match state
        .db(move |s| s.create_mailbox(&id, &mailbox, grant, now))
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

/// `DELETE /v1/mailboxes/{mailbox}`, with the shared token or a full
/// reader's token.
pub async fn delete_mailbox(
    State(state): State<AppState>,
    Path(mailbox): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let mailbox = parse_mailbox(&mailbox)?;
    let hash = ids::sha256(&bearer(&headers)?);
    access(state.db(move |s| s.delete_mailbox(&mailbox, &hash)).await?)?;
    state.0.hub.notify(&mailbox, Event::MailboxDeleted);
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/mailboxes/{mailbox}/read-token`, with a full reader's token:
/// drops the shared token, so that legacy readers lose access. Live
/// subscriptions with it end.
pub async fn drop_read_token(
    State(state): State<AppState>,
    Path(mailbox): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let mailbox = parse_mailbox(&mailbox)?;
    let hash = ids::sha256(&bearer(&headers)?);
    access(
        state
            .db(move |s| s.drop_shared_token(&mailbox, &hash))
            .await?,
    )?;
    state.0.hub.notify(&mailbox, Event::AccessChanged);
    Ok(StatusCode::NO_CONTENT)
}

/// Body of `POST /v1/mailboxes/{mailbox}/revoke-readers`.
#[derive(Deserialize)]
struct RevokeReadersBody {
    /// Readers to keep, besides the caller.
    keep: Vec<String>,
    /// Whether to drop the shared token in the same step. Required, so that
    /// a removal decides it explicitly.
    drop_shared_token: bool,
}

/// `POST /v1/mailboxes/{mailbox}/revoke-readers`, with a full reader's
/// token: revokes every reader except the caller and those in `keep`, and
/// with `drop_shared_token` drops the shared token, in one step. Live
/// subscriptions of revoked readers end with `unknown_reader`, those with
/// the dropped shared token with `unauthorized`. Answers with the revoked
/// readers.
pub async fn revoke_readers(
    State(state): State<AppState>,
    Path(mailbox): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<impl IntoResponse, ApiError> {
    let mailbox = parse_mailbox(&mailbox)?;
    let hash = ids::sha256(&bearer(&headers)?);
    // The token is checked before the body is, as for readers.
    let principal = access(state.db(move |s| s.principal(&mailbox, &hash)).await?)?;
    if !matches!(principal, Principal::Reader(_, Role::Full)) {
        return Err(ApiError::Unauthorized);
    }
    let request: RevokeReadersBody = read_json(body).await?;
    if request.keep.len() > MAX_READERS_PER_MAILBOX {
        return Err(ApiError::BadRequest);
    }
    let keep = request
        .keep
        .iter()
        .map(|text| parse_reader(text))
        .collect::<Result<Vec<_>, _>>()?;
    let drop_shared = request.drop_shared_token;
    let revoked = access(
        state
            .db(move |s| s.revoke_readers(&mailbox, &hash, &keep, drop_shared))
            .await?,
    )?;
    for reader in &revoked {
        state.0.hub.notify(&mailbox, Event::ReaderRemoved(*reader));
    }
    if drop_shared {
        state.0.hub.notify(&mailbox, Event::AccessChanged);
    }
    let revoked: Vec<String> = revoked.iter().map(|id| ids::encode(id)).collect();
    Ok(Json(serde_json::json!({ "revoked": revoked })))
}

#[derive(Deserialize)]
struct SendTokenBody {
    send_token_hash: String,
}

/// `PUT /v1/mailboxes/{mailbox}/send-token`, with a full reader's token:
/// replaces the send token.
pub async fn put_send_token(
    State(state): State<AppState>,
    Path(mailbox): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, ApiError> {
    let mailbox = parse_mailbox(&mailbox)?;
    let hash = ids::sha256(&bearer(&headers)?);
    // The token is checked before the body is, as for readers.
    let principal = access(state.db(move |s| s.principal(&mailbox, &hash)).await?)?;
    if !matches!(principal, Principal::Reader(_, Role::Full)) {
        return Err(ApiError::Unauthorized);
    }
    let request: SendTokenBody = read_json(body).await?;
    let send_hash = ids::decode_hex_hash(&request.send_token_hash).ok_or(ApiError::BadRequest)?;
    access(
        state
            .db(move |s| s.set_send_token(&mailbox, &hash, &send_hash))
            .await?,
    )?;
    Ok(StatusCode::NO_CONTENT)
}

/// Body of `PUT /v1/mailboxes/{mailbox}/readers/{reader}`. A missing field
/// leaves the reader's value as it is; `"push_url":null` removes the push
/// URL. `token_hash` and `role` are new in M10.
#[derive(Deserialize)]
struct PutReaderBody {
    #[serde(default, deserialize_with = "present")]
    push_url: Option<Option<String>>,
    token_hash: Option<String>,
    role: Option<String>,
}

/// Tells a field given as `null` (`Some(None)`) from a missing one (`None`).
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

impl PutReaderBody {
    fn parse(self) -> Option<ReaderChange> {
        let token_hash = match &self.token_hash {
            Some(text) => Some(ids::decode_hex_hash(text)?),
            None => None,
        };
        let role = match self.role.as_deref() {
            None => None,
            Some("full") => Some(Role::Full),
            Some("satellite") => Some(Role::Satellite),
            Some(_) => return None,
        };
        Some(ReaderChange {
            push_url: self.push_url,
            token_hash,
            role,
        })
    }
}

/// `PUT /v1/mailboxes/{mailbox}/readers/{reader}`: with the shared token or
/// a full reader's token for any reader; with a satellite's token only for
/// itself, and without `role`.
pub async fn put_reader(
    State(state): State<AppState>,
    Path((mailbox, reader)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, ApiError> {
    let mailbox = parse_mailbox(&mailbox)?;
    let reader = parse_reader(&reader)?;
    let hash = ids::sha256(&bearer(&headers)?);
    // A wrong token is reported before an unreadable body.
    access(state.db(move |s| s.principal(&mailbox, &hash)).await?)?;
    let request: PutReaderBody = read_json(body).await?;
    let change = request.parse().ok_or(ApiError::BadRequest)?;
    if let Some(Some(url)) = &change.push_url {
        state
            .0
            .push_policy
            .validate(url)
            .map_err(|_| ApiError::BadRequest)?;
    }
    let now = state.now();
    let outcome = state
        .db(move |s| s.put_reader(&mailbox, &reader, &hash, &change, now))
        .await?;
    match outcome {
        PutReader::Ok { token_changed } => {
            if token_changed {
                state.0.hub.notify(&mailbox, Event::AccessChanged);
            }
            Ok(StatusCode::NO_CONTENT)
        }
        PutReader::UnknownMailbox => Err(ApiError::UnknownMailbox),
        PutReader::Unauthorized => Err(ApiError::Unauthorized),
        PutReader::TooManyReaders | PutReader::BadRequest => Err(ApiError::BadRequest),
    }
}

/// `DELETE /v1/mailboxes/{mailbox}/readers/{reader}`: with the shared token
/// or a full reader's token for any reader, with a reader's own token for
/// itself (leaving).
pub async fn delete_reader(
    State(state): State<AppState>,
    Path((mailbox, reader)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let mailbox = parse_mailbox(&mailbox)?;
    let reader = parse_reader(&reader)?;
    let hash = ids::sha256(&bearer(&headers)?);
    access(
        state
            .db(move |s| s.delete_reader(&mailbox, &reader, &hash))
            .await?,
    )?;
    state.0.hub.notify(&mailbox, Event::ReaderRemoved(reader));
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/mailboxes/{mailbox}/messages`, with the send token. The body must
/// be exactly one envelope long and is stored as it is.
pub async fn post_message(
    State(state): State<AppState>,
    Path(mailbox): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, ApiError> {
    let mailbox = parse_mailbox(&mailbox)?;
    let token = bearer(&headers)?;
    let id = mailbox;
    let send_hash = state
        .db(move |s| s.send_token_hash(&id))
        .await?
        .ok_or(ApiError::UnknownMailbox)?;
    if !ids::token_matches(&token, &send_hash) {
        return Err(ApiError::Unauthorized);
    }
    // Anything but an envelope is refused, so that the relay cannot carry
    // other data; a longer body is not read beyond one byte too many.
    let data = axum::body::to_bytes(body, lz_wire::ENVELOPE_LEN)
        .await
        .map_err(|_| ApiError::BadRequest)?;
    if data.len() != lz_wire::ENVELOPE_LEN {
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

/// `PUT /v1/handovers/{handover}`: a handover slot with the access token, the
/// shared token or a full reader's token of any mailbox of this relay, or,
/// without `Lz-Fetch-Hash`, a reserved reveal slot with the reply token.
/// Tokens of a voucher family share one small quota, so that guests cannot
/// keep the owner from adding devices. A satellite's token is refused.
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
    let family = if is_access_token(&state, &token) {
        Family::Owner
    } else {
        state
            .db(move |s| s.token_family(&auth_hash))
            .await?
            .ok_or(ApiError::Unauthorized)?
    };
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
                family,
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

/// Maps a refusal of the store to its error. An unknown mailbox wins over a
/// wrong token, so a removed client learns that its mailbox is gone.
pub(crate) fn access<T>(outcome: Access<T>) -> Result<T, ApiError> {
    match outcome {
        Access::Ok(value) => Ok(value),
        Access::UnknownMailbox => Err(ApiError::UnknownMailbox),
        Access::UnknownReader => Err(ApiError::UnknownReader),
        Access::Unauthorized => Err(ApiError::Unauthorized),
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
