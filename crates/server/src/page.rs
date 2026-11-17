//! The setup page and its JSON API (`docs/m8b-container.md`, "Page API").
//!
//! Every connection from a peer not on the allow list is closed before a
//! byte of it is read. In Home Assistant the only peer is the Supervisor,
//! which strips the ingress prefix (and sends it in `X-Ingress-Path`), so
//! everything is served under `/` and the page uses relative URLs only.

use std::convert::Infallible;
use std::io;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::{Body, Bytes, Incoming};
use hyper::header::{self, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use lz_client::ctx::{ContactView, Ctx, Overview, Status};
use lz_client::i18n::{Arg, Texts};
use lz_client::local::MAX_CONNECTIONS;
use lz_client::relay::Http;
use lz_client::show::{self, reported, when};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::net::TcpListener;

use crate::join::{self, Refused};
use crate::server::Shared;

/// Largest request body in bytes.
const MAX_BODY: usize = 4096;

/// Most bytes of a request head; hyper does not accept less than 8 KiB.
const MAX_HEAD: usize = 16 * 1024;

/// How long a client may take to send the request head.
const HEADER_TIMEOUT: Duration = Duration::from_secs(5);

/// How long one connection may last, request and answer together.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

const CSP: &str = "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'";

/// The files of the page, embedded: path, content type, content.
const FILES: &[(&str, &str, &[u8])] = &[
    (
        "/",
        "text/html; charset=utf-8",
        include_bytes!("../page/index.html"),
    ),
    (
        "/index.html",
        "text/html; charset=utf-8",
        include_bytes!("../page/index.html"),
    ),
    (
        "/page.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../page/page.js"),
    ),
    (
        "/page.css",
        "text/css; charset=utf-8",
        include_bytes!("../page/page.css"),
    ),
];

type Answer = Response<Full<Bytes>>;

/// Whether `peer` is on the allow list. IPv4 addresses mapped into IPv6
/// count as IPv4.
pub fn allowed(allow: &[IpAddr], peer: IpAddr) -> bool {
    let peer = peer.to_canonical();
    allow.iter().any(|ip| ip.to_canonical() == peer)
}

/// Serves the page until the future is dropped, each connection in its own
/// task. Connections of peers not on `allow` are closed at once.
pub async fn serve(
    listener: TcpListener,
    shared: Arc<Shared>,
    allow: Vec<IpAddr>,
) -> io::Result<()> {
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(_) => {
                // For example too many open files: wait instead of spinning.
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        if !allowed(&allow, peer.ip()) {
            drop(stream);
            continue;
        }
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            continue;
        };
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            serve_connection(stream, shared).await;
            drop(permit);
        });
    }
}

async fn serve_connection(stream: tokio::net::TcpStream, shared: Arc<Shared>) {
    let service = service_fn(move |request| {
        let shared = Arc::clone(&shared);
        async move { Ok::<_, Infallible>(handle(request, shared).await) }
    });
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_TIMEOUT)
        .keep_alive(false)
        .max_buf_size(MAX_HEAD);
    let connection = builder.serve_connection(TokioIo::new(stream), service);
    let _ = tokio::time::timeout(CONNECTION_TIMEOUT, connection).await;
}

/// Answers one request, with the strict headers on every answer.
async fn handle(request: Request<Incoming>, shared: Arc<Shared>) -> Answer {
    let mut answer = route(request, shared).await;
    let headers = answer.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    answer
}

async fn route(request: Request<Incoming>, shared: Arc<Shared>) -> Answer {
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    if let Some((_, content_type, content)) = FILES.iter().find(|(p, _, _)| *p == path) {
        if method != Method::GET {
            return not_allowed(&shared, "GET");
        }
        let mut answer = Response::new(Full::new(Bytes::from_static(content)));
        answer
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
        return answer;
    }
    let expected = match path.as_str() {
        "/api/texts" | "/api/status" => Method::GET,
        "/api/join" | "/api/join/answer" | "/api/join/cancel" | "/api/token/renew" => Method::POST,
        "/api/state" => Method::DELETE,
        _ if contact_of(&path).is_some() => Method::PUT,
        _ => return error(&shared, StatusCode::NOT_FOUND, "server-error-not-found"),
    };
    if method != expected {
        return not_allowed(&shared, expected.as_str());
    }
    match path.as_str() {
        "/api/texts" => json_answer(
            StatusCode::OK,
            &json!({ "lang": shared.lang.tag(), "texts": shared.lang.raw_texts("web-") }),
        ),
        "/api/status" => match blocking(&shared, status).await {
            Ok(value) => json_answer(StatusCode::OK, &value),
            Err(answer) => *answer,
        },
        "/api/join" => start_join(request, &shared).await,
        "/api/join/answer" => {
            let body: JoinAnswer = match read_json(request, &shared).await {
                Ok(body) => body,
                Err(answer) => return *answer,
            };
            if join::answer(&shared, body.matches) {
                empty(StatusCode::NO_CONTENT)
            } else {
                error(&shared, StatusCode::CONFLICT, "server-error-no-join")
            }
        }
        "/api/join/cancel" => {
            join::cancel(&shared);
            empty(StatusCode::NO_CONTENT)
        }
        "/api/state" => match with_device(&shared, |ctx| ctx.end_state()).await {
            Ok(()) => empty(StatusCode::NO_CONTENT),
            Err(answer) => *answer,
        },
        "/api/token/renew" => match with_device(&shared, |ctx| ctx.renew_report_token()).await {
            Ok(token) => json_answer(StatusCode::OK, &json!({ "token": token })),
            Err(answer) => *answer,
        },
        _ => {
            let Some(name) = contact_of(&path) else {
                unreachable!("checked above")
            };
            set_reports(request, &shared, name).await
        }
    }
}

/// The name in `/api/contacts/<name>/reports`, URL-decoded; `None` for
/// another path or a name that does not decode.
fn contact_of(path: &str) -> Option<String> {
    let name = path
        .strip_prefix("/api/contacts/")?
        .strip_suffix("/reports")?;
    if name.is_empty() || name.contains('/') {
        return None;
    }
    percent_decode(name)
}

/// Decodes `%XX` escapes; the result must be UTF-8.
pub fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[derive(Deserialize)]
struct JoinBody {
    #[serde(default)]
    relay: Option<String>,
    name: String,
}

#[derive(Deserialize)]
struct JoinAnswer {
    matches: bool,
}

#[derive(Deserialize)]
struct ReportsBody {
    allowed: bool,
    #[serde(default)]
    delay_minutes: Option<u32>,
}

/// `POST api/join`.
async fn start_join(request: Request<Incoming>, shared: &Arc<Shared>) -> Answer {
    let body: JoinBody = match read_json(request, shared).await {
        Ok(body) => body,
        Err(answer) => return *answer,
    };
    let relay = body
        .relay
        .filter(|relay| !relay.trim().is_empty())
        .or_else(|| shared.default_relay.clone());
    let Some(relay) = relay else {
        return error(shared, StatusCode::BAD_REQUEST, "server-error-no-relay");
    };
    match join::start(shared, relay, body.name) {
        Ok(()) => empty(StatusCode::ACCEPTED),
        Err(Refused::Running) => error(shared, StatusCode::CONFLICT, "app-error-flow-running"),
        Err(Refused::HasDevice) => {
            let path = shared.state.display().to_string();
            let text = shared.text_with("error-join-has-device", "path", &path);
            error_text(StatusCode::CONFLICT, &text)
        }
    }
}

/// `PUT api/contacts/<name>/reports`.
async fn set_reports(request: Request<Incoming>, shared: &Arc<Shared>, name: String) -> Answer {
    let body: ReportsBody = match read_json(request, shared).await {
        Ok(body) => body,
        Err(answer) => return *answer,
    };
    let result = with_device(shared, move |ctx| {
        ctx.set_report_setting(&name, body.allowed, body.delay_minutes)
            .map(drop)
    })
    .await;
    match result {
        Ok(()) => empty(StatusCode::NO_CONTENT),
        Err(answer) => *answer,
    }
}

/// `GET api/status`.
pub fn status(shared: &Shared) -> lz_client::ctx::Result<Value> {
    let (joining, join) = {
        let join = shared.join.lock().expect("lock poisoned");
        (join.running(), join.view.clone())
    };
    let exists = shared.state.exists();
    let setup = if joining {
        "joining"
    } else if exists {
        "ready"
    } else {
        "none"
    };
    let (mut device, mut state, mut contacts, mut token) =
        (Value::Null, Value::Null, Vec::new(), Value::Null);
    if exists && !joining {
        let ctx = shared.quiet_ctx();
        let t = shared.texts();
        let devices = ctx.devices()?;
        let overview = ctx.overview()?;
        let this = devices.iter().find(|d| d.this);
        device = json!({
            "name": this.map(|d| d.name.as_str()).unwrap_or_default(),
            "others": devices.iter().filter(|d| !d.this).map(|d| &d.name).collect::<Vec<_>>(),
        });
        state = state_json(&overview, t);
        contacts = overview
            .contacts
            .into_iter()
            .map(|view| contact_json(view, overview.now, t))
            .collect();
        token = ctx.report_token()?.map_or(Value::Null, Value::String);
    }
    Ok(json!({
        "setup": setup,
        "default_relay": shared.default_relay,
        "join": join,
        "device": device,
        "state": state,
        "contacts": contacts,
        "token": token,
        "socket": shared.socket.display().to_string(),
        "http": shared.http.map(|address| json!({ "address": address.to_string() })),
    }))
}

fn state_json(overview: &Overview, t: Texts) -> Value {
    let Some(state) = &overview.state else {
        return Value::Null;
    };
    let text = t.text(
        "app-reported-state",
        &[
            ("reason", Arg::Text(&show::reported_state(t, &state.reason))),
            ("until", Arg::Text(&when(t, state.until, overview.now))),
            (
                "origin",
                Arg::Text(&show::state_origin(t, state.shared, state.handed)),
            ),
        ],
    );
    json!({
        "reason": state.reason,
        "until": state.until,
        "shared": state.shared,
        "handed": state.handed,
        "text": text,
    })
}

fn contact_json(view: ContactView, now: u64, t: Texts) -> Value {
    let paired = view.status == Status::Paired;
    let incoming = view.incoming.filter(|_| paired).map(|open| {
        let since = when(t, open.since, now);
        let expires = when(t, open.expires_at, now);
        let mut args = vec![
            ("name", Arg::Text(&view.name)),
            ("since", Arg::Text(&since)),
            ("expires", Arg::Text(&expires)),
        ];
        let reason = open.reported.map(|reason| reported(t, reason));
        match &reason {
            Some(reason) => {
                args.push(("reported", Arg::Text(reason)));
                t.text("app-contact-incoming-reported", &args)
            }
            None => t.text("app-contact-incoming", &args),
        }
    });
    json!({
        "name": view.name,
        "paired": paired,
        "reports_allowed": view.reports_allowed,
        "report_delay_minutes": view.report_delay_minutes,
        "incoming": incoming,
    })
}

/// Runs `work` on a context outside the async threads (the state file is
/// locked and synced), if there is a state file: 409 without one, 400 with
/// the text of a failure.
async fn with_device<T: Send + 'static>(
    shared: &Arc<Shared>,
    work: impl FnOnce(&mut Ctx<Http>) -> lz_client::ctx::Result<T> + Send + 'static,
) -> Result<T, Box<Answer>> {
    if !shared.state.exists() {
        return Err(Box::new(error(
            shared,
            StatusCode::CONFLICT,
            "server-error-no-device",
        )));
    }
    blocking(shared, move |shared| work(&mut shared.quiet_ctx())).await
}

/// Runs blocking work on the state file through `spawn_blocking`; a
/// failure becomes 400 with its text.
async fn blocking<T: Send + 'static>(
    shared: &Arc<Shared>,
    work: impl FnOnce(&Shared) -> lz_client::ctx::Result<T> + Send + 'static,
) -> Result<T, Box<Answer>> {
    let worker = Arc::clone(shared);
    match tokio::task::spawn_blocking(move || work(&worker)).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(fail)) => Err(Box::new(error_text(StatusCode::BAD_REQUEST, &fail.0))),
        Err(e) => Err(Box::new(error_text(
            StatusCode::INTERNAL_SERVER_ERROR,
            &shared.text_with("server-error-internal", "error", &e.to_string()),
        ))),
    }
}

/// Reads a JSON body of at most [`MAX_BODY`] bytes.
async fn read_json<T: DeserializeOwned>(
    request: Request<Incoming>,
    shared: &Shared,
) -> Result<T, Box<Answer>> {
    let too_large = || {
        Box::new(error(
            shared,
            StatusCode::PAYLOAD_TOO_LARGE,
            "server-error-body-large",
        ))
    };
    if request.body().size_hint().lower() > MAX_BODY as u64 {
        return Err(too_large());
    }
    let body = match Limited::new(request.into_body(), MAX_BODY).collect().await {
        Ok(body) => body.to_bytes(),
        Err(e) if e.downcast_ref::<LengthLimitError>().is_some() => return Err(too_large()),
        Err(e) => return Err(bad_body(shared, &e.to_string())),
    };
    serde_json::from_slice(&body).map_err(|e| bad_body(shared, &e.to_string()))
}

fn bad_body(shared: &Shared, detail: &str) -> Box<Answer> {
    Box::new(error_text(
        StatusCode::BAD_REQUEST,
        &shared.text_with("server-error-body", "error", detail),
    ))
}

fn empty(status: StatusCode) -> Answer {
    let mut answer = Response::new(Full::default());
    *answer.status_mut() = status;
    answer
}

fn json_answer(status: StatusCode, value: &Value) -> Answer {
    let mut answer = Response::new(Full::new(Bytes::from(value.to_string())));
    *answer.status_mut() = status;
    answer.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    answer
}

/// `{"error": text}`.
fn error_text(status: StatusCode, text: &str) -> Answer {
    json_answer(status, &json!({ "error": text }))
}

/// `{"error": …}` with the text `id` in the language of the page.
fn error(shared: &Shared, status: StatusCode, id: &str) -> Answer {
    error_text(status, &shared.text(id))
}

fn not_allowed(shared: &Shared, allow: &str) -> Answer {
    let mut answer = error(
        shared,
        StatusCode::METHOD_NOT_ALLOWED,
        "server-error-method",
    );
    if let Ok(allow) = HeaderValue::from_str(allow) {
        answer.headers_mut().insert(header::ALLOW, allow);
    }
    answer
}
