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
    wait_until("bridge report", || {
        fixture
            .requests()
            .iter()
            .any(|r| r.starts_with("/report?bridge=undefined"))
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
