//! Error responses: a status code and `{"error":"<code>"}`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// An error the relay reports to a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiError {
    /// 400 `bad_request`: the request is not readable.
    BadRequest,
    /// 401 `unauthorized`: token missing or wrong.
    Unauthorized,
    /// 404 `unknown_mailbox`: the mailbox does not exist (any more).
    UnknownMailbox,
    /// 404 `unknown_reader`: the reader is not registered.
    UnknownReader,
    /// 404 `unknown_handover`: no handover under this identifier (any more).
    UnknownHandover,
    /// 409 `handover_exists`: the identifier is taken or reserved.
    HandoverExists,
    /// 413 `too_large`: body longer than the limit.
    TooLarge,
    /// 429 `mailbox_quota`: the voucher mailbox has derived the maximum.
    MailboxQuota,
    /// 429 `handovers_quota`: too many waiting handovers for this token.
    HandoversQuota,
    /// 429 `mailbox_full`: too many waiting messages.
    MailboxFull,
    /// 429 `rate_limited`: too many deliveries in the last hour.
    RateLimited,
    /// 503 `handovers_full`: too many waiting handovers on the relay.
    HandoversFull,
    /// 500 `internal`: a fault in the relay itself.
    Internal,
}

impl ApiError {
    /// The code in the `error` field, also used in WebSocket error frames.
    pub fn code(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::Unauthorized => "unauthorized",
            Self::UnknownMailbox => "unknown_mailbox",
            Self::UnknownReader => "unknown_reader",
            Self::UnknownHandover => "unknown_handover",
            Self::HandoverExists => "handover_exists",
            Self::TooLarge => "too_large",
            Self::MailboxQuota => "mailbox_quota",
            Self::HandoversQuota => "handovers_quota",
            Self::MailboxFull => "mailbox_full",
            Self::RateLimited => "rate_limited",
            Self::HandoversFull => "handovers_full",
            Self::Internal => "internal",
        }
    }

    /// The HTTP status.
    pub fn status(self) -> StatusCode {
        match self {
            Self::BadRequest => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::UnknownMailbox | Self::UnknownReader | Self::UnknownHandover => {
                StatusCode::NOT_FOUND
            }
            Self::HandoverExists => StatusCode::CONFLICT,
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::MailboxQuota | Self::HandoversQuota | Self::MailboxFull | Self::RateLimited => {
                StatusCode::TOO_MANY_REQUESTS
            }
            Self::HandoversFull => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status(),
            Json(serde_json::json!({ "error": self.code() })),
        )
            .into_response()
    }
}
