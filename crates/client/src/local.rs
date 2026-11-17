//! The local interface: external programs report a state ("asleep" for
//! eight hours) to this device (`docs/m8-local-interface.md`).
//!
//! It speaks minimal HTTP/1.1, so `curl --unix-socket` works from scripts:
//!
//! | Call | Body | Answer |
//! |---|---|---|
//! | `POST /state` | `{"reason":"asleep","minutes":480}` | 204; 400 with `{"error":…}` |
//! | `DELETE /state` | – | 204 |
//! | `GET /state` | – | `{"reason":…,"until":…}` or 404 |
//!
//! Only the state in the state file changes here. Sending reported answers
//! is up to the caller. On Unix the interface listens on a socket that only
//! its owner may use; the HTTP part works on any connection, so a Windows
//! named pipe can reuse it.

use std::convert::Infallible;
use std::io;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::{Body, Bytes, Incoming};
use hyper::header::{self, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use lz_core::report::Invalid;
use lz_core::store::{Device, JsonFile, Store};
use lz_core::wire::{Reason, TOKEN_LEN, Token};
use serde::Deserialize;
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::ctx::Clock;

/// Largest request body in bytes.
const MAX_BODY: usize = 4096;

/// Most bytes of a request head; hyper does not accept less than 8 KiB.
const MAX_HEAD: usize = 16 * 1024;

/// How long a client may take to send the request head.
const HEADER_TIMEOUT: Duration = Duration::from_secs(5);

/// How long one connection may last, request and answer together.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

/// Most connections served at the same time; further ones are closed.
#[cfg(unix)]
const MAX_CONNECTIONS: usize = 16;

/// The only path of the interface.
const STATE_PATH: &str = "/state";

/// Methods allowed on [`STATE_PATH`], for the `Allow` header.
const ALLOWED: &str = "GET, POST, DELETE";

/// Configuration of the local interface.
pub struct Local {
    /// The state file whose reported state the interface changes.
    pub store: JsonFile,
    /// Source of the current time.
    pub clock: Clock,
    /// If set, every request needs `Authorization: Bearer <token in hex>`.
    pub token: Option<Token>,
}

/// Body of `POST /state`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetState {
    reason: String,
    minutes: u32,
}

/// Carries the error of the state file or of the state out of
/// [`Store::update`], which needs a conversion from [`io::Error`].
enum Failed {
    Io(io::Error),
    Invalid(Invalid),
}

impl From<io::Error> for Failed {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// The answer type of the interface.
type Answer = Response<Full<Bytes>>;

/// Default socket path: `$XDG_RUNTIME_DIR/lebenszeichen/<name>.sock`.
/// `None` if `XDG_RUNTIME_DIR` is unset or not an absolute path.
#[cfg(unix)]
pub fn default_socket_path(name: &str) -> Option<PathBuf> {
    let runtime = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    if !runtime.is_absolute() {
        return None;
    }
    Some(runtime.join("lebenszeichen").join(format!("{name}.sock")))
}

/// A bound socket, ready for [`serve`].
///
/// The socket file stays when the listener is dropped; the next [`bind`]
/// replaces it once nobody serves it any more.
#[cfg(unix)]
pub struct LocalListener {
    inner: std::os::unix::net::UnixListener,
    path: PathBuf,
}

#[cfg(unix)]
impl LocalListener {
    /// Path of the socket.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Binds the socket at `path`, readable and writable only by its owner.
///
/// Creates the parent directory with mode 0700 if it is missing, and
/// refuses one that others may use. A socket left behind by an instance
/// that has ended is replaced; a socket that is still served gives
/// [`io::ErrorKind::AddrInUse`]. Anything else at the path is left alone and
/// gives an error. Needs no runtime; [`serve`] does.
#[cfg(unix)]
pub fn bind(path: &Path) -> io::Result<LocalListener> {
    use std::fs::{self, DirBuilder, Permissions};
    use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};

    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !dir.exists() {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let meta = fs::metadata(dir)?;
    // Someone else's directory, even with mode 0700, lets its owner in.
    // On Linux `/proc/self` belongs to the user of this process.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != fs::metadata("/proc/self")?.uid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "the socket directory {} belongs to another user",
                    dir.display()
                ),
            ));
        }
    }
    let mode = meta.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "the socket directory {} is open to other users (mode {:o}); \
                 allow only its owner (chmod 700) or choose another path",
                dir.display(),
                mode & 0o777
            ),
        ));
    }

    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => match UnixStream::connect(path) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("another instance is serving the socket {}", path.display()),
                ));
            }
            // Nobody listens: left behind by an instance that has ended.
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                match fs::remove_file(path) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                    _ => {}
                }
            }
            Err(error) => return Err(error),
        },
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{} exists and is not a socket; not touching it",
                    path.display()
                ),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    // Between bind and chmod the socket has the mode the umask gives; the
    // directory checked above keeps other users out in the meantime.
    let inner = UnixListener::bind(path)?;
    fs::set_permissions(path, Permissions::from_mode(0o600))?;
    inner.set_nonblocking(true)?;
    Ok(LocalListener {
        inner,
        path: path.to_owned(),
    })
}

/// Serves the socket until the future is dropped, each connection in its
/// own task. Fails only if the listener cannot join the tokio runtime.
#[cfg(unix)]
pub async fn serve(listener: LocalListener, local: Local) -> io::Result<()> {
    let listener = tokio::net::UnixListener::from_std(listener.inner)?;
    let local = Arc::new(local);
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(_) => {
                // For example too many open files: wait instead of spinning.
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        // Beyond the limit the connection is closed right away.
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            continue;
        };
        let local = Arc::clone(&local);
        tokio::spawn(async move {
            serve_connection(stream, local).await;
            drop(permit);
        });
    }
}

/// Serves one request on one connection, whatever its transport.
///
/// The connection is closed after the answer, or after
/// [`CONNECTION_TIMEOUT`] at the latest.
pub async fn serve_connection<S>(io: S, local: Arc<Local>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let service = service_fn(move |request| {
        let local = Arc::clone(&local);
        async move { Ok::<_, Infallible>(handle(request, local).await) }
    });
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_TIMEOUT)
        .keep_alive(false)
        .max_buf_size(MAX_HEAD);
    let connection = builder.serve_connection(TokioIo::new(io), service);
    // Errors of single connections (a client that hangs up, a bad request
    // head) concern only that client; hyper has answered what it could.
    let _ = tokio::time::timeout(CONNECTION_TIMEOUT, connection).await;
}

/// Answers one request.
async fn handle(request: Request<Incoming>, local: Arc<Local>) -> Answer {
    if request.uri().path() != STATE_PATH {
        return error(StatusCode::NOT_FOUND, "not found");
    }
    if let Some(token) = &local.token
        && !authorized(&request, token)
    {
        return error(StatusCode::UNAUTHORIZED, "token required");
    }
    match *request.method() {
        Method::GET => get_state(local).await,
        Method::POST => set_state(request, local).await,
        Method::DELETE => {
            let result = blocking(local, |local| {
                local.store.update(|device: &mut Device| {
                    device.clear_state();
                    Ok::<_, io::Error>(())
                })
            })
            .await;
            match result {
                Ok(()) => empty(StatusCode::NO_CONTENT),
                Err(answer) => *answer,
            }
        }
        _ => {
            let mut answer = error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
            answer
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static(ALLOWED));
            answer
        }
    }
}

/// `GET /state`.
async fn get_state(local: Arc<Local>) -> Answer {
    let result = blocking(local, |local| {
        let now = local.clock.now();
        let device = local.store.load()?;
        Ok(device.current_state(now).copied())
    })
    .await;
    match result {
        Ok(Some(state)) => json_answer(
            StatusCode::OK,
            &json!({ "reason": state.reason.as_str(), "until": state.until }),
        ),
        Ok(None) => error(StatusCode::NOT_FOUND, "no state"),
        Err(answer) => *answer,
    }
}

/// `POST /state`.
async fn set_state(request: Request<Incoming>, local: Arc<Local>) -> Answer {
    // The Content-Length, if any, is the lower bound of the size hint.
    if request.body().size_hint().lower() > MAX_BODY as u64 {
        return too_large();
    }
    let body = match Limited::new(request.into_body(), MAX_BODY).collect().await {
        Ok(body) => body.to_bytes(),
        Err(e) if e.downcast_ref::<LengthLimitError>().is_some() => return too_large(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "could not read the body"),
    };
    let set: SetState = match serde_json::from_slice(&body) {
        Ok(set) => set,
        Err(e) => return error(StatusCode::BAD_REQUEST, &format!("invalid body: {e}")),
    };
    let Ok(reason) = Reason::new(set.reason.trim()) else {
        return error(StatusCode::BAD_REQUEST, "the reason is too long");
    };
    let result = blocking(local, move |local| {
        let now = local.clock.now();
        Ok(local.store.update(|device: &mut Device| {
            device
                .set_state(reason, set.minutes, now)
                .map_err(Failed::Invalid)
        }))
    })
    .await;
    match result {
        Ok(Ok(())) => empty(StatusCode::NO_CONTENT),
        Ok(Err(Failed::Invalid(invalid))) => error(StatusCode::BAD_REQUEST, &invalid.to_string()),
        Ok(Err(Failed::Io(e))) => state_file_error(&e),
        Err(answer) => *answer,
    }
}

/// Runs blocking work on the state file outside the async threads.
///
/// [`JsonFile`] reads, writes and syncs files and waits for a file lock
/// that another process (the app, `lz`) may hold for a while, so it runs
/// through `spawn_blocking` rather than inline, where it would stall other
/// connections and the caller's tasks. Errors of the state file become the
/// answer 500 (boxed: a response is large).
async fn blocking<T: Send + 'static>(
    local: Arc<Local>,
    work: impl FnOnce(&Local) -> io::Result<T> + Send + 'static,
) -> Result<T, Box<Answer>> {
    match tokio::task::spawn_blocking(move || work(&local)).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(Box::new(state_file_error(&e))),
        Err(_) => Err(Box::new(error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal error",
        ))),
    }
}

/// Whether the request carries `Authorization: Bearer <token>`, the token
/// in lower-case hex. Missing and wrong tokens are treated alike.
fn authorized(request: &Request<Incoming>, token: &Token) -> bool {
    let Some(given) = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(decode_token)
    else {
        return false;
    };
    given.ct_eq(token).into()
}

/// Decodes a token written as exactly 64 lower-case hex digits.
fn decode_token(text: &str) -> Option<Token> {
    fn digit(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        }
    }
    let text = text.as_bytes();
    if text.len() != 2 * TOKEN_LEN {
        return None;
    }
    let mut out = [0; TOKEN_LEN];
    for (byte, [high, low]) in out.iter_mut().zip(text.as_chunks::<2>().0) {
        *byte = digit(*high)? << 4 | digit(*low)?;
    }
    Some(out)
}

/// An answer without a body.
fn empty(status: StatusCode) -> Answer {
    let mut answer = Response::new(Full::default());
    *answer.status_mut() = status;
    answer
}

/// An answer with a JSON body.
fn json_answer(status: StatusCode, value: &serde_json::Value) -> Answer {
    let mut answer = Response::new(Full::new(Bytes::from(value.to_string())));
    *answer.status_mut() = status;
    answer.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    answer
}

/// An error answer: `{"error": text}`.
fn error(status: StatusCode, text: &str) -> Answer {
    json_answer(status, &json!({ "error": text }))
}

/// 413 for a body above [`MAX_BODY`].
fn too_large() -> Answer {
    error(StatusCode::PAYLOAD_TOO_LARGE, "the body is too large")
}

/// 500 for an error of the state file.
fn state_file_error(e: &io::Error) -> Answer {
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        &format!("state file: {e}"),
    )
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs::{self, Permissions};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener as StdListener;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    use lz_core::report::MAX_STATE_MINUTES;
    use lz_core::store::{Device, JsonFile, Store};
    use lz_core::wire::{MAX_REASON_LEN, Token};
    use rand_core::OsRng;
    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    use super::{Local, bind, default_socket_path, serve};
    use crate::ctx::Clock;

    const NOW: u64 = 1_800_000_000;

    /// A served socket in a temporary directory.
    struct Server {
        _dir: tempfile::TempDir,
        socket: PathBuf,
        store: JsonFile,
        task: tokio::task::JoinHandle<std::io::Result<()>>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn start(token: Option<Token>) -> Server {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonFile::new(dir.path().join("device.json"));
        store.create(&Device::new(&mut OsRng)).unwrap();
        let socket = dir.path().join("run").join("lz.sock");
        let listener = bind(&socket).unwrap();
        let local = Local {
            store: store.clone(),
            clock: Clock::Manual(Arc::new(AtomicU64::new(NOW))),
            token,
        };
        let task = tokio::spawn(serve(listener, local));
        Server {
            _dir: dir,
            socket,
            store,
            task,
        }
    }

    /// Sends a raw request and returns the status and the body as JSON
    /// (`Null` if empty).
    async fn raw(socket: &Path, request: &[u8]) -> (u16, Value, String) {
        let mut stream = UnixStream::connect(socket).await.unwrap();
        stream.write_all(request).await.unwrap();
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).await.unwrap();
        let answer = String::from_utf8(answer).unwrap();
        let (head, body) = answer.split_once("\r\n\r\n").unwrap();
        let status = head[9..12].parse().unwrap();
        let body = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(body).unwrap()
        };
        (status, body, head.to_owned())
    }

    async fn call(
        socket: &Path,
        method: &str,
        path: &str,
        headers: &str,
        body: &str,
    ) -> (u16, Value, String) {
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{headers}\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        raw(socket, request.as_bytes()).await
    }

    async fn post(socket: &Path, body: &str) -> (u16, Value) {
        let (status, body, _) = call(socket, "POST", "/state", "", body).await;
        (status, body)
    }

    async fn get(socket: &Path) -> (u16, Value) {
        let (status, body, _) = call(socket, "GET", "/state", "", "").await;
        (status, body)
    }

    #[tokio::test]
    async fn set_get_and_clear() {
        let server = start(None);
        let s = &server.socket;
        assert_eq!(get(s).await.0, 404);
        assert_eq!(
            post(s, r#"{"reason": "  asleep ", "minutes": 480}"#).await,
            (204, Value::Null)
        );
        let (status, body) = get(s).await;
        assert_eq!(status, 200);
        assert_eq!(body["reason"], "asleep");
        assert_eq!(body["until"], NOW + 480 * 60);
        let stored = server.store.load().unwrap();
        assert_eq!(stored.current_state(NOW).unwrap().until, NOW + 480 * 60);

        let (status, _, _) = call(s, "DELETE", "/state", "", "").await;
        assert_eq!(status, 204);
        assert_eq!(
            get(s).await,
            (404, serde_json::json!({"error": "no state"}))
        );
    }

    #[tokio::test]
    async fn invalid_states_are_rejected() {
        let server = start(None);
        let s = &server.socket;
        let long = "x".repeat(MAX_REASON_LEN + 1);
        let cases = [
            (
                r#"{"reason": "  ", "minutes": 10}"#.to_owned(),
                Some("the reason is empty"),
            ),
            (r#"{"reason": "asleep", "minutes": 0}"#.to_owned(), None),
            (
                format!(
                    r#"{{"reason": "asleep", "minutes": {}}}"#,
                    MAX_STATE_MINUTES + 1
                ),
                None,
            ),
            (
                format!(r#"{{"reason": "{long}", "minutes": 10}}"#),
                Some("the reason is too long"),
            ),
            (
                r#"{"reason": "asleep", "minutes": 10, "extra": 1}"#.to_owned(),
                None,
            ),
            (r#"{"reason": "asleep", "minutes": -1}"#.to_owned(), None),
            ("not json".to_owned(), None),
        ];
        for (body, text) in cases {
            let (status, answer) = post(s, &body).await;
            assert_eq!(status, 400, "{body}");
            let error = answer["error"].as_str().unwrap();
            if let Some(text) = text {
                assert_eq!(error, text);
            }
        }
        let (_, answer) = post(s, r#"{"reason": "a", "minutes": 0}"#).await;
        assert!(answer["error"].as_str().unwrap().contains("1 to 10080"));
        assert_eq!(get(s).await.0, 404);
    }

    #[tokio::test]
    async fn large_bodies_are_rejected() {
        let server = start(None);
        let s = &server.socket;
        let big = format!(r#"{{"reason": "{}", "minutes": 10}}"#, " ".repeat(5000));
        assert_eq!(post(s, &big).await.0, 413);

        // Without Content-Length, the limit applies while reading.
        let request = format!(
            "POST /state HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
             Transfer-Encoding: chunked\r\n\r\n{:x}\r\n{big}\r\n0\r\n\r\n",
            big.len()
        );
        assert_eq!(raw(s, request.as_bytes()).await.0, 413);
        assert_eq!(get(s).await.0, 404);
    }

    #[tokio::test]
    async fn wrong_method_and_path() {
        let server = start(None);
        let s = &server.socket;
        let (status, body, head) = call(s, "PUT", "/state", "", "").await;
        assert_eq!(status, 405);
        assert!(body["error"].is_string());
        assert!(
            head.to_ascii_lowercase()
                .contains("allow: get, post, delete")
        );
        assert_eq!(call(s, "GET", "/other", "", "").await.0, 404);
        assert_eq!(call(s, "GET", "/", "", "").await.0, 404);
    }

    #[tokio::test]
    async fn token_is_required_when_set() {
        let token = [0xa7; 32];
        let server = start(Some(token));
        let s = &server.socket;
        let body = r#"{"reason": "asleep", "minutes": 10}"#;
        let required = serde_json::json!({"error": "token required"});
        let right = format!("Authorization: Bearer {}\r\n", "a7".repeat(32));
        let wrong = [
            String::new(),
            format!("Authorization: Bearer {}\r\n", "a8".repeat(32)),
            format!("Authorization: Bearer {}\r\n", "A7".repeat(32)),
            format!("Authorization: Bearer {}\r\n", "a7".repeat(31)),
            format!("Authorization: {}\r\n", "a7".repeat(32)),
        ];
        for headers in &wrong {
            let (status, answer, _) = call(s, "POST", "/state", headers, body).await;
            assert_eq!((status, answer), (401, required.clone()), "{headers}");
            assert_eq!(call(s, "GET", "/state", headers, "").await.0, 401);
        }
        assert_eq!(call(s, "POST", "/state", &right, body).await.0, 204);
        assert_eq!(call(s, "GET", "/state", &right, "").await.0, 200);
    }

    #[tokio::test]
    async fn without_token_the_header_is_ignored() {
        let server = start(None);
        let headers = format!("Authorization: Bearer {}\r\n", "00".repeat(32));
        let body = r#"{"reason": "asleep", "minutes": 10}"#;
        let (status, _, _) = call(&server.socket, "POST", "/state", &headers, body).await;
        assert_eq!(status, 204);
    }

    #[tokio::test]
    async fn state_file_errors_give_500() {
        let server = start(None);
        fs::write(server.store.path(), "garbage").unwrap();
        let (status, body) = get(&server.socket).await;
        assert_eq!(status, 500);
        assert!(body["error"].as_str().unwrap().starts_with("state file: "));
    }

    /// A directory only its owner may use; temporary directories may be
    /// open to others.
    fn private_dir(dir: &tempfile::TempDir) -> PathBuf {
        let run = dir.path().join("run");
        fs::create_dir(&run).unwrap();
        fs::set_permissions(&run, Permissions::from_mode(0o700)).unwrap();
        run
    }

    #[test]
    fn bind_sets_modes() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("a").join("b").join("lz.sock");
        let _listener = bind(&socket).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&socket), 0o600);
        assert_eq!(mode(socket.parent().unwrap()), 0o700);
    }

    #[test]
    fn bind_refuses_an_open_directory() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        fs::create_dir(&run).unwrap();
        fs::set_permissions(&run, Permissions::from_mode(0o755)).unwrap();
        let error = bind(&run.join("lz.sock")).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("755"));
        assert_eq!(
            fs::metadata(&run).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn bind_replaces_a_stale_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = private_dir(&dir).join("lz.sock");
        drop(StdListener::bind(&socket).unwrap());
        assert!(socket.exists());
        let _listener = bind(&socket).unwrap();
        assert!(std::os::unix::net::UnixStream::connect(&socket).is_ok());
    }

    #[test]
    fn bind_refuses_a_live_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = private_dir(&dir).join("lz.sock");
        let _first = bind(&socket).unwrap();
        let error = bind(&socket).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
    }

    #[test]
    fn bind_keeps_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = private_dir(&dir).join("lz.sock");
        fs::write(&path, "keep me").unwrap();
        let error = bind(&path).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&path).unwrap(), "keep me");
    }

    #[test]
    fn default_path_follows_the_runtime_dir() {
        // Reads the real environment; only checks the shape.
        if let Some(path) = default_socket_path("lz") {
            assert!(path.is_absolute());
            assert!(path.ends_with("lebenszeichen/lz.sock"));
        }
    }
}
