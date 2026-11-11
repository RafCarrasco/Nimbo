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

struct Fixture {
    url: String,
    log: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Fixture {
    fn start() -> std::io::Result<Self> {
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
                        let (status, body) = if path == "/" {
                            ("200 OK", PAGE)
                        } else if path.starts_with("/report?") {
                            ("204 No Content", "")
                        } else {
                            ("404 Not Found", "")
                        };
                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: text/html\r\nContent-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; connect-src 'self'\r\nConnection: close\r\n\r\n{body}",
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
        // Bounded teardown: the WM_CLOSE path is exercised in the test body; this is a backstop.
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

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
