//! CP2 S2-core loopback receiver (L5b): the request parser as a pure function over bytes plus
//! REAL blocking sockets on 127.0.0.1 with small `Limits`. Protocol/budget evidence only; no
//! browser, emulator or non-loopback socket. LOGIN NOT DELIVERED until S2-integration.
use nimbo_desktop::login::*;
use nimbo_desktop::loopback::*;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

const APP: &str = "http://127.0.0.1:4173";

struct Live {
    flow: LoginFlow,
    port: u16,
    state: String,
    notified: Arc<AtomicUsize>,
    clock: Arc<AtomicU64>,
}

fn flow_with(limits: Limits) -> (LoginFlow, Arc<AtomicUsize>, Arc<AtomicU64>) {
    let notified = Arc::new(AtomicUsize::new(0));
    let n = notified.clone();
    let (flow, clock) = flow_notifying(
        limits,
        Arc::new(move || {
            n.fetch_add(1, Ordering::SeqCst);
            true
        }),
    );
    (flow, notified, clock)
}
fn flow_notifying(limits: Limits, notify: Notify) -> (LoginFlow, Arc<AtomicU64>) {
    let clock = Arc::new(AtomicU64::new(5));
    let c = clock.clone();
    let seed = Arc::new(AtomicU64::new(0x1234_5678));
    let flow = LoginFlow::new(Hooks::new(
        Arc::new(move |buf: &mut [u8; 24]| {
            let mut x = seed.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::SeqCst);
            for b in buf.iter_mut() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *b = x as u8;
            }
            true
        }),
        notify,
        Arc::new(move || c.load(Ordering::SeqCst)),
    ));
    flow.set_limits(limits);
    (flow, clock)
}
fn small() -> Limits {
    Limits {
        connection_deadline_ms: 600,
        write_timeout_ms: 600,
        ..Limits::RELEASE
    }
}
fn begin_on(flow: &LoginFlow, id: u32) -> (u16, String) {
    let url = flow
        .begin(Caller { generation: 1, id }, APP, 5)
        .started
        .expect("begin");
    let text = url.as_str();
    let state = text.rsplit_once("state=").unwrap().1.to_owned();
    (url.port(), state)
}
fn live(limits: Limits) -> Live {
    let (flow, notified, clock) = flow_with(limits);
    let (port, state) = begin_on(&flow, 1);
    Live {
        flow,
        port,
        state,
        notified,
        clock,
    }
}

/// Waits until a worker socket is in the registry, so a following cancel/replacement exercises
/// the REGISTRY close path (not the register-after-stop guard).
fn wait_registered(flow: &LoginFlow) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while flow.stats().registered == 0 {
        assert!(Instant::now() < deadline, "worker registered");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Sends raw bytes and reads until EOF/reset (bounded by 5 s). Returns (bytes, reset).
fn exchange(port: u16, raw: &[u8]) -> (Vec<u8>, bool) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let _ = s.write_all(raw);
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match s.read(&mut buf) {
            Ok(0) => return (out, false),
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                panic!("receiver did not close within 5 s")
            }
            Err(_) => return (out, true),
        }
    }
}
fn request(port: u16, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut raw = format!("POST /callback HTTP/1.1\r\n{headers}\r\n").into_bytes();
    raw.extend_from_slice(body);
    exchange(port, &raw).0
}
fn form(port: u16, body: &str) -> Vec<u8> {
    request(
        port,
        &format!(
            "Host: 127.0.0.1:{port}\r\nContent-Length: {}\r\n",
            body.len()
        ),
        body.as_bytes(),
    )
}
fn status(resp: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(resp).ok()?;
    text.strip_prefix("HTTP/1.1 ")?.get(..3)?.parse().ok()
}

// ---------- pure parser ----------

fn head(text: &str) -> HeadParse {
    parse_head(text.as_bytes(), 4242, &Limits::RELEASE)
}
fn reject(text: &str) -> Reject {
    match head(text) {
        HeadParse::Done(Err(r), _) => r,
        other => panic!("expected reject for {text:?}, got {other:?}"),
    }
}
fn accept(text: &str) -> Request {
    match head(text) {
        HeadParse::Done(Ok(r), _) => r,
        other => panic!("expected accept for {text:?}, got {other:?}"),
    }
}
const OK_HEAD: &str = "Host: 127.0.0.1:4242\r\nContent-Length: 5\r\n";

#[test]
fn release_limits_are_pinned() {
    assert_eq!(
        Limits::RELEASE,
        Limits {
            max_workers_global: 4,
            max_acceptors_global: 4,
            connection_deadline_ms: 10_000,
            write_timeout_ms: 5_000,
            request_line_bytes: 2048,
            header_bytes: 8192,
            header_fields: 32,
            body_bytes: 65_536,
        }
    );
}

#[test]
fn request_line_variants() {
    let line = |l: &str| format!("{l}\r\n{OK_HEAD}\r\n");
    assert_eq!(reject(&line("GET /callback HTTP/1.1")), Reject::NotFound);
    assert_eq!(reject(&line("POST /callbackx HTTP/1.1")), Reject::NotFound);
    assert_eq!(
        reject(&line("POST /callback?x=1 HTTP/1.1")),
        Reject::NotFound
    );
    assert_eq!(reject(&line("POST / HTTP/1.1")), Reject::NotFound);
    assert_eq!(reject(&line("POST /callback HTTP/1.0")), Reject::BadRequest);
    assert_eq!(
        reject(&line("POST  /callback HTTP/1.1")),
        Reject::BadRequest
    );
    assert_eq!(reject(&line("POST /callback")), Reject::BadRequest);
    let long = format!("POST /callback{} HTTP/1.1", "x".repeat(2048));
    assert_eq!(reject(&line(&long)), Reject::BadRequest);
    // Exactly 2048 bytes of request line is within the limit (and simply not found).
    let fit = format!("POST /{} HTTP/1.1", "y".repeat(2048 - 15));
    assert_eq!(fit.len(), 2048);
    assert_eq!(reject(&line(&fit)), Reject::NotFound);
    // Bare LF terminators are accepted; a bare CR is not.
    assert_eq!(
        accept(&format!(
            "POST /callback HTTP/1.1\n{}\n",
            OK_HEAD.replace("\r\n", "\n")
        ))
        .content_length,
        5
    );
    assert_eq!(
        reject(&format!("POST /callback\rHTTP/1.1\r\n{OK_HEAD}\r\n")),
        Reject::BadRequest
    );
    assert!(matches!(
        parse_head(
            b"POST /callback HTTP/1.1\r\nHost: 1",
            4242,
            &Limits::RELEASE
        ),
        HeadParse::Incomplete
    ));
}

#[test]
fn header_limits_and_grammar() {
    let with = |extra: &str| format!("POST /callback HTTP/1.1\r\n{OK_HEAD}{extra}\r\n");
    let fields: String = (0..30).map(|i| format!("X-{i}: v\r\n")).collect();
    accept(&with(&fields)); // 32 fields
    let fields: String = (0..31).map(|i| format!("X-{i}: v\r\n")).collect();
    assert_eq!(reject(&with(&fields)), Reject::HeaderLimit);
    let big = format!("X-Pad: {}\r\n", "p".repeat(8192));
    assert_eq!(reject(&with(&big)), Reject::HeaderLimit);
    assert_eq!(reject(&with("X-Bad : v\r\n")), Reject::BadRequest);
    assert_eq!(reject(&with("X-Fold: a\r\n b\r\n")), Reject::BadRequest);
    assert_eq!(reject(&with("X-Cr: a\rb\r\n")), Reject::BadRequest);
    assert_eq!(reject(&with("NoColon\r\n")), Reject::BadRequest);
    assert_eq!(reject(&with("X-Ctl: a\u{1}b\r\n")), Reject::BadRequest);
    // A head that never ends but already exceeds every limit is rejected before more reads.
    let endless = format!("POST /callback HTTP/1.1\r\nX: {}", "z".repeat(10_400));
    assert!(matches!(
        parse_head(endless.as_bytes(), 4242, &Limits::RELEASE),
        HeadParse::Done(Err(Reject::HeaderLimit), _)
    ));
}

#[test]
fn host_matrix() {
    let with = |hosts: &str| format!("POST /callback HTTP/1.1\r\n{hosts}Content-Length: 5\r\n\r\n");
    accept(&with("Host: 127.0.0.1:4242\r\n"));
    accept(&with("host:127.0.0.1:4242 \r\n"));
    assert_eq!(reject(&with("")), Reject::BadRequest);
    assert_eq!(
        reject(&with("Host: 127.0.0.1:4242\r\nHost: 127.0.0.1:4242\r\n")),
        Reject::BadRequest
    );
    for foreign in [
        "localhost:4242",
        "127.0.0.1:4243",
        "attacker.example:4242",
        "127.0.0.1",
    ] {
        assert_eq!(
            reject(&with(&format!("Host: {foreign}\r\n"))),
            Reject::NotFound,
            "{foreign}"
        );
    }
}

#[test]
fn transfer_encoding_and_content_length_matrix() {
    let with = |h: &str| format!("POST /callback HTTP/1.1\r\nHost: 127.0.0.1:4242\r\n{h}\r\n");
    for te in [
        "Transfer-Encoding: chunked\r\n",
        "Transfer-Encoding: identity\r\n",
        "Transfer-Encoding: chunked\r\nContent-Length: 5\r\n",
    ] {
        assert_eq!(reject(&with(te)), Reject::BadRequest, "{te}");
    }
    assert_eq!(reject(&with("")), Reject::LengthRequired);
    assert_eq!(
        reject(&with("Content-Length: 5\r\nContent-Length: 6\r\n")),
        Reject::BadRequest
    );
    assert_eq!(
        accept(&with("Content-Length: 5\r\nContent-Length: 5\r\n")).content_length,
        5
    );
    assert_eq!(accept(&with("Content-Length: 5, 5\r\n")).content_length, 5);
    assert_eq!(accept(&with("Content-Length: 0\r\n")).content_length, 0);
    for bad in ["5, 6", "+1", "1e3", "-1", "", "01", "1 1"] {
        assert_eq!(
            reject(&with(&format!("Content-Length: {bad}\r\n"))),
            Reject::BadRequest,
            "{bad}"
        );
    }
    let digits21 = "1".repeat(21);
    assert_eq!(
        reject(&with(&format!("Content-Length: {digits21}\r\n"))),
        Reject::BadRequest
    );
    assert_eq!(
        accept(&with("Content-Length: 65536\r\n")).content_length,
        65_536
    );
    assert_eq!(
        reject(&with("Content-Length: 65537\r\n")),
        Reject::BodyTooLarge
    );
    let digits20 = "9".repeat(20);
    assert_eq!(
        reject(&with(&format!("Content-Length: {digits20}\r\n"))),
        Reject::BodyTooLarge
    );
}

#[test]
fn form_decoding_is_whatwg_first_wins() {
    let get = |body: &str, name: &str| form_get(body.as_bytes(), name.as_bytes());
    assert_eq!(get("a=1&a=2", "a"), Some(b"1".to_vec()));
    assert_eq!(get("a=x+y%2B", "a"), Some(b"x y+".to_vec()));
    assert_eq!(get("a=%ZZ%4", "a"), Some(b"%ZZ%4".to_vec()));
    assert_eq!(get("a=%", "a"), Some(b"%".to_vec()));
    assert_eq!(get("&&a=1&&", "a"), Some(b"1".to_vec()));
    assert_eq!(get("a&b=2", "a"), Some(Vec::new()));
    assert_eq!(get("a=b=c", "a"), Some(b"b=c".to_vec()));
    assert_eq!(get("%61=1", "a"), Some(b"1".to_vec()));
    assert_eq!(get("b=1", "a"), None);
    assert_eq!(get("a=%e2%82%ac", "a"), Some("€".as_bytes().to_vec()));
}

#[test]
fn state_tokens_and_origin_checks_with_constant_time_compare() {
    let expected = *b"0123456789abcdef0123456789abcdef0123456789abcdef";
    let check = |body: &str, origin: Option<&str>| {
        check_form(body.as_bytes(), &expected, origin.map(str::as_bytes), APP)
    };
    let ok = "state=0123456789abcdef0123456789abcdef0123456789abcdef&idToken=i&accessToken=a";
    let c = check(ok, None).expect("valid");
    assert_eq!((c.id_token(), c.access_token()), ("i", "a"));
    let state = &ok[6..54];
    for (i, wrong) in [0usize, 24, 47].into_iter().map(|i| {
        let mut s = state.as_bytes().to_vec();
        s[i] = b'z';
        (i, String::from_utf8(s).unwrap())
    }) {
        let body = ok.replacen(state, &wrong, 1);
        assert_eq!(
            check(&body, None).unwrap_err(),
            Reject::StateRejected,
            "{i}"
        );
    }
    for body in [
        ok.replacen(state, &state.to_uppercase(), 1),
        ok.replacen(state, &state[..47], 1),
        ok.replacen(state, &format!("{state}0"), 1),
        "idToken=i".to_owned(),
        ok.replacen("state=", "state=%FF", 1),
    ] {
        assert_eq!(check(&body, None).unwrap_err(), Reject::StateRejected);
    }
    // Absent tokens default to empty (the flow turns an empty idToken into the Google text).
    let bare = check(&ok[..54], None).unwrap();
    assert_eq!((bare.id_token(), bare.access_token()), ("", ""));
    for bad in ["%00", "%1F", "%7F", "%FF", "%C3%28", "a%0Ab"] {
        let body = ok.replacen("idToken=i", &format!("idToken={bad}"), 1);
        assert_eq!(check(&body, None).unwrap_err(), Reject::Malformed, "{bad}");
        let body = ok.replacen("accessToken=a", &format!("accessToken={bad}"), 1);
        assert_eq!(check(&body, None).unwrap_err(), Reject::Malformed, "{bad}");
    }
    assert!(check(ok, Some("null")).is_ok());
    assert!(check(ok, Some(APP)).is_ok());
    for origin in [
        "https://evil.test",
        "http://127.0.0.1:4174",
        "http://localhost:4173",
        "",
    ] {
        assert_eq!(
            check(ok, Some(origin)).unwrap_err(),
            Reject::OriginRejected,
            "{origin}"
        );
    }
    // State is checked before tokens and Origin (the 403 never depends on token bytes).
    let both = ok
        .replacen(state, &state[..47], 1)
        .replacen("idToken=i", "idToken=%00", 1);
    assert_eq!(
        check(&both, Some("https://evil.test")).unwrap_err(),
        Reject::StateRejected
    );
}

#[test]
fn response_bytes_are_fixed_and_the_done_page_is_byte_identical_to_electron() {
    let main = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../main.cjs"))
        .expect("desktop/main.cjs");
    let start = main.find("const LOGIN_DONE_PAGE = `").unwrap() + 25;
    let end = start + main[start..].find('`').unwrap();
    assert_eq!(LOGIN_DONE_PAGE, &main[start..end]);
    let done = response_bytes(Response::Done);
    let text = String::from_utf8(done).unwrap();
    assert_eq!(
        text,
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{LOGIN_DONE_PAGE}",
            LOGIN_DONE_PAGE.len()
        )
    );
    let forbidden =
        String::from_utf8(response_bytes(Response::Reject(Reject::StateRejected))).unwrap();
    assert!(
        forbidden
            .starts_with("HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain; charset=utf-8\r\n")
    );
    assert!(forbidden.ends_with("\r\n\r\nLink de login inválido."));
    assert_eq!(
        String::from_utf8(response_bytes(Response::Reject(Reject::NotFound))).unwrap(),
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
    );
    for (r, line) in [
        (Reject::BadRequest, "HTTP/1.1 400 Bad Request\r\n"),
        (Reject::Malformed, "HTTP/1.1 400 Bad Request\r\n"),
        (Reject::LengthRequired, "HTTP/1.1 411 Length Required\r\n"),
        (
            Reject::HeaderLimit,
            "HTTP/1.1 431 Request Header Fields Too Large\r\n",
        ),
        (Reject::OriginRejected, "HTTP/1.1 403 Forbidden\r\n"),
    ] {
        assert!(
            String::from_utf8(response_bytes(Response::Reject(r)))
                .unwrap()
                .starts_with(line)
        );
    }
    assert!(response_bytes(Response::Reject(Reject::BodyTooLarge)).is_empty());
    assert!(response_bytes(Response::Reject(Reject::Truncated)).is_empty());
}

#[test]
fn secret_buf_wipes_owned_bytes_best_effort() {
    let buf = SecretBuf::new(b"synthetic-secret".to_vec());
    assert_eq!(buf.as_bytes(), b"synthetic-secret");
    assert!(!format!("{buf:?}").contains("synthetic"));
    assert_eq!(buf.into_inner_after_zero(), vec![0u8; 16]);
    // Drop itself runs the wipe (thread-local accounting, labeled best effort).
    let before = wiped_bytes_on_this_thread();
    drop(SecretBuf::new(vec![7u8; 37]));
    assert_eq!(wiped_bytes_on_this_thread() - before, 37);
}

// ---------- real sockets ----------

// ---------- notification observer (test-owned) ----------

/// What the test-owned notify hook does. `Gated` is a SYNTHETIC in-process barrier on the real
/// Linux loopback path (not native Windows evidence): the hook blocks (bounded by 2 s) until
/// the test thread has observed the counter once, so the notifier deterministically completes
/// AFTER the client's EOF and first observation. `Withheld` (no notification) and `Duplicate`
/// (two notifications) are negative controls.
#[derive(Clone, Copy, Debug)]
enum NotifyMode {
    Immediate,
    Gated,
    Withheld,
    Duplicate,
}

/// Counts completed notify-hook calls; `count()` is the scenarios' only read of the counter.
struct Notified {
    mode: NotifyMode,
    count: AtomicUsize,
    first_seen: Mutex<Option<usize>>,
    released: Mutex<bool>,
    wake: Condvar,
    gate_timed_out: AtomicBool,
}
impl Notified {
    fn new(mode: NotifyMode) -> Arc<Self> {
        Arc::new(Self {
            mode,
            count: AtomicUsize::new(0),
            first_seen: Mutex::new(None),
            released: Mutex::new(false),
            wake: Condvar::new(),
            gate_timed_out: AtomicBool::new(false),
        })
    }
    fn hook(&self) {
        match self.mode {
            NotifyMode::Immediate => {
                self.count.fetch_add(1, Ordering::SeqCst);
            }
            NotifyMode::Gated => {
                let released = self.released.lock().unwrap_or_else(PoisonError::into_inner);
                let (_released, wait) = self
                    .wake
                    .wait_timeout_while(released, Duration::from_secs(2), |r| !*r)
                    .unwrap_or_else(PoisonError::into_inner);
                if wait.timed_out() {
                    self.gate_timed_out.store(true, Ordering::SeqCst);
                }
                self.count.fetch_add(1, Ordering::SeqCst);
            }
            NotifyMode::Withheld => {}
            NotifyMode::Duplicate => {
                self.count.fetch_add(2, Ordering::SeqCst);
            }
        }
    }
    /// Reads the counter, records the first value seen, then releases a gated hook.
    fn count(&self) -> usize {
        let n = self.count.load(Ordering::SeqCst);
        self.first_seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(n);
        self.release();
        n
    }
    fn release(&self) {
        *self.released.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.wake.notify_all();
    }
}

/// Bounded wait (the existing 2 s supervision bound) for the ACTUAL notifier. EOF on the client
/// socket does not imply it ran: `Worker::run` half-closes the socket BEFORE `flow.notify()`.
fn wait_notified(n: &Notified, want: usize) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while n.count() < want {
        assert!(Instant::now() < deadline, "notifier did not run within 2 s");
        std::thread::sleep(Duration::from_millis(5));
    }
}
/// After `wait_idle` reported no worker left (each worker's slot is released only after its
/// `notify` returned), no later notification can arrive: the count is final.
fn assert_exactly_one_notification(n: &Notified) {
    assert_eq!(n.count(), 1, "exactly one notification");
}

struct Observed {
    flow: LoginFlow,
    port: u16,
    state: String,
    notified: Arc<Notified>,
}
fn observed(mode: NotifyMode) -> Observed {
    let notified = Notified::new(mode);
    let n = notified.clone();
    let (flow, _clock) = flow_notifying(
        small(),
        Arc::new(move || {
            n.hook();
            true
        }),
    );
    let (port, state) = begin_on(&flow, 1);
    Observed {
        flow,
        port,
        state,
        notified,
    }
}

fn valid_post_scenario(l: &Observed) {
    let body = format!(
        "state={}&idToken=synthetic-id&accessToken=synthetic-access",
        l.state
    );
    let resp = form(l.port, &body);
    let text = String::from_utf8(resp).unwrap();
    assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(text.ends_with(LOGIN_DONE_PAGE));
    assert!(
        !text.contains("synthetic") && !text.contains(&l.state),
        "no echo"
    );
    wait_notified(&l.notified, 1);
    assert!(l.flow.activation_reserved());
    let s = l.flow.take_settlements(5);
    assert_eq!(s.len(), 1, "exactly one settlement");
    let Outcome::Credential(c) = &s[0].outcome else {
        panic!()
    };
    assert_eq!(
        (c.id_token(), c.access_token()),
        ("synthetic-id", "synthetic-access")
    );
    // The listener stops after delivery: a second POST is refused within 2 s.
    let deadline = Instant::now() + Duration::from_secs(2);
    while TcpStream::connect(("127.0.0.1", l.port)).is_ok() {
        assert!(Instant::now() < deadline, "port still open");
        std::thread::sleep(Duration::from_millis(20));
    }
    let st = l.flow.wait_idle(Duration::from_secs(2));
    assert_eq!((st.workers, st.acceptors), (0, 0));
    assert_exactly_one_notification(&l.notified);
    let (codes, _) = l.flow.take_codes();
    assert!(codes.contains(&"login-credential-delivered"), "{codes:?}");
}

#[test]
fn valid_post_delivers_writes_done_page_and_notifies_then_listener_closes() {
    valid_post_scenario(&observed(NotifyMode::Immediate));
}

#[test]
fn rejections_over_real_sockets_keep_the_login_pending() {
    let l = live(small());
    let p = l.port;
    assert_eq!(
        status(&exchange(p, b"GET /callback HTTP/1.1\r\nHost: x\r\n\r\n").0),
        Some(404)
    );
    assert_eq!(status(&form(p, "state=0000&idToken=x")), Some(403));
    let resp = form(p, &format!("state={}&idToken=%00", l.state));
    assert_eq!(status(&resp), Some(400));
    let resp = request(
        p,
        &format!("Host: 127.0.0.1:{p}\r\nOrigin: https://evil.test\r\nContent-Length: 54\r\n"),
        format!("state={}", l.state).as_bytes(),
    );
    assert_eq!(status(&resp), Some(403));
    assert_eq!(
        status(&request(
            p,
            "Host: 127.0.0.1:1\r\nContent-Length: 1\r\n",
            b"x"
        )),
        Some(404)
    );
    assert_eq!(
        status(&request(p, &format!("Host: 127.0.0.1:{p}\r\n"), b"")),
        Some(411)
    );
    // CL > 65,536: closed without any response byte.
    let (bytes, _) = exchange(
        p,
        format!("POST /callback HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\nContent-Length: 65537\r\n\r\n")
            .as_bytes(),
    );
    assert!(bytes.is_empty());
    assert!(l.flow.is_active(), "no lockout, login still pending");
    assert_eq!(l.notified.load(Ordering::SeqCst), 0);
    let ok = form(p, &format!("state={}&idToken=t", l.state));
    assert_eq!(
        status(&ok),
        Some(200),
        "a correct POST later still succeeds"
    );
}

#[test]
fn truncated_body_and_idle_connection_close_without_response_after_the_real_deadline() {
    let l = live(small());
    let p = l.port;
    let started = Instant::now();
    let (bytes, _) = exchange(
        p,
        format!(
            "POST /callback HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\nContent-Length: 50\r\n\r\nstate="
        )
        .as_bytes(),
    );
    assert!(bytes.is_empty(), "truncated body: no response");
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(500) && took < Duration::from_secs(3),
        "{took:?}"
    );
    let started = Instant::now();
    let (bytes, _) = exchange(p, b"");
    assert!(bytes.is_empty(), "idle: no response");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(l.flow.is_active());
    // Virtual-clock login expiry is a separate concern (tests/login.rs); the real deadline did
    // not expire the login.
    l.clock.fetch_add(1, Ordering::SeqCst);
    assert!(l.flow.tick(l.clock.load(Ordering::SeqCst)).is_empty());
}

fn not_pending_scenario(l: &Observed) {
    // Parity main.cjs:103-104: the 200 page is written BEFORE the pending check.
    let mut late = TcpStream::connect(("127.0.0.1", l.port)).unwrap();
    late.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    late.write_all(b"POST /callback HTTP/1.1\r\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while l.flow.stats().workers == 0 {
        assert!(Instant::now() < deadline, "late connection accepted");
        std::thread::sleep(Duration::from_millis(5));
    }
    let body = format!("state={}&idToken=first", l.state);
    assert_eq!(status(&form(l.port, &body)), Some(200));
    let late_body = format!("state={}&idToken=second", l.state);
    late.write_all(
        format!(
            "Host: 127.0.0.1:{}\r\nContent-Length: {}\r\n\r\n{late_body}",
            l.port,
            late_body.len()
        )
        .as_bytes(),
    )
    .unwrap();
    let mut resp = Vec::new();
    let _ = late.read_to_end(&mut resp);
    let text = String::from_utf8(resp).unwrap();
    assert!(text.starts_with("HTTP/1.1 200 OK\r\n") && text.ends_with(LOGIN_DONE_PAGE));
    let s = l.flow.take_settlements(5);
    assert_eq!(s.len(), 1);
    let Outcome::Credential(c) = &s[0].outcome else {
        panic!()
    };
    assert_eq!(c.id_token(), "first", "exactly one Deliver");
    wait_notified(&l.notified, 1);
    let st = l.flow.wait_idle(Duration::from_secs(2));
    assert_eq!((st.workers, st.acceptors), (0, 0));
    assert_exactly_one_notification(&l.notified);
    let (codes, _) = l.flow.take_codes();
    assert!(codes.contains(&"login-ignored-not-pending"), "{codes:?}");
}

#[test]
fn not_pending_post_still_gets_the_done_page_and_delivers_nothing() {
    not_pending_scenario(&observed(NotifyMode::Immediate));
}

type Scenario = fn(&Observed);
const SCENARIOS: [(&str, Scenario); 2] = [
    ("valid_post", valid_post_scenario),
    ("not_pending", not_pending_scenario),
];

/// Runs `scenario` under `mode` and returns its panic message, if any. Whatever happened, the
/// gate is released and the flow shut down and drained (bounded) before returning.
fn run_controlled(mode: NotifyMode, scenario: Scenario) -> (Option<String>, Arc<Notified>) {
    let l = observed(mode);
    let outcome = catch_unwind(AssertUnwindSafe(|| scenario(&l)));
    l.notified.release();
    l.flow.shutdown();
    let st = l.flow.wait_idle(Duration::from_secs(2));
    assert_eq!((st.workers, st.acceptors), (0, 0), "control teardown");
    let message = outcome.err().map(|p| {
        p.downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|m| (*m).to_owned()))
            .unwrap_or_default()
    });
    (message, l.notified)
}

/// Runs EVERY scenario under `mode` and judges each with `verdict` (an error or `None`) before
/// asserting, so one scenario's failure never hides the other's.
fn control_all(mode: NotifyMode, verdict: fn(Option<String>, &Notified) -> Option<String>) {
    let failures: Vec<String> = SCENARIOS
        .into_iter()
        .filter_map(|(name, scenario)| {
            let (panic, n) = run_controlled(mode, scenario);
            verdict(panic, &n).map(|e| format!("{name}: {e}"))
        })
        .collect();
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Causal positive control: the client reads the whole done page to EOF while the notifier is
/// still held (first observed count 0), and the scenarios still pass once it completes.
#[test]
fn scenarios_pass_when_the_notifier_completes_only_after_eof() {
    control_all(NotifyMode::Gated, |panic, n| {
        let first = *n.first_seen.lock().unwrap_or_else(PoisonError::into_inner);
        let completed = n.count.load(Ordering::SeqCst);
        if let Some(p) = panic {
            Some(format!("scenario failed: {p}"))
        } else if first != Some(0) {
            Some(format!(
                "EOF not observed before the notifier ran: {first:?}"
            ))
        } else if n.gate_timed_out.load(Ordering::SeqCst) {
            Some("gate not released by the observation".to_owned())
        } else if completed != 1 {
            Some(format!("{completed} notifications"))
        } else {
            None
        }
    });
}

/// Negative control: a notification that never happens still fails both scenarios.
#[test]
fn scenarios_fail_when_the_notification_is_withheld() {
    control_all(NotifyMode::Withheld, |panic, n| match panic {
        None => Some("withheld notification accepted".to_owned()),
        Some(p) if !p.contains("notifier did not run within 2 s") => {
            Some(format!("wrong failure: {p}"))
        }
        Some(_) if n.count.load(Ordering::SeqCst) != 0 => Some("counter moved".to_owned()),
        Some(_) => None,
    });
}

/// Negative control: a duplicate notification still fails both scenarios.
#[test]
fn scenarios_fail_on_a_duplicate_notification() {
    control_all(NotifyMode::Duplicate, |panic, n| match panic {
        None => Some("duplicate notification accepted".to_owned()),
        Some(p) if !p.contains("exactly one notification") => Some(format!("wrong failure: {p}")),
        Some(_) if n.count.load(Ordering::SeqCst) != 2 => Some("counter not 2".to_owned()),
        Some(_) => None,
    });
}

#[test]
fn cancel_closes_a_registered_connection_promptly_without_response() {
    let (flow, notified, _) = flow_with(small());
    let (port, state) = begin_on(&flow, 1);
    // Keep a connection open across the cancel: the registry closes it promptly.
    let mut held = TcpStream::connect(("127.0.0.1", port)).unwrap();
    held.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let _ = held.write_all(b"POST /callback HTTP/1.1\r\n");
    wait_registered(&flow);
    let started = Instant::now();
    assert_eq!(flow.cancel(5).len(), 1);
    let mut buf = [0u8; 64];
    let n = held.read(&mut buf).unwrap_or(0);
    assert_eq!(n, 0, "closed by the registry, no response");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let _ = state;
    assert_eq!(notified.load(Ordering::SeqCst), 0);
    let st = flow.wait_idle(Duration::from_secs(2));
    assert_eq!((st.workers, st.acceptors), (0, 0));
}

#[test]
fn replacement_closes_a_blocked_worker_of_the_old_login_promptly() {
    let (flow, _, _) = flow_with(Limits::RELEASE); // REAL 10 s deadline
    let (old_port, _) = begin_on(&flow, 1);
    let mut blocked = TcpStream::connect(("127.0.0.1", old_port)).unwrap();
    blocked
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let _ = blocked.write_all(b"POST /callback HTTP/1.1\r\nHost: ");
    wait_registered(&flow);
    let started = Instant::now();
    let b = flow.begin(
        Caller {
            generation: 1,
            id: 2,
        },
        APP,
        5,
    );
    assert_eq!(b.settled.len(), 1);
    let mut buf = [0u8; 8];
    assert_eq!(blocked.read(&mut buf).unwrap_or(0), 0);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while flow.stats().workers != 0 {
        assert!(Instant::now() < deadline, "worker survived replacement");
        std::thread::sleep(Duration::from_millis(10));
    }
    flow.shutdown();
    let st = flow.wait_idle(Duration::from_secs(2));
    assert_eq!((st.workers, st.acceptors), (0, 0));
}

#[test]
fn fifth_connection_is_dropped_and_the_worker_cap_is_global_across_listeners() {
    let (flow, _, _) = flow_with(Limits::RELEASE);
    let (port_a, _) = begin_on(&flow, 1);
    let idle: Vec<TcpStream> = (0..4)
        .map(|_| {
            let s = TcpStream::connect(("127.0.0.1", port_a)).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            s
        })
        .collect();
    let deadline = Instant::now() + Duration::from_secs(2);
    while flow.stats().workers < 4 {
        assert!(Instant::now() < deadline, "{:?}", flow.stats());
        std::thread::sleep(Duration::from_millis(10));
    }
    let started = Instant::now();
    let (bytes, _) = exchange(port_a, b"POST /callback HTTP/1.1\r\n");
    assert!(bytes.is_empty() && started.elapsed() < Duration::from_secs(2));
    assert!(flow.stats().max_workers <= 4);
    let (codes, _) = flow.take_codes();
    assert!(
        codes.contains(&"callback-too-many-connections"),
        "{codes:?}"
    );
    drop(idle);
    flow.shutdown();
    let st = flow.wait_idle(Duration::from_secs(2));
    assert_eq!((st.workers, st.acceptors), (0, 0));
    assert!(st.max_workers <= 4);
}

#[test]
fn global_worker_cap_spans_two_live_listeners() {
    // Two listeners of ONE flow alive at once. A replaced login's listener exits within
    // ~20 ms, too briefly to hold, so listener B is a doc-hidden test hook: a second acceptor
    // of the same flow (own acceptor reservation, stale login id, never delivers).
    let (flow, _, _) = flow_with(Limits::RELEASE);
    let (port_a, _) = begin_on(&flow, 1);
    let extra = flow
        .spawn_test_listener(APP)
        .expect("second listener of the same flow");
    let idle: Vec<TcpStream> = (0..4)
        .map(|_| TcpStream::connect(("127.0.0.1", port_a)).unwrap())
        .collect();
    let deadline = Instant::now() + Duration::from_secs(2);
    while flow.stats().workers < 4 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let started = Instant::now();
    let (bytes, _) = exchange(extra, b"POST /callback HTTP/1.1\r\n");
    assert!(bytes.is_empty(), "listener B connection dropped at once");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(flow.stats().max_workers, 4);
    drop(idle);
    flow.shutdown();
    let st = flow.wait_idle(Duration::from_secs(2));
    assert_eq!((st.workers, st.acceptors), (0, 0));
}

#[test]
fn raw_literal_quote_body_unit_bound_is_accepted_intact() {
    // (a) DIRECT UNIT BOUND (not a browser body): 65,400 literal `"` bytes are valid
    // urlencoded data; prefix + 65,400 < 65,536.
    let l = live(small());
    let prefix = format!("state={}&idToken=", l.state);
    let mut body = prefix.into_bytes();
    body.extend(std::iter::repeat_n(b'"', 65_400));
    assert!(body.len() <= 65_536);
    let resp = request(
        l.port,
        &format!(
            "Host: 127.0.0.1:{}\r\nContent-Length: {}\r\n",
            l.port,
            body.len()
        ),
        &body,
    );
    assert_eq!(status(&resp), Some(200));
    let s = l.flow.take_settlements(5);
    let Outcome::Credential(c) = &s[0].outcome else {
        panic!()
    };
    assert_eq!(c.id_token().len(), 65_400);
    assert!(c.id_token().bytes().all(|b| b == b'"'));
}

#[test]
fn browser_encoded_body_bound_accepts_the_largest_and_closes_one_byte_more() {
    // (b) END-TO-END ENCODED BOUND: a browser sends `"` as `%22`, so at most
    // floor((65,536 - prefix) / 3) quotes fit; the remainder is padded with `a`.
    let l = live(small());
    let prefix = format!("state={}&idToken=", l.state);
    let quotes = (65_536 - prefix.len()) / 3;
    let pad = 65_536 - prefix.len() - quotes * 3;
    let body = format!("{prefix}{}{}", "%22".repeat(quotes), "a".repeat(pad));
    assert_eq!(body.len(), 65_536);
    let over = format!("{body}a");
    let (bytes, _) = exchange(
        l.port,
        format!(
            "POST /callback HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: {}\r\n\r\n{over}",
            l.port,
            over.len()
        )
        .as_bytes(),
    );
    assert!(bytes.is_empty(), "65,537 bytes: closed without response");
    assert!(l.flow.is_active());
    assert_eq!(status(&form(l.port, &body)), Some(200));
    let s = l.flow.take_settlements(5);
    let Outcome::Credential(c) = &s[0].outcome else {
        panic!()
    };
    assert_eq!(c.id_token().len(), quotes + pad);
}

#[test]
fn burst_of_begin_cancel_with_idle_clients_stays_within_global_budgets() {
    let (flow, _, _) = flow_with(Limits::RELEASE);
    let threads_before = os_threads();
    let mut held = Vec::new();
    let mut max_threads = 0;
    for i in 0..500u32 {
        let b = flow.begin(
            Caller {
                generation: 1,
                id: i + 1,
            },
            APP,
            5,
        );
        if let Ok(url) = &b.started
            && i % 125 == 0
        {
            for _ in 0..4 {
                if let Ok(s) = TcpStream::connect(("127.0.0.1", url.port())) {
                    held.push(s);
                }
            }
        }
        flow.cancel(5);
        let s = flow.stats();
        assert!(s.workers <= 4 && s.acceptors <= 4, "{s:?}");
        if i % 50 == 0 {
            max_threads = max_threads.max(os_threads().unwrap_or(0));
        }
    }
    let s = flow.stats();
    assert!(s.max_workers <= 4 && s.max_acceptors <= 4, "{s:?}");
    if let Some(before) = threads_before {
        assert!(
            max_threads <= before + 8 + 16,
            "threads {before} -> {max_threads}"
        );
    }
    flow.shutdown();
    let st = flow.wait_idle(Duration::from_secs(2));
    assert_eq!((st.workers, st.acceptors), (0, 0), "{st:?}");
    for s in &held {
        let _ = s.shutdown(Shutdown::Both);
    }
}

// ---------- never-reading peer with a tiny receive buffer (plan §8.2) ----------

/// Test-only client socket whose SO_RCVBUF is set to 1 BEFORE connect (the OS rounds it up; the
/// effective value is read back with getsockopt and returned). std has no such option and the
/// crate takes no new dependency, so this is raw FFI to the OS socket API that std already links.
#[cfg(target_os = "linux")]
fn tiny_rcvbuf_connect(port: u16) -> (TcpStream, i32) {
    use std::os::fd::FromRawFd;
    #[repr(C)]
    struct SockaddrIn {
        family: u16,
        port: u16,
        addr: u32,
        zero: [u8; 8],
    }
    unsafe extern "C" {
        fn socket(domain: i32, kind: i32, protocol: i32) -> i32;
        fn setsockopt(fd: i32, level: i32, name: i32, value: *const i32, len: u32) -> i32;
        fn getsockopt(fd: i32, level: i32, name: i32, value: *mut i32, len: *mut u32) -> i32;
        fn connect(fd: i32, addr: *const SockaddrIn, len: u32) -> i32;
        fn close(fd: i32) -> i32;
    }
    const AF_INET: i32 = 2;
    const SOCK_STREAM: i32 = 1;
    const SOL_SOCKET: i32 = 1;
    const SO_RCVBUF: i32 = 8;
    let addr = SockaddrIn {
        family: AF_INET as u16,
        port: port.to_be(),
        addr: u32::from_be_bytes([127, 0, 0, 1]).to_be(),
        zero: [0; 8],
    };
    let int_len = std::mem::size_of::<i32>() as u32;
    // SAFETY: plain socket syscalls on a descriptor this function owns; every pointer refers to
    // a live local of the declared size, and the descriptor is closed on every failure path.
    unsafe {
        let fd = socket(AF_INET, SOCK_STREAM, 0);
        assert!(fd >= 0, "socket() failed");
        let one: i32 = 1;
        let mut effective: i32 = 0;
        let mut len = int_len;
        let ok = setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &one, int_len) == 0
            && getsockopt(fd, SOL_SOCKET, SO_RCVBUF, &mut effective, &mut len) == 0
            && connect(fd, &addr, std::mem::size_of::<SockaddrIn>() as u32) == 0;
        if !ok {
            close(fd);
            panic!("tiny SO_RCVBUF client setup failed");
        }
        (TcpStream::from_raw_fd(fd), effective)
    }
}

/// Windows twin of the Linux helper (Winsock is already started by the receiver's std listener).
/// Compiled by the windows-msvc check/clippy gates only; its runtime is the native user gate.
#[cfg(windows)]
fn tiny_rcvbuf_connect(port: u16) -> (TcpStream, i32) {
    use std::os::windows::io::FromRawSocket;
    #[repr(C)]
    struct SockaddrIn {
        family: u16,
        port: u16,
        addr: u32,
        zero: [u8; 8],
    }
    #[link(name = "ws2_32")]
    unsafe extern "system" {
        fn socket(af: i32, kind: i32, protocol: i32) -> usize;
        fn setsockopt(s: usize, level: i32, name: i32, value: *const i32, len: i32) -> i32;
        fn getsockopt(s: usize, level: i32, name: i32, value: *mut i32, len: *mut i32) -> i32;
        fn connect(s: usize, addr: *const SockaddrIn, len: i32) -> i32;
        fn closesocket(s: usize) -> i32;
    }
    const AF_INET: i32 = 2;
    const SOCK_STREAM: i32 = 1;
    const SOL_SOCKET: i32 = 0xffff;
    const SO_RCVBUF: i32 = 0x1002;
    let addr = SockaddrIn {
        family: AF_INET as u16,
        port: port.to_be(),
        addr: u32::from_be_bytes([127, 0, 0, 1]).to_be(),
        zero: [0; 8],
    };
    let int_len = std::mem::size_of::<i32>() as i32;
    // SAFETY: see the Linux helper; the SOCKET is owned here and closed on every failure path.
    unsafe {
        let s = socket(AF_INET, SOCK_STREAM, 0);
        assert!(s != usize::MAX, "socket() failed");
        let one: i32 = 1;
        let mut effective: i32 = 0;
        let mut len = int_len;
        let ok = setsockopt(s, SOL_SOCKET, SO_RCVBUF, &one, int_len) == 0
            && getsockopt(s, SOL_SOCKET, SO_RCVBUF, &mut effective, &mut len) == 0
            && connect(s, &addr, std::mem::size_of::<SockaddrIn>() as i32) == 0;
        if !ok {
            closesocket(s);
            panic!("tiny SO_RCVBUF client setup failed");
        }
        (TcpStream::from_raw_socket(s as u64), effective)
    }
}

/// No silent skip: a target without a tiny-receive-buffer mechanism fails this required test.
#[cfg(not(any(target_os = "linux", windows)))]
fn tiny_rcvbuf_connect(_port: u16) -> (TcpStream, i32) {
    panic!("no tiny SO_RCVBUF client mechanism on this target: required §8.2 test cannot run")
}

/// Plan §8.2 "write timeout (peer never reads: socket with tiny receive buffer -> bounded)".
/// The peer sends a VALID callback and its application never reads while the receiver works.
/// Asserted: bounded credential delivery, exactly-once settlement, worker/acceptor teardown and
/// port closure within the real connection deadline, and no echo of state/tokens.
/// Coverage boundary (recorded, not asserted): the fixed responses are a few hundred bytes and
/// fit the kernel socket buffers, so native TCP pressure is NOT expected to reach the write
/// timeout branch; whether `callback-response-timeout` was emitted is printed as an observation.
/// The timeout/deadline algorithm itself is covered by the injected-writer unit tests in
/// src/loopback.rs, which are algorithm evidence only, not native socket-pressure proof.
#[test]
fn never_reading_peer_with_tiny_receive_buffer_is_bounded_and_delivers_exactly_once() {
    let limits = Limits {
        connection_deadline_ms: 2_000,
        write_timeout_ms: 300,
        ..Limits::RELEASE
    };
    let l = live(limits);
    let (mut peer, rcvbuf) = tiny_rcvbuf_connect(l.port);
    assert!(
        rcvbuf > 0 && rcvbuf <= 4608,
        "tiny receive buffer not observed: {rcvbuf}"
    );
    let body = format!(
        "state={}&idToken=nonreader-id&accessToken=nonreader-access",
        l.state
    );
    let raw = format!(
        "POST /callback HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: {}\r\n\r\n{body}",
        l.port,
        body.len()
    );
    let sent = Instant::now();
    peer.write_all(raw.as_bytes()).expect("send request");
    // From here on the peer's application reads NOTHING until the receiver has torn down.
    let bound = Duration::from_millis(limits.connection_deadline_ms);
    let mut settled = Vec::new();
    while settled.is_empty() {
        assert!(sent.elapsed() < bound, "no settlement within the deadline");
        settled = l.flow.take_settlements(5);
        std::thread::sleep(Duration::from_millis(5));
    }
    let delivered_ms = sent.elapsed().as_millis();
    assert_eq!(settled.len(), 1, "exactly one settlement");
    let Outcome::Credential(c) = &settled[0].outcome else {
        panic!("expected a credential")
    };
    assert_eq!(
        (c.id_token(), c.access_token()),
        ("nonreader-id", "nonreader-access")
    );
    let st = l.flow.wait_idle(bound.saturating_sub(sent.elapsed()));
    assert_eq!(
        (st.workers, st.acceptors, st.registered),
        (0, 0, 0),
        "{st:?}"
    );
    let idle_ms = sent.elapsed().as_millis();
    assert!(sent.elapsed() < bound, "worker not bounded: {idle_ms} ms");
    let refuse_by = Instant::now() + Duration::from_secs(2);
    while TcpStream::connect(("127.0.0.1", l.port)).is_ok() {
        assert!(Instant::now() < refuse_by, "port still open");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(l.flow.take_settlements(5).is_empty(), "settled twice");
    assert_eq!(l.notified.load(Ordering::SeqCst), 1);
    let (codes, _) = l.flow.take_codes();
    let delivered = codes
        .iter()
        .filter(|c| **c == "login-credential-delivered")
        .count();
    assert_eq!(delivered, 1, "{codes:?}");
    let timeout_code = codes.contains(&"callback-response-timeout");
    // Only now, after teardown, does the test read what reached the peer: a prefix of the exact
    // fixed Done response (the whole response when it fit the buffers), never an echo.
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut got = Vec::new();
    let _ = peer.read_to_end(&mut got);
    let fixed = response_bytes(Response::Done);
    assert!(
        !got.is_empty() && fixed.starts_with(&got),
        "not the fixed response"
    );
    let text = String::from_utf8_lossy(&got);
    assert!(
        !text.contains("nonreader") && !text.contains(&l.state),
        "no echo"
    );
    eprintln!(
        "NONREADER_OBSERVED rcvbuf_effective={rcvbuf} delivered_ms={delivered_ms} \
         idle_ms={idle_ms} response_bytes={} received_bytes={} full_response={} \
         callback_response_timeout_code={timeout_code}",
        fixed.len(),
        got.len(),
        got == fixed
    );
}

/// Linux-only sample of this test process's OS threads (other tests run in parallel, so the
/// bound here is loose; the strict per-process bound is asserted by the stdio L4 test).
fn os_threads() -> Option<usize> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix("Threads:"))
        .and_then(|v| v.trim().parse().ok())
}
