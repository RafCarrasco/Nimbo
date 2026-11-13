//! Portable std-only HTTP/1.1 receiver for the browser-login callback (CP2 S2-core, plan §5).
//! One non-blocking acceptor per pending login on `127.0.0.1:<random>`, at most
//! `max_workers_global` blocking connection workers process-wide, strict RFC 9112 head parsing,
//! WHATWG form decoding, a constant-time state compare and fixed responses that never echo the
//! request. Fail closed: anything unexpected gets a fixed error or a silent close. The 200 page
//! means only "the receiver answered"; it is NOT Firebase sign-in success.
use crate::login::{CallbackDecision, Credential, LoginFlow, Slot};
use std::io::{self, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

/// Receiver limits. Release code uses `Limits::RELEASE` only; smaller values exist for tests
/// and the debug-only stdio example (`LoginFlow::set_limits`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_workers_global: usize,
    pub max_acceptors_global: usize,
    pub connection_deadline_ms: u64,
    pub write_timeout_ms: u64,
    pub request_line_bytes: usize,
    pub header_bytes: usize,
    pub header_fields: usize,
    pub body_bytes: usize,
}
impl Limits {
    pub const RELEASE: Limits = Limits {
        max_workers_global: 4,
        max_acceptors_global: 4,
        connection_deadline_ms: 10_000,
        write_timeout_ms: 5_000,
        request_line_bytes: 2048,
        header_bytes: 8192,
        header_fields: 32,
        body_bytes: 65_536,
    };
}

/// Electron `LOGIN_DONE_PAGE` (desktop/main.cjs:63-67), byte-identical (pinned by tests).
pub const LOGIN_DONE_PAGE: &str = concat!(
    "<!doctype html><html lang=\"pt-BR\"><head><meta charset=\"utf-8\"><title>Nimbo</title>\n",
    "<style>body{margin:0;min-height:100vh;display:grid;place-items:center;background:#0b1020;color:#eef1ff;\n",
    "font-family:Inter,Segoe UI,system-ui,sans-serif}div{text-align:center}h1{font-size:22px;margin:0 0 8px}\n",
    "p{color:#9aa6c4;margin:0}</style></head><body><div><h1>Pronto, você entrou.</h1>\n",
    "<p>Pode fechar esta aba e voltar ao app Nimbo.</p></div></body></html>",
);
const INVALID_LINK: &str = "Link de login inválido.";

// ---------- best-effort owned-buffer wiping ----------

thread_local! {
    static WIPED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Exclusively owned bytes zeroed on drop. BEST EFFORT ONLY: copies made elsewhere
/// (reallocation leftovers, serialized JSON, HSTRING, WebView2, the browser) are not covered
/// and no memory-secrecy guarantee is claimed.
pub struct SecretBuf(Vec<u8>);
pub(crate) fn wipe(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: `b` is a valid, aligned, exclusive `&mut u8` into a live owned buffer.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
}
impl SecretBuf {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    /// In-crate writers append in place (never through a reallocating copy when pre-sized).
    pub(crate) fn vec_mut(&mut self) -> &mut Vec<u8> {
        &mut self.0
    }
    /// Wipes in place and returns the (now zeroed) buffer: test observation of `wipe`.
    #[doc(hidden)]
    pub fn into_inner_after_zero(mut self) -> Vec<u8> {
        let mut bytes = std::mem::take(&mut self.0);
        wipe(&mut bytes);
        bytes
    }
}
impl Drop for SecretBuf {
    fn drop(&mut self) {
        wipe(&mut self.0);
        // Test accounting: bytes observed zero AFTER the wipe (a drop that skipped the wipe of
        // non-zero bytes is visible).
        let zero = self.0.iter().filter(|&&b| b == 0).count();
        WIPED.with(|w| w.set(w.get() + zero));
    }
}
impl std::fmt::Debug for SecretBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretBuf({} bytes)", self.0.len())
    }
}
/// Bytes observed zero after `SecretBuf::drop`'s wipe on the calling thread (test accounting).
#[doc(hidden)]
pub fn wiped_bytes_on_this_thread() -> usize {
    WIPED.with(|w| w.get())
}

// ---------- pure parsing ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    NotFound,
    BadRequest,
    HeaderLimit,
    LengthRequired,
    BodyTooLarge,
    Truncated,
    StateRejected,
    OriginRejected,
    Malformed,
}
impl Reject {
    pub fn code(self) -> &'static str {
        match self {
            Reject::NotFound => "callback-not-found",
            Reject::BadRequest => "callback-bad-request",
            Reject::HeaderLimit => "callback-header-limit",
            Reject::LengthRequired => "callback-length-required",
            Reject::BodyTooLarge => "callback-body-too-large",
            Reject::Truncated => "callback-truncated",
            Reject::StateRejected => "callback-state-rejected",
            Reject::OriginRejected => "callback-origin-rejected",
            Reject::Malformed => "callback-malformed",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub content_length: usize,
    /// `None` absent; several Origin fields are reduced to an empty (rejected) value.
    pub origin: Option<Vec<u8>>,
}

#[derive(Debug)]
pub enum HeadParse {
    /// No complete head yet and every limit still holds: read more.
    Incomplete,
    /// Decision plus the head length (body bytes start there).
    Done(Result<Request, Reject>, usize),
}

fn strip_cr(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}
fn trim_ows(mut v: &[u8]) -> &[u8] {
    while let [b' ' | b'\t', rest @ ..] = v {
        v = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = v {
        v = rest;
    }
    v
}
fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Request head (RFC 9112 §2-§6, plan §5.2 steps 1-5) as a pure function over the bytes
/// received so far. CRLF or bare LF end a line; any other CR is a 400.
pub fn parse_head(buf: &[u8], port: u16, limits: &Limits) -> HeadParse {
    let Some(line_end) = buf.iter().position(|&b| b == b'\n') else {
        return if buf.len() > limits.request_line_bytes + 1 {
            HeadParse::Done(Err(Reject::BadRequest), buf.len())
        } else {
            HeadParse::Incomplete
        };
    };
    let line = strip_cr(&buf[..line_end]);
    if line.len() > limits.request_line_bytes {
        return HeadParse::Done(Err(Reject::BadRequest), line_end + 1);
    }
    let mut pos = line_end + 1;
    let mut fields = Vec::new();
    let mut total = 0usize;
    loop {
        let Some(off) = buf[pos..].iter().position(|&b| b == b'\n') else {
            return if buf.len() - (line_end + 1) > limits.header_bytes + 2 {
                HeadParse::Done(Err(Reject::HeaderLimit), buf.len())
            } else {
                HeadParse::Incomplete
            };
        };
        let raw = &buf[pos..pos + off];
        pos += off + 1;
        let field = strip_cr(raw);
        if field.is_empty() {
            break;
        }
        total += raw.len() + 1;
        fields.push(field);
        if total > limits.header_bytes || fields.len() > limits.header_fields {
            return HeadParse::Done(Err(Reject::HeaderLimit), pos);
        }
    }
    HeadParse::Done(evaluate(line, &fields, port, limits), pos)
}

fn evaluate(line: &[u8], fields: &[&[u8]], port: u16, limits: &Limits) -> Result<Request, Reject> {
    use Reject::*;
    if line.contains(&b'\r') {
        return Err(BadRequest);
    }
    let parts: Vec<&[u8]> = line.split(|&b| b == b' ').collect();
    let [method, target, version] = parts.as_slice() else {
        return Err(BadRequest);
    };
    if *version != b"HTTP/1.1" || method.is_empty() || !method.iter().all(|&b| is_tchar(b)) {
        return Err(BadRequest);
    }
    let (mut hosts, mut lengths, mut origins, mut te) = (Vec::new(), Vec::new(), Vec::new(), false);
    for field in fields {
        if matches!(field.first(), Some(b' ' | b'\t')) {
            return Err(BadRequest); // obs-fold
        }
        let colon = field.iter().position(|&b| b == b':').ok_or(BadRequest)?;
        let name = &field[..colon];
        if name.is_empty() || !name.iter().all(|&b| is_tchar(b)) {
            return Err(BadRequest); // includes whitespace before the colon
        }
        let value = trim_ows(&field[colon + 1..]);
        if value.iter().any(|&b| (b < 0x20 && b != b'\t') || b == 0x7f) {
            return Err(BadRequest);
        }
        match name.to_ascii_lowercase().as_slice() {
            b"host" => hosts.push(value),
            b"content-length" => lengths.push(value),
            b"origin" => origins.push(value),
            b"transfer-encoding" => te = true,
            _ => {}
        }
    }
    if *method != b"POST" || *target != b"/callback" {
        return Err(NotFound);
    }
    let [host] = hosts.as_slice() else {
        return Err(BadRequest);
    };
    if *host != format!("127.0.0.1:{port}").as_bytes() {
        return Err(NotFound);
    }
    if te {
        return Err(BadRequest);
    }
    if lengths.is_empty() {
        return Err(LengthRequired);
    }
    let mut length: Option<u128> = None;
    for part in lengths.iter().flat_map(|v| v.split(|&b| b == b',')) {
        let digits = trim_ows(part);
        let valid = !digits.is_empty()
            && digits.len() <= 20
            && digits.iter().all(u8::is_ascii_digit)
            && !(digits.len() > 1 && digits[0] == b'0');
        if !valid {
            return Err(BadRequest);
        }
        let n = digits
            .iter()
            .fold(0u128, |n, d| n * 10 + u128::from(d - b'0'));
        if length.is_some_and(|l| l != n) {
            return Err(BadRequest);
        }
        length = Some(n);
    }
    let length = length.ok_or(BadRequest)?;
    if length > limits.body_bytes as u128 {
        return Err(BodyTooLarge);
    }
    let origin = match origins.as_slice() {
        [] => None,
        [one] => Some(one.to_vec()),
        _ => Some(Vec::new()),
    };
    Ok(Request {
        content_length: length as usize,
        origin,
    })
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}
/// WHATWG application/x-www-form-urlencoded byte decoding: `+` is a space, `%XY` a byte, any
/// other `%` stays literal. Never longer than the input.
fn decode(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    out
}
/// First value for `name` (URLSearchParams.get: first occurrence wins), decoded.
pub fn form_get(body: &[u8], name: &[u8]) -> Option<Vec<u8>> {
    body.split(|&b| b == b'&')
        .filter(|seq| !seq.is_empty())
        .find_map(|seq| {
            let (n, v) = match seq.iter().position(|&b| b == b'=') {
                Some(eq) => (&seq[..eq], &seq[eq + 1..]),
                None => (seq, &seq[seq.len()..]),
            };
            (decode(n) == name).then(|| decode(v))
        })
}

fn token_ok(t: &[u8]) -> bool {
    std::str::from_utf8(t).is_ok() && !t.iter().any(|&b| b < 0x20 || b == 0x7f)
}

/// Plan §5.2 steps 7-10, in order: state (constant time), tokens, Origin.
pub fn check_form(
    body: &[u8],
    expected_state: &[u8; 48],
    origin: Option<&[u8]>,
    app_origin: &str,
) -> Result<Credential, Reject> {
    let state = SecretBuf::new(form_get(body, b"state").unwrap_or_default());
    // The length (48) is public; the content compare is full-length constant time.
    let same: bool =
        state.as_bytes().len() == 48 && bool::from(state.as_bytes().ct_eq(expected_state));
    if !same {
        return Err(Reject::StateRejected);
    }
    let id_token = SecretBuf::new(form_get(body, b"idToken").unwrap_or_default());
    let access_token = SecretBuf::new(form_get(body, b"accessToken").unwrap_or_default());
    if !token_ok(id_token.as_bytes()) || !token_ok(access_token.as_bytes()) {
        return Err(Reject::Malformed);
    }
    match origin {
        None => {}
        Some(o) if o == b"null" || o == app_origin.as_bytes() => {}
        Some(_) => return Err(Reject::OriginRejected),
    }
    Ok(Credential::new(id_token, access_token))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Response {
    Done,
    Reject(Reject),
}
/// Fixed response bytes; empty means "close without a response".
pub fn response_bytes(r: Response) -> Vec<u8> {
    let (status, content_type, body) = match r {
        Response::Done => ("200 OK", Some("text/html; charset=utf-8"), LOGIN_DONE_PAGE),
        Response::Reject(Reject::StateRejected | Reject::OriginRejected) => (
            "403 Forbidden",
            Some("text/plain; charset=utf-8"),
            INVALID_LINK,
        ),
        Response::Reject(Reject::NotFound) => ("404 Not Found", None, ""),
        Response::Reject(Reject::BadRequest | Reject::Malformed) => ("400 Bad Request", None, ""),
        Response::Reject(Reject::LengthRequired) => ("411 Length Required", None, ""),
        Response::Reject(Reject::HeaderLimit) => ("431 Request Header Fields Too Large", None, ""),
        Response::Reject(Reject::BodyTooLarge | Reject::Truncated) => return Vec::new(),
    };
    let content_type = content_type
        .map(|t| format!("Content-Type: {t}\r\n"))
        .unwrap_or_default();
    format!(
        "HTTP/1.1 {status}\r\n{content_type}Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

// ---------- threads (no dispatcher, no WebView2: only the flow, registry, codes, notify) ----------

/// Everything an acceptor owns. Dropping it releases the listener (port) and the acceptor slot.
pub(crate) struct Acceptor {
    pub login_id: u64,
    pub expected: [u8; 48],
    pub app_origin: Arc<str>,
    pub listener: TcpListener,
    pub port: u16,
    pub stop: Arc<AtomicBool>,
    pub flow: LoginFlow,
    pub limits: Limits,
    pub slot: Slot,
}

/// Non-blocking accept + 20 ms sleep + stop flag. Exit latency is "<= 20 ms + scheduler",
/// observed by bounded polling in tests, never joined from the UI thread.
pub(crate) fn run_acceptor(a: Acceptor) {
    loop {
        if a.stop.load(Ordering::SeqCst) || a.flow.is_shut_down() {
            break;
        }
        match a.listener.accept() {
            Ok((stream, _)) => accept_one(&a, stream),
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::Interrupted
                        | ErrorKind::ConnectionAborted
                        | ErrorKind::ConnectionReset
                ) => {}
            Err(e) => {
                a.flow.server_failed(a.login_id, &e.to_string());
                break;
            }
        }
    }
    drop(a.slot);
}

fn accept_one(a: &Acceptor, stream: TcpStream) {
    let Some(slot) = a.flow.reserve_worker() else {
        a.flow.push_code("callback-too-many-connections");
        let _ = stream.shutdown(Shutdown::Both);
        return; // dropped at once, no bytes
    };
    let worker = Worker {
        login_id: a.login_id,
        expected: a.expected,
        app_origin: a.app_origin.clone(),
        port: a.port,
        stop: a.stop.clone(),
        flow: a.flow.clone(),
        limits: a.limits,
        _slot: slot,
    };
    if a.flow.spawn(Box::new(move || worker.run(stream))).is_err() {
        a.flow.push_code("callback-spawn-failed"); // closure dropped: stream and slot released
    }
}

struct Worker {
    login_id: u64,
    expected: [u8; 48],
    app_origin: Arc<str>,
    port: u16,
    stop: Arc<AtomicBool>,
    flow: LoginFlow,
    limits: Limits,
    _slot: Slot,
}

fn read_until(stream: &mut TcpStream, buf: &mut [u8], deadline: Instant) -> io::Result<usize> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(ErrorKind::TimedOut.into());
    }
    stream.set_read_timeout(Some(left))?;
    stream.read(buf)
}

impl Worker {
    fn run(self, mut stream: TcpStream) {
        // Windows sockets accepted from a non-blocking listener inherit non-blocking mode.
        if stream.set_nonblocking(false).is_err() {
            return;
        }
        let Ok(clone) = stream.try_clone() else {
            return;
        };
        // Registration re-checks stop/shutdown under the registry lock: a worker accepted
        // before a cancel can never register after the registry was drained.
        let Some(_registration) = self.flow.register(self.login_id, clone, &self.stop) else {
            let _ = stream.shutdown(Shutdown::Both);
            return;
        };
        let deadline = Instant::now() + Duration::from_millis(self.limits.connection_deadline_ms);
        let (response, decision) = self.handle(&mut stream, deadline);
        let bytes = response_bytes(response);
        if !bytes.is_empty() {
            let wrote = stream
                .set_write_timeout(Some(Duration::from_millis(
                    self.limits.write_timeout_ms.max(1),
                )))
                .and_then(|()| stream.write_all(&bytes));
            if wrote.is_err() {
                self.flow.push_code("callback-response-timeout");
            }
        }
        let _ = stream.shutdown(Shutdown::Write);
        if matches!(
            decision,
            Some(CallbackDecision::Deliver | CallbackDecision::ExpiredAtCallback)
        ) {
            self.flow.notify();
        }
    }

    fn handle(
        &self,
        stream: &mut TcpStream,
        deadline: Instant,
    ) -> (Response, Option<CallbackDecision>) {
        let silent = (Response::Reject(Reject::Truncated), None);
        let mut head = SecretBuf::new(Vec::with_capacity(4096));
        let mut chunk = [0u8; 2048];
        let (request, head_len) = loop {
            match parse_head(head.as_bytes(), self.port, &self.limits) {
                HeadParse::Incomplete => match read_until(stream, &mut chunk, deadline) {
                    Ok(n) if n > 0 => head.0.extend_from_slice(&chunk[..n]),
                    _ => {
                        self.flow.push_code("callback-truncated");
                        wipe(&mut chunk);
                        return silent;
                    }
                },
                HeadParse::Done(Err(r), _) => {
                    self.flow.push_code(r.code());
                    return (Response::Reject(r), None);
                }
                HeadParse::Done(Ok(request), head_len) => break (request, head_len),
            }
        };
        let length = request.content_length;
        let mut body = SecretBuf::new(Vec::with_capacity(length));
        let early = &head.as_bytes()[head_len..];
        body.0.extend_from_slice(&early[..early.len().min(length)]);
        drop(head);
        while body.0.len() < length {
            let want = (length - body.0.len()).min(chunk.len());
            match read_until(stream, &mut chunk[..want], deadline) {
                Ok(n) if n > 0 => body.0.extend_from_slice(&chunk[..n]),
                _ => {
                    self.flow.push_code("callback-truncated");
                    wipe(&mut chunk);
                    return silent;
                }
            }
        }
        wipe(&mut chunk);
        let credential = match check_form(
            body.as_bytes(),
            &self.expected,
            request.origin.as_deref(),
            &self.app_origin,
        ) {
            Ok(c) => c,
            Err(r) => {
                self.flow.push_code(r.code());
                return (Response::Reject(r), None);
            }
        };
        drop(body);
        let decision = self
            .flow
            .callback(self.login_id, credential, self.flow.now());
        self.flow.push_code(match decision {
            CallbackDecision::Deliver => "login-credential-delivered",
            CallbackDecision::IgnoreNotPending => "login-ignored-not-pending",
            CallbackDecision::ExpiredAtCallback => "login-expired-at-callback",
        });
        (Response::Done, Some(decision))
    }
}
