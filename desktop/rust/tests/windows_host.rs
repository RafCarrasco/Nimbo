#![cfg(all(windows, feature = "test-host"))]
//! Interactive Windows ONLY: launches the real debug `test-host` executable against a
//! 127.0.0.1 fixture. No production site, login, credentials or real ShellExecute (the
//! test-host build records external opens). Run serially:
//!   cargo +1.99.1 test --manifest-path desktop/rust/Cargo.toml --locked --features test-host \
//!     --test windows_host -- --ignored --nocapture --test-threads=1
//! with NIMBO_HOST_TEST_DIR set to an owned, empty scratch directory.
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    UI::WindowsAndMessaging::*,
};
use windows::core::BOOL;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;
const EXE: &str = env!("CARGO_BIN_EXE_nimbo-desktop");

/// Same title rule as the hosted page (src/lib/version.ts:6, src/main.tsx:11). First click:
/// getDisplayMedia (needs activation); second click: window.open; then a top-level navigation.
const PAGE: &str = r#"<!doctype html><meta charset=utf-8><title>fixture</title><body style="margin:0;height:100vh">
<script>
const v = /\bnimbo\/(\d+\.\d+\.\d+)/i.exec(navigator.userAgent)?.[1] || '';
if (v) document.title = 'Nimbo ' + v;
const report = (k, x) => fetch('/report?' + k + '=' + encodeURIComponent(x)).catch(() => {});
report('bridge', typeof window.nimboDesktop);
let clicks = 0;
document.addEventListener('click', () => {
  clicks++;
  if (clicks === 1) navigator.mediaDevices.getDisplayMedia({ video: true })
    .then(() => report('gdm', 'resolved'), (e) => report('gdm', e.name));
  if (clicks === 2) {
    report('popup', String(window.open('https://example.org/popup') === null));
    setTimeout(() => { location.href = 'https://example.org/'; }, 200);
  }
});
</script>"#;

const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; connect-src 'self'";

/// CP1-F3-autoplay: NO click. The top document (app origin) plays locally synthesized silent PCM
/// (blob:, no network media) and embeds the same page from `localhost` (a different origin than
/// the 127.0.0.1 app origin) as the foreign-iframe negative control. Results go to /report.
const AUTOPLAY_PAGE: &str = r#"<!doctype html><meta charset=utf-8><title>autoplay</title><body>
<script>
const report = (k, x) => fetch('/report?' + k + '=' + encodeURIComponent(x)).catch(() => {});
const role = location.pathname === '/frame' ? 'frame' : 'top';
const n = 4000, buf = new ArrayBuffer(44 + n * 2), d = new DataView(buf);
const tag = (o, t) => { for (let i = 0; i < t.length; i++) d.setUint8(o + i, t.charCodeAt(i)); };
tag(0, 'RIFF'); d.setUint32(4, 36 + n * 2, true); tag(8, 'WAVE'); tag(12, 'fmt ');
d.setUint32(16, 16, true); d.setUint16(20, 1, true); d.setUint16(22, 1, true);
d.setUint32(24, 8000, true); d.setUint32(28, 16000, true); d.setUint16(32, 2, true);
d.setUint16(34, 16, true); tag(36, 'data'); d.setUint32(40, n * 2, true);
report(role + '-activation', navigator.userActivation ? String(navigator.userActivation.hasBeenActive) : 'unsupported');
const audio = new Audio(URL.createObjectURL(new Blob([buf], { type: 'audio/wav' })));
audio.muted = false;
audio.play().then(() => report(role + '-play', 'played'), (e) => report(role + '-play', e.name));
if (role === 'top') {
  const f = document.createElement('iframe');
  f.src = 'http://localhost:' + location.port + '/frame';
  document.body.appendChild(f);
}
</script>"#;
const AUTOPLAY_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; media-src blob:; connect-src 'self'; frame-src http://localhost:*";

/// CP2 S1 bridge (Tester F2). The host only ever replies to the TOP-LEVEL document, so the
/// discriminating observer is the top page: it records every `result` id on its own
/// `chrome.webview` and knows which ids its bridge issued (1, 2, ...). After a real `ready` and
/// the first answer (which also yields the current `gen`), each iframe (same-origin `/frame`,
/// foreign `localhost`) is told to post a VALID, fresh, state-changing invoke: current gen,
/// distinctive high id, `nimbo:capture:audio [true]`, no hello. If the host attributed it to the
/// main frame it would answer the top page (an unissued result id) and advance `last_id`, so the
/// next bridge call (id 2) would be a dropped replay. The top page observes the same-origin
/// iframe directly and the foreign one through a bounded `postMessage` report. Positive control
/// (last, so legitimate ids stay ordered): the top page posts the same request with id
/// 4000000100 itself; the observer must see exactly that unissued id. Values are URL-encoded.
const BRIDGE_PAGE: &str = r#"<!doctype html><meta charset=utf-8><title>bridge</title><body>
<script>
const report = (k, x) => fetch('/report?' + k + '=' + encodeURIComponent(x)).catch(() => {});
const role = location.pathname !== '/frame' ? 'top' : location.hostname === 'localhost' ? 'foreign' : 'same';
const appOrigin = 'http://127.0.0.1:' + location.port, foreignOrigin = 'http://localhost:' + location.port;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const within = (p, ms) => Promise.race([p, sleep(ms).then(() => 'timeout')]);
const probe = (gen, id) => ({ v: 1, kind: 'invoke', id, gen, method: 'nimbo:capture:audio', args: [true] });
report(role + '-bridge', typeof window.nimboDesktop);
if (role === 'top') (async () => {
  const bridge = window.nimboDesktop;
  report('top-keys', Object.keys(bridge || {}).sort().join(','));
  if (!bridge || !window.chrome || !window.chrome.webview) return report('top-observer', 'no-bridge');
  const results = [];
  let gen = null, issued = 0;
  window.chrome.webview.addEventListener('message', (e) => {
    const m = e.data;
    if (!m || m.kind !== 'result') return;
    results.push(m.id);
    if (m.id === 1) gen = m.gen;
  });
  const unexpected = () => results.filter((id) => !(id >= 1 && id <= issued));
  const list = (ids) => ids.join(',') || 'none';
  const call = () => { issued++; return bridge.startAudio().then((r) => JSON.stringify(r), (e) => 'error ' + e.message); };
  report('top-start', await within(call(), 5000));
  report('top-gen', String(gen));
  if (gen === null) return;
  const frame = (src) => new Promise((resolve) => {
    const f = document.createElement('iframe');
    f.onload = () => resolve(f);
    f.src = src;
    document.body.appendChild(f);
  });
  const same = await within(frame('/frame'), 10000);
  const foreign = await within(frame(foreignOrigin + '/frame'), 10000);
  let samePost = 'timeout', foreignPost = 'timeout';
  if (same !== 'timeout') {
    same.contentWindow.postMessage({ probe: { gen, id: 4000000000 } }, appOrigin);
    for (let i = 0; i < 50 && samePost === 'timeout'; i++) {
      await sleep(100);
      samePost = same.contentWindow.nimboProbe || 'timeout';
    }
  }
  if (foreign !== 'timeout') {
    const answered = new Promise((resolve) => window.addEventListener('message', (e) => {
      if (e.origin === foreignOrigin && e.source === foreign.contentWindow && e.data && e.data.nimboProbe) resolve(String(e.data.nimboProbe));
    }));
    foreign.contentWindow.postMessage({ probe: { gen, id: 4000000001 } }, foreignOrigin);
    foreignPost = await within(answered, 5000);
  }
  report('same-post', samePost);
  report('foreign-post', foreignPost);
  await sleep(1500);
  report('top-unexpected', list(unexpected()));
  report('top-after', await within(call(), 5000));
  window.chrome.webview.postMessage(probe(gen, 4000000100));
  for (let i = 0; i < 50 && !unexpected().length; i++) await sleep(100);
  report('top-control', list(unexpected()));
  report('top-results', list(results));
})();
else window.addEventListener('message', (e) => {
  if (e.origin !== appOrigin || e.source !== window.parent || !e.data || !e.data.probe) return;
  let state = 'no-webview';
  if (window.chrome && window.chrome.webview) {
    window.chrome.webview.postMessage(probe(e.data.probe.gen, e.data.probe.id));
    state = 'sent';
  }
  window.nimboProbe = state;
  if (role === 'foreign') window.parent.postMessage({ nimboProbe: state }, appOrigin);
});
</script>"#;
const BRIDGE_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; connect-src 'self'; frame-src 'self' http://localhost:*";

/// Minimal percent-decoding for /report values written by `encodeURIComponent`.
fn decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let hex = raw
            .get(i + 1..i + 3)
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

struct Fixture {
    url: String,
    log: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Fixture {
    fn start() -> std::io::Result<Self> {
        Self::serve(PAGE, CSP)
    }
    /// Serves `page` at `/` (and `/frame`, used only by the autoplay page) with `csp`.
    fn serve(page: &'static str, csp: &'static str) -> std::io::Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let url = format!("http://127.0.0.1:{}/", listener.local_addr()?.port());
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));
        let (flag, sink) = (stop.clone(), log.clone());
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        let _ = socket.set_nonblocking(false);
                        let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));
                        let mut buf = [0u8; 8192];
                        let n = socket.read(&mut buf).unwrap_or(0);
                        let request = String::from_utf8_lossy(&buf[..n]).to_string();
                        let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
                        let ua = request
                            .lines()
                            .find_map(|l| l.strip_prefix("User-Agent: "))
                            .unwrap_or("")
                            .to_owned();
                        if let Ok(mut log) = sink.lock() {
                            log.push(format!("{path} UA={ua}"));
                        }
                        let (status, body) = if path == "/" || path == "/frame" {
                            ("200 OK", page)
                        } else if path.starts_with("/report?") {
                            ("204 No Content", "")
                        } else {
                            ("404 Not Found", "")
                        };
                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: text/html\r\nContent-Security-Policy: {csp}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = socket.write_all(response.as_bytes());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            url,
            log,
            stop,
            thread: Some(thread),
        })
    }
    fn requests(&self) -> Vec<String> {
        self.log.lock().map(|l| l.clone()).unwrap_or_default()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct App(Child);
impl Drop for App {
    fn drop(&mut self) {
        // Bounded teardown, also on a timeout/early `?`: the WM_CLOSE path is exercised in the
        // test body; this backstop kills the host AND its WebView2 child process tree.
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = Command::new("taskkill")
                .args(["/T", "/F", "/PID"])
                .arg(self.0.id().to_string())
                .status();
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// Removes a per-test scratch directory once the WebView2 processes released it (bounded).
fn remove_scratch(dir: &Path) {
    for _ in 0..50 {
        if std::fs::remove_dir_all(dir).is_ok() || !dir.exists() {
            println!("SCRATCH_REMOVED {}", dir.display());
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    println!(
        "SCRATCH_LEFT {} (delete after WebView2 exits)",
        dir.display()
    );
}

fn owned_scratch(name: &str) -> TestResult<PathBuf> {
    let root = std::env::var_os("NIMBO_HOST_TEST_DIR")
        .ok_or("set NIMBO_HOST_TEST_DIR to an owned empty scratch directory")?;
    let dir = PathBuf::from(root).join(format!("{name}-{}", std::process::id()));
    std::fs::create_dir(&dir)?; // fresh => cold WebView2 profile
    println!("ISOLATED_HOST_DIR {}", dir.display());
    Ok(dir)
}

const STARTUP_ERRORS: [&str; 3] = [
    "webview2-unavailable",
    "webview2-controller-failed",
    "fatal-dialog",
];

fn wait_until<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> TestResult<T> {
    let start = Instant::now();
    loop {
        if let Some(value) = probe() {
            return Ok(value);
        }
        if start.elapsed() > Duration::from_secs(20) {
            return Err(format!("timed out waiting for {what}").into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

struct Search {
    pid: u32,
    class: &'static str,
    found: Option<HWND>,
}
fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: bounded buffer.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..usize::try_from(n).unwrap_or(0)])
}
extern "system" fn by_pid(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the &mut Search passed to EnumWindows for the duration of the call.
    let search = unsafe { &mut *(data.0 as *mut Search) };
    let mut pid = 0;
    // SAFETY: plain query on an enumerated window.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == search.pid && class_of(hwnd) == search.class {
        search.found = Some(hwnd);
        return BOOL(0);
    }
    BOOL(1)
}
extern "system" fn by_class(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: as above.
    let search = unsafe { &mut *(data.0 as *mut Search) };
    if class_of(hwnd) == search.class {
        search.found = Some(hwnd);
        return BOOL(0);
    }
    BOOL(1)
}
fn main_window(pid: u32) -> Option<HWND> {
    let mut search = Search {
        pid,
        class: "Nimbo.MainWindow",
        found: None,
    };
    // SAFETY: callback only touches `search`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(by_pid), LPARAM(&mut search as *mut _ as isize)) };
    search.found
}
fn render_widget(main: HWND) -> Option<HWND> {
    let mut search = Search {
        pid: 0,
        class: "Chrome_RenderWidgetHostHWND",
        found: None,
    };
    // SAFETY: as above.
    let _ = unsafe {
        EnumChildWindows(
            Some(main),
            Some(by_class),
            LPARAM(&mut search as *mut _ as isize),
        )
    };
    search.found
}
extern "system" fn any_child(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the &mut Option<HWND> passed to EnumChildWindows for the call.
    unsafe { *(data.0 as *mut Option<HWND>) = Some(hwnd) };
    BOOL(0)
}
/// Any child of the host's main window. The host itself creates none; WebView2 adds one to
/// the parent before CreateCoreWebView2Controller returns, so a child marks the controller
/// creation phase (until `started` is logged).
fn host_child(main: HWND) -> Option<HWND> {
    let mut found = None;
    // SAFETY: callback only touches `found`, which outlives the call.
    let _ = unsafe {
        EnumChildWindows(
            Some(main),
            Some(any_child),
            LPARAM(&mut found as *mut _ as isize),
        )
    };
    found
}
fn title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: bounded buffer.
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..usize::try_from(n).unwrap_or(0)])
}
/// Real input messages to the renderer's own HWND (never a blind screen click into other apps).
fn click(widget: HWND) {
    let at = LPARAM((40 << 16) | 40);
    // SAFETY: posting standard mouse messages to the test host's renderer window.
    unsafe {
        let _ = PostMessageW(Some(widget), WM_LBUTTONDOWN, WPARAM(1), at);
        let _ = PostMessageW(Some(widget), WM_LBUTTONUP, WPARAM(0), at);
    }
}
fn events(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("events.log")).unwrap_or_default()
}
fn launch(fixture: &Fixture, dir: &Path) -> std::io::Result<Child> {
    Command::new(EXE)
        .arg("--test-host-url")
        .arg(&fixture.url)
        .arg("--test-host-dir")
        .arg(dir)
        .spawn()
}

#[test]
#[ignore = "interactive Windows WebView2 host test; run with --ignored --nocapture --test-threads=1"]
fn opens_local_fixture_and_enforces_policy() -> TestResult<()> {
    let root = std::env::var_os("NIMBO_HOST_TEST_DIR")
        .ok_or("set NIMBO_HOST_TEST_DIR to an owned empty scratch directory")?;
    let dir: PathBuf = PathBuf::from(root).join(format!("nimbo-host-{}", std::process::id()));
    std::fs::create_dir(&dir)?;
    println!("ISOLATED_HOST_DIR {}", dir.display());
    let fixture = Fixture::start()?;

    // The test-host binary refuses to start without its loopback/scratch arguments.
    let refused = Command::new(EXE).status()?;
    assert_eq!(
        refused.code(),
        Some(64),
        "test-host must not open anything by default"
    );
    let production = Command::new(EXE)
        .args([
            "--test-host-url",
            "https://golive-ad118.web.app/",
            "--test-host-dir",
        ])
        .arg(&dir)
        .status()?;
    assert_eq!(
        production.code(),
        Some(64),
        "test-host rejects non-loopback URLs"
    );

    let mut app = App(launch(&fixture, &dir)?);
    let pid = app.0.id();
    let main = wait_until("main window", || main_window(pid))?;
    let version = env!("CARGO_PKG_VERSION");
    let expected_title = format!("Nimbo {version}");
    wait_until("native title from document.title", || {
        (title(main) == expected_title).then_some(())
    })?;
    let page = fixture
        .requests()
        .into_iter()
        .find(|r| r.starts_with("/ "))
        .ok_or("fixture page not requested")?;
    assert!(page.contains(&format!("Nimbo/{version}")), "{page}");
    // CP2 S1: the app-origin top document now has the bridge v1 object.
    wait_until("bridge report", || {
        fixture
            .requests()
            .iter()
            .any(|r| r.starts_with("/report?bridge=object"))
            .then_some(())
    })?;

    let widget = wait_until("renderer HWND", || render_widget(main))?;
    click(widget);
    wait_until("getDisplayMedia rejection", || {
        fixture
            .requests()
            .iter()
            .find(|r| r.starts_with("/report?gdm="))
            .cloned()
    })
    .map(|r| assert!(!r.starts_with("/report?gdm=resolved"), "{r}"))?;
    assert!(
        events(&dir).contains("screen-capture-cancelled"),
        "ScreenCaptureStarting must be reached and cancelled natively"
    );

    click(widget);
    wait_until("popup and navigation recorded", || {
        let e = events(&dir);
        (e.contains("popup-denied https://example.org/popup")
            && e.contains("external https://example.org/popup")
            && e.contains("navigation-external")
            && e.contains("external https://example.org/\n"))
        .then_some(())
    })?;
    assert!(
        fixture
            .requests()
            .iter()
            .any(|r| r.starts_with("/report?popup=true"))
    );
    assert_eq!(
        title(main),
        expected_title,
        "page stayed after cancelled navigation"
    );

    // Single instance: a second launch focuses/restores the first and exits.
    // SAFETY: test-owned child window.
    unsafe {
        let _ = ShowWindow(main, SW_MINIMIZE);
    }
    wait_until("minimized", || {
        unsafe { IsIconic(main) }.as_bool().then_some(())
    })?;
    let mut second = launch(&fixture, &dir)?;
    let status = wait_until("second instance exit", || second.try_wait().ok().flatten())?;
    assert!(status.success(), "second instance must forward and exit 0");
    wait_until("restored", || {
        (!unsafe { IsIconic(main) }.as_bool()).then_some(())
    })?;
    assert!(events(&dir).contains("focus-request"));

    // Quit on close (main.cjs:730).
    // SAFETY: test-owned child window.
    unsafe {
        PostMessageW(Some(main), WM_CLOSE, WPARAM(0), LPARAM(0))?;
    }
    let exit = wait_until("app exit", || app.0.try_wait().ok().flatten())?;
    assert!(exit.success());
    println!("REAL_WINDOWS_HOST_POLICY_OK pid={pid}");
    Ok(())
}

/// Review F2: the user closes the visible main window while WebView2 environment/controller
/// creation is still pending (fresh, cold profile). Expected: quiet exit 0, no WebView2 error.
#[test]
#[ignore = "interactive Windows WebView2 host test; run with --ignored --nocapture --test-threads=1"]
fn close_during_startup_quits_quietly() -> TestResult<()> {
    let dir = owned_scratch("nimbo-close")?;
    let fixture = Fixture::start()?;
    let mut app = App(launch(&fixture, &dir)?);
    let pid = app.0.id();
    // Poll tightly: the window is shown before WebView2 creation starts.
    let start = Instant::now();
    let main = loop {
        if let Some(main) = main_window(pid) {
            break main;
        }
        if start.elapsed() > Duration::from_secs(20) {
            return Err("timed out waiting for main window".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // SAFETY: test-owned child window.
    unsafe { PostMessageW(Some(main), WM_CLOSE, WPARAM(0), LPARAM(0))? };
    let exit = wait_until("app exit", || app.0.try_wait().ok().flatten())?;
    let log = events(&dir);
    println!("EVENTS_LOG_BEGIN\n{log}EVENTS_LOG_END");
    assert_eq!(exit.code(), Some(0), "a user close is a normal quit");
    for error in STARTUP_ERRORS {
        assert!(!log.contains(error), "unexpected {error}");
    }
    assert!(
        log.contains("closed-during-startup"),
        "INCONCLUSIVE: WebView2 started before WM_CLOSE was handled; rerun with a fresh directory"
    );
    drop(app);
    drop(fixture);
    remove_scratch(&dir);
    println!("CLOSE_DURING_STARTUP_OK pid={pid}");
    Ok(())
}

/// Review R1: the user closes the window while WebView2 *controller* creation is in flight.
/// WebView2 then completes the creation with E_ABORT inside DestroyWindow, before the startup
/// pump sees WM_QUIT; the host must still settle it as a quiet close (exit 0, no dialog).
/// Targets the phase: WM_CLOSE is posted at the first sighting of a child window of the host's
/// main window (added by WebView2 during controller creation) while `started` is not logged.
/// Fails as INCONCLUSIVE, never passes, when the phase was not hit; rerun on a fresh directory.
#[test]
#[ignore = "interactive Windows WebView2 host test; run with --ignored --nocapture --test-threads=1"]
fn close_during_controller_creation_quits_quietly() -> TestResult<()> {
    let dir = owned_scratch("nimbo-close-controller")?;
    let fixture = Fixture::start()?;
    let mut app = App(launch(&fixture, &dir)?);
    let pid = app.0.id();
    let start = Instant::now();
    let timed_out = |what: &str| -> TestResult<()> {
        if start.elapsed() > Duration::from_secs(30) {
            return Err(format!("timed out waiting for {what}").into());
        }
        std::thread::sleep(Duration::from_millis(1)); // poll granularity only
        Ok(())
    };
    let main = loop {
        if let Some(main) = main_window(pid) {
            break main;
        }
        timed_out("main window")?;
    };
    loop {
        if host_child(main).is_some() {
            break;
        }
        if app.0.try_wait()?.is_some() {
            return Err("host exited before controller creation began".into());
        }
        timed_out("controller creation (child window)")?;
    }
    let started_before_close = events(&dir).contains("started\n");
    // SAFETY: test-owned child window.
    unsafe { PostMessageW(Some(main), WM_CLOSE, WPARAM(0), LPARAM(0))? };
    let exit = wait_until("app exit", || app.0.try_wait().ok().flatten())?;
    let log = events(&dir);
    println!("EVENTS_LOG_BEGIN\n{log}EVENTS_LOG_END");
    println!("CONTROLLER_PHASE_TARGET child_seen=true started_before_close={started_before_close}");
    assert_eq!(exit.code(), Some(0), "a user close is a normal quit");
    for error in STARTUP_ERRORS {
        assert!(!log.contains(error), "unexpected {error}");
    }
    assert!(!log.contains("fatal-"), "no fatal path after a user close");
    assert!(
        !started_before_close
            && !log.contains("started\n")
            && log.contains("closed-during-startup"),
        "INCONCLUSIVE: controller creation finished before WM_CLOSE was handled; rerun with a fresh directory"
    );
    drop(app);
    drop(fixture);
    remove_scratch(&dir);
    println!("CLOSE_DURING_CONTROLLER_CREATION_OK pid={pid}");
    Ok(())
}

/// Review F1: a genuine WebView2 environment failure after the window exists. The documented
/// WEBVIEW2_BROWSER_EXECUTABLE_FOLDER override points at a missing runtime folder (child env
/// only). The test host records `fatal-dialog` on the exact pre-dialog path instead of showing
/// modal UI; the real MessageBox stays the operator check (parity row 26).
#[test]
#[ignore = "interactive Windows WebView2 host test; run with --ignored --nocapture --test-threads=1"]
fn startup_failure_discards_quit_before_reporting() -> TestResult<()> {
    let dir = owned_scratch("nimbo-fatal")?;
    let fixture = Fixture::start()?;
    let child = Command::new(EXE)
        .env(
            "WEBVIEW2_BROWSER_EXECUTABLE_FOLDER",
            dir.join("missing-runtime"),
        )
        .arg("--test-host-url")
        .arg(&fixture.url)
        .arg("--test-host-dir")
        .arg(&dir)
        .spawn()?;
    let mut app = App(child);
    let pid = app.0.id();
    let exit = wait_until("app exit", || app.0.try_wait().ok().flatten())?;
    let log = events(&dir);
    println!("EVENTS_LOG_BEGIN\n{log}EVENTS_LOG_END");
    // 101 would mean the debug_assert (WM_QUIT still pending before the dialog) fired.
    assert_eq!(exit.code(), Some(1), "genuine failure exits 1");
    let order = [
        "webview2-unavailable\n",
        "fatal-quit-discarded\n",
        "fatal-dialog webview2-unavailable\n",
    ]
    .map(|line| log.find(line));
    assert!(
        order.iter().all(Option::is_some) && order.is_sorted(),
        "expected failure, discarded WM_QUIT, then dialog: {order:?}"
    );
    assert!(!log.contains("started"), "no navigation after a failure");
    drop(app);
    drop(fixture);
    remove_scratch(&dir);
    println!("STARTUP_FAILURE_REPORT_OK pid={pid}");
    Ok(())
}

/// CP1-F3-autoplay (user decision: Electron parity). With NO user input, the app-origin page's
/// unmuted `audio.play()` of silent synthetic PCM must resolve, while a foreign-origin iframe
/// (localhost vs the 127.0.0.1 app origin, no `allow=autoplay`) must not gain playback. Proves
/// the policy on this WebView2 Runtime only; real live sound stays an operator check (row 32).
#[test]
#[ignore = "interactive Windows WebView2 host test; run with --ignored --nocapture --test-threads=1"]
fn app_origin_autoplays_without_gesture_foreign_iframe_does_not() -> TestResult<()> {
    let dir = owned_scratch("nimbo-autoplay")?;
    let fixture = Fixture::serve(AUTOPLAY_PAGE, AUTOPLAY_CSP)?;
    let mut app = App(launch(&fixture, &dir)?);
    let pid = app.0.id();
    let main = wait_until("main window", || main_window(pid))?;
    let report = |key: &str| {
        let prefix = format!("/report?{key}=");
        fixture.requests().iter().find_map(|r| {
            r.strip_prefix(&prefix)
                .and_then(|rest| rest.split_whitespace().next())
                .map(str::to_owned)
        })
    };
    let top_activation = wait_until("top activation report", || report("top-activation"))?;
    let top = wait_until("top play result", || report("top-play"))?;
    let frame = wait_until("frame play result", || report("frame-play"))?;
    let log = events(&dir);
    println!("EVENTS_LOG_BEGIN\n{log}EVENTS_LOG_END");
    println!(
        "AUTOPLAY_RESULTS top_activation={top_activation} top={top} frame={frame} permission_event_allow={} permission_event_deny={}",
        log.contains("permission Autoplay Allow"),
        log.contains("permission Autoplay Deny")
    );
    assert_ne!(
        top_activation, "true",
        "no user gesture may precede the play"
    );
    assert_eq!(
        top, "played",
        "AUTOPLAY_RUNTIME_GATE: app-origin play() without gesture was rejected ({top}); \
         the app-origin AUTOPLAY ALLOW alone is insufficient on this runtime: coordinator decision"
    );
    assert_ne!(
        frame, "played",
        "a foreign-origin iframe must not gain autoplay"
    );
    assert!(!log.contains("permission Other Allow"));
    // SAFETY: test-owned child window.
    unsafe { PostMessageW(Some(main), WM_CLOSE, WPARAM(0), LPARAM(0))? };
    let exit = wait_until("app exit", || app.0.try_wait().ok().flatten())?;
    assert!(exit.success());
    drop(app);
    drop(fixture);
    remove_scratch(&dir);
    println!("AUTOPLAY_APP_ORIGIN_OK pid={pid}");
    Ok(())
}

/// CP2 S1 NATIVE-GATE (user-run on Windows): bridge v1 is present only in the app-origin main
/// frame; iframes (same-origin and `localhost`) see none, and a valid state-changing invoke they
/// post is never answered to (nor changes the state of) the top-level document; see BRIDGE_PAGE.
#[test]
#[ignore = "interactive Windows WebView2 host test; run with --ignored --nocapture --test-threads=1"]
fn bridge_present_in_main_frame_absent_in_iframes() -> TestResult<()> {
    let dir = owned_scratch("nimbo-bridge")?;
    let fixture = Fixture::serve(BRIDGE_PAGE, BRIDGE_CSP)?;
    let mut app = App(launch(&fixture, &dir)?);
    let pid = app.0.id();
    let main = wait_until("main window", || main_window(pid))?;
    let report = |key: &str| {
        let prefix = format!("/report?{key}=");
        fixture.requests().iter().find_map(|r| {
            r.strip_prefix(&prefix)
                .and_then(|rest| rest.split_whitespace().next())
                .map(decode)
        })
    };
    let top = wait_until("top bridge", || report("top-bridge"))?;
    let keys = wait_until("top keys", || report("top-keys"))?;
    let start = wait_until("top startAudio", || report("top-start"))?;
    let generation = wait_until("top generation", || report("top-gen"))?;
    let same = wait_until("same-origin iframe bridge", || report("same-bridge"))?;
    let foreign = wait_until("foreign iframe bridge", || report("foreign-bridge"))?;
    let same_post = wait_until("same-origin iframe probe", || report("same-post"))?;
    let foreign_post = wait_until("foreign iframe probe", || report("foreign-post"))?;
    let unexpected = wait_until("top observer after iframes", || report("top-unexpected"))?;
    let after = wait_until("top call after iframes", || report("top-after"))?;
    let control = wait_until("positive control", || report("top-control"))?;
    let results = wait_until("top result ids", || report("top-results"))?;
    let log = events(&dir);
    println!("EVENTS_LOG_BEGIN\n{log}EVENTS_LOG_END");
    println!(
        "BRIDGE_RESULTS top={top} start={start} gen={generation} same={same} foreign={foreign} same_post={same_post} foreign_post={foreign_post} unexpected={unexpected} after={after} control={control} results={results}"
    );
    assert_eq!(top, "object");
    assert_eq!(
        keys,
        "cancelSignIn,installUpdate,onAudio,onGameEnded,onGameState,onUpdateProgress,setCaptureAudio,signIn,startAudio,startGameWatch,stopAudio,stopGameWatch,version,watchUpdate"
    );
    assert_eq!(start, r#"{"ok":true,"value":null}"#);
    assert!(
        generation.parse::<u64>().is_ok_and(|g| g >= 1),
        "{generation}"
    );
    assert_eq!(same, "undefined", "no bridge in a same-origin iframe");
    assert_eq!(foreign, "undefined", "no bridge in a foreign iframe");
    // Each iframe really posted the valid probe, or has no chrome.webview at all (then nothing
    // can be attributed); a probe that was never attempted fails the test.
    for post in [&same_post, &foreign_post] {
        assert!(
            post == "sent" || post == "no-webview",
            "iframe probe: {post}"
        );
    }
    // Discriminating observation at the top level: no unissued result id, and the next bridge
    // id (2) was still fresh, i.e. `last_id` did not jump to an iframe's 4000000000/1.
    assert_eq!(unexpected, "none", "an iframe message was answered");
    assert_eq!(
        after, r#"{"ok":true,"value":null}"#,
        "state changed by an iframe"
    );
    // Positive control: the same valid request from the top document IS observed.
    assert_eq!(control, "4000000100", "observer must see an accepted probe");
    assert_eq!(results, "1,2,4000000100");
    // A message posted from an iframe yields `bridge-unauthorized` or nothing; every other
    // drop code would mean an iframe message reached the main-frame dispatcher.
    for code in [
        "bridge-frame-id-unavailable",
        "bridge-replayed-id",
        "bridge-stale",
        "bridge-malformed",
        "bridge-not-ready",
        "bridge-disabled",
        "bridge-duplicate-hello",
        "bridge-unknown-method",
    ] {
        assert!(!log.contains(code), "unexpected {code}");
    }
    // SAFETY: test-owned child window.
    unsafe { PostMessageW(Some(main), WM_CLOSE, WPARAM(0), LPARAM(0))? };
    let exit = wait_until("app exit", || app.0.try_wait().ok().flatten())?;
    assert!(exit.success());
    drop(app);
    drop(fixture);
    remove_scratch(&dir);
    println!("BRIDGE_MAIN_FRAME_ONLY_OK pid={pid}");
    Ok(())
}

/// CP2 S2-core NATIVE-GATE login page: click 1 starts a signIn; click 2 starts another and
/// cancels it 500 ms later. Every Result goes to /report. Synthetic credentials only.
const LOGIN_PAGE: &str = r#"<!doctype html><meta charset=utf-8><title>login</title><body style="margin:0;height:100vh">
<script>
const report = (k, x) => fetch('/report?' + k + '=' + encodeURIComponent(x)).catch(() => {});
let clicks = 0;
document.addEventListener('click', () => {
  clicks++;
  const n = clicks;
  window.nimboDesktop.signIn().then((r) => report('signin' + n, JSON.stringify(r)));
  if (n === 2) setTimeout(() => window.nimboDesktop.cancelSignIn().then((r) => report('cancel', JSON.stringify(r))), 500);
});
</script>"#;

/// CP2 S2-core NATIVE-GATE (user-run on Windows): the test acts as the system browser. It reads
/// `<dir>/login-url.txt`, POSTs a synthetic credential to the real receiver, and checks the
/// done page, the page-visible Result, the redacted recorder line, the deleted URL file, the
/// closed port, the cancel path and the bounded shutdown record. NOT a Google/Firebase login.
#[test]
#[ignore = "interactive Windows WebView2 host test; run with --ignored --nocapture --test-threads=1"]
fn synthetic_browser_login_round_trip() -> TestResult<()> {
    let dir = owned_scratch("nimbo-login")?;
    let fixture = Fixture::serve(LOGIN_PAGE, CSP)?;
    let origin = fixture.url.trim_end_matches('/').to_owned();
    let mut app = App(launch(&fixture, &dir)?);
    let pid = app.0.id();
    let main = wait_until("main window", || main_window(pid))?;
    let widget = wait_until("renderer", || render_widget(main))?;
    let report = |key: &str| {
        let prefix = format!("/report?{key}=");
        fixture.requests().iter().find_map(|r| {
            r.strip_prefix(&prefix)
                .and_then(|rest| rest.split_whitespace().next())
                .map(decode)
        })
    };
    wait_until("started", || events(&dir).contains("started").then_some(()))?;
    click(widget);
    let url_file = dir.join("login-url.txt");
    let url = wait_until("login-url.txt", || std::fs::read_to_string(&url_file).ok())?;
    let prefix = format!("{origin}/desktop-login?port=");
    let rest = url.strip_prefix(&prefix).ok_or("login URL shape")?;
    let (port, state) = rest.split_once("&state=").ok_or("login URL state")?;
    let port: u16 = port.parse()?;
    assert!(port >= 1024 && state.len() == 48);
    let body = format!("state={state}&idToken=synthetic-id&accessToken=synthetic-access");
    let mut socket = std::net::TcpStream::connect(("127.0.0.1", port))?;
    socket.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        socket,
        "POST /callback HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: {origin}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )?;
    let mut response = String::new();
    socket.read_to_string(&mut response)?;
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with(nimbo_desktop::loopback::LOGIN_DONE_PAGE));
    let first = wait_until("signIn result", || report("signin1"))?;
    assert_eq!(
        first,
        r#"{"ok":true,"value":{"idToken":"synthetic-id","accessToken":"synthetic-access"}}"#
    );
    wait_until("login-url.txt deleted", || {
        (!url_file.exists()).then_some(())
    })?;
    let refused = wait_until("receiver port closed", || {
        std::net::TcpStream::connect(("127.0.0.1", port))
            .is_err()
            .then_some(())
    });
    assert!(refused.is_ok(), "second POST must be refused");
    click(widget);
    let second = wait_until("cancelled signIn", || report("signin2"))?;
    assert_eq!(second, r#"{"ok":false,"error":"Login cancelado."}"#);
    assert_eq!(wait_until("cancel", || report("cancel"))?, r#"{"ok":true}"#);
    // SAFETY: test-owned child window.
    unsafe { PostMessageW(Some(main), WM_CLOSE, WPARAM(0), LPARAM(0))? };
    let exit = wait_until("app exit", || app.0.try_wait().ok().flatten())?;
    assert!(exit.success());
    let log = events(&dir);
    println!(
        "EVENTS_LOG_BEGIN\n{}EVENTS_LOG_END",
        log.replace(state, "<STATE-LEAKED>")
    );
    for code in [
        "login-begin",
        "login-credential-delivered",
        "login-result-posted",
        "focus-request",
        "login-cancelled",
        "shutdown workers=0 acceptors=0",
    ] {
        assert!(log.contains(code), "missing {code}");
    }
    assert!(log.contains(&format!("external {prefix}{port}&state=<redacted>")));
    for secret in [state, "synthetic-id", "synthetic-access"] {
        assert!(!log.contains(secret), "events.log leaked a secret");
    }
    drop(app);
    drop(fixture);
    remove_scratch(&dir);
    println!("SYNTHETIC_LOGIN_ROUND_TRIP_OK pid={pid} (NOT a Firebase sign-in)");
    Ok(())
}
