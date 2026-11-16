//! Checkpoint-1 Windows host shell: one HWND + WebView2 that opens the hosted Nimbo with the
//! Electron window/navigation/popup/permission/UA/reload/single-instance semantics.
//!
//! CP2 S1 adds the `nimboDesktop` bridge v1 (portable dispatcher in `crate::bridge`): injected
//! script awaited before navigation, ContentLoading generation/main-frame attribution and
//! WebMessageReceived. CP2 S2-core adds the browser-login receiver wiring: a `LoginFlow` whose
//! receiver threads wake this UI thread with `WM_APP_LOGIN` (PostMessageW, never blocking), a
//! 1 s `WM_TIMER` fallback, and bounded shutdown after the message loop. Execution of all of
//! this is a Windows NATIVE-GATE. CP2 S3 adds the state-only activation link: a second instance
//! forwards it as a bounded WM_COPYDATA decided from the header alone (`plan_copydata`), the
//! first instance consumes the one-shot login slot and focuses; release builds conservatively
//! self-register `HKCU\Software\Classes\golive` (`crate::scheme`). NOT here: capture/audio/
//! game watch, updater/installer.
//! A debug-only `test-host` build replaces the hosted URL with a loopback fixture, keeps every
//! file under an owned scratch directory and records external opens instead of launching them.
use crate::bridge::{self, Dispatcher, Outbound};
use crate::login::{Activation, ActivationLink, Hooks, LoginFlow, Notify, Scheme};
use crate::policy::{
    BROWSER_ARGUMENTS, CaptureDecision, Config, KeyAction, KeyInput, MAIN_WINDOW, Navigation,
    Permission, PermissionDecision, accelerator, user_agent, webview2_permission,
    webview2_permission_kind,
};
use crate::shell::{
    AsyncWaitEnd, FOCUS_REQUEST, Guards, InstanceIdentity, InstanceRequest, LINK_REQUEST, Rect,
    Startup, StartupFailure, colorref, initial_window_rect, instance_request, min_track_size,
    plan_copydata, window_title,
};
use std::{
    cell::RefCell,
    ffi::OsString,
    path::{Path, PathBuf},
    process::ExitCode,
    rc::Rc,
    sync::{Arc, Mutex, OnceLock, PoisonError, mpsc},
    time::{Duration, Instant},
};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::*;
use windows::Win32::{
    Foundation::{
        COLORREF, CloseHandle, E_POINTER, E_UNEXPECTED, ERROR_ALREADY_EXISTS, GetLastError, HANDLE,
        HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
    },
    Graphics::Gdi::{
        CreateSolidBrush, GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
        UpdateWindow,
    },
    Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom},
    System::{
        Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
        DataExchange::COPYDATASTRUCT,
        LibraryLoader::GetModuleHandleW,
        Threading::CreateMutexW,
    },
    UI::{
        HiDpi::{
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, GetDpiForWindow,
            MDT_EFFECTIVE_DPI, SetProcessDpiAwarenessContext,
        },
        Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL},
        Shell::SetCurrentProcessExplicitAppUserModelID,
        WindowsAndMessaging::*,
    },
};
use windows::core::{HSTRING, Interface, PCWSTR, PWSTR, Result, w};

const MAIN_CLASS: PCWSTR = w!("Nimbo.MainWindow");
const WM_APP_EXTERNAL: u32 = WM_APP + 1;
/// Receiver threads -> UI thread: drain login settlements (`Dispatcher::drain_settlements`).
const WM_APP_LOGIN: u32 = WM_APP + 2;
/// 1 s login housekeeping (expiry fallback, unclaimed drop, activation TTL).
const LOGIN_TIMER: usize = 1;
const LOGIN_SHUTDOWN_WAIT: Duration = Duration::from_secs(2);
/// MAKEINTRESOURCE(1): the icon embedded by build.rs from desktop/icon.ico.
const ICON_ID: PCWSTR = PCWSTR(std::ptr::without_provenance(1));

// ---------- fixed-code logging; payloads only in the owned test-host recorder ----------

thread_local! {
    static RECORDER: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

struct State {
    main: HWND,
    controller: Option<ICoreWebView2Controller>,
    external: Vec<String>,
    config: Rc<Config>,
    dispatcher: Option<Rc<RefCell<Dispatcher>>>,
    core: Option<ICoreWebView2>,
    login: Option<LoginFlow>,
    /// HWND (as isize) the login notifier may post to; `None` once WM_DESTROY began, so a late
    /// receiver thread can never post to a destroyed (and possibly reused) handle.
    notify_target: Arc<Mutex<Option<isize>>>,
}

/// Activation scheme of this process (release `golive` or this test directory's isolated
/// scheme); set once in `run` before any window exists.
static LINK_SCHEME: OnceLock<Scheme> = OnceLock::new();

/// One monotonic clock for the dispatcher and the login flow (`Instant`-based ms).
fn now_ms() -> u64 {
    static STARTED: OnceLock<Instant> = OnceLock::new();
    u64::try_from(STARTED.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Plan T3: the system-preferred CSPRNG; a failure is reported, never replaced.
fn bcrypt_rng(buf: &mut [u8; 24]) -> bool {
    // SAFETY: `buf` is a valid writable buffer for the duration of the call.
    unsafe { BCryptGenRandom(None, buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }.is_ok()
}

/// Cross-thread wake-up. PostMessageW never blocks; the target lock is held only around it and
/// is cleared by WM_DESTROY (HWND lifetime/reuse). A failed post is reported by the flow
/// (`login-notify-failed`); the 1 s timer then still drains.
fn login_notifier(target: Arc<Mutex<Option<isize>>>) -> Notify {
    Arc::new(move || {
        let target = target.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(hwnd) = *target else { return false };
        // SAFETY: the handle is our live main window (cleared before it is destroyed).
        unsafe {
            PostMessageW(
                Some(HWND(hwnd as *mut _)),
                WM_APP_LOGIN,
                WPARAM(0),
                LPARAM(0),
            )
        }
        .is_ok()
    })
}
fn clear_notify_target(target: &Arc<Mutex<Option<isize>>>) {
    *target.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

/// Release builds print only fixed codes (never URLs, titles or arguments). The test-host
/// recorder also stores the detail, but only under its owned loopback scratch directory.
fn event(code: &str, detail: Option<&str>) {
    eprintln!("nimbo-host: {code}");
    if cfg!(feature = "test-host") {
        RECORDER.with(|r| {
            if let Some(path) = r.borrow().as_ref() {
                use std::io::Write;
                let line = match detail {
                    Some(d) => format!("{code} {d}\n"),
                    None => format!("{code}\n"),
                };
                let written = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .and_then(|mut f| f.write_all(line.as_bytes()));
                if written.is_err() {
                    eprintln!("nimbo-host: recorder-write-failed");
                }
            }
        });
    }
}

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}
fn controller() -> Option<ICoreWebView2Controller> {
    with_state(|s| s.controller.clone()).flatten()
}
fn main_window() -> Option<HWND> {
    with_state(|s| s.main)
}

// ---------- launch modes (selected at compile time) ----------

struct Launch {
    config: Rc<Config>,
    identity: InstanceIdentity,
    user_data: PathBuf,
    test_host: bool,
    scheme: Scheme,
    /// A valid activation link argument (state only; never logged).
    link: Option<ActivationLink>,
    #[cfg(feature = "test-host")]
    scheme_op: Option<crate::shell::SchemeOp>,
    #[cfg(feature = "test-host")]
    dir: PathBuf,
}

#[cfg(not(feature = "test-host"))]
fn launch(args: &[OsString]) -> std::result::Result<Launch, &'static str> {
    use crate::shell::LinkArg;
    let summary = crate::shell::classify_args(args);
    let link = match summary.link {
        LinkArg::Link(link) => Some(link),
        LinkArg::Malformed => {
            event("link-ignored", None);
            None
        }
        LinkArg::None => None,
    };
    for _ in 0..summary.ignored {
        event("ignored-argument", None);
    }
    Ok(Launch {
        config: Rc::new(Config::release()),
        identity: InstanceIdentity::release(),
        user_data: local_app_data()
            .ok_or("local-app-data-unavailable")?
            .join("Nimbo")
            .join("WebView2"),
        test_host: false,
        scheme: Scheme::Release,
        link,
    })
}

#[cfg(feature = "test-host")]
fn launch(args: &[OsString]) -> std::result::Result<Launch, &'static str> {
    let parsed = crate::shell::parse_test_host_args(args)?;
    let meta = std::fs::symlink_metadata(&parsed.dir).map_err(|_| "test-host-dir-missing")?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("test-host-dir-not-owned-directory");
    }
    RECORDER.with(|r| *r.borrow_mut() = Some(parsed.dir.join("events.log")));
    Ok(Launch {
        identity: InstanceIdentity::test_host(&parsed.dir),
        user_data: parsed.dir.join("webview2"),
        config: Rc::new(parsed.config),
        test_host: true,
        scheme: Scheme::test_host(&parsed.dir),
        link: parsed.link,
        scheme_op: parsed.scheme_op,
        dir: parsed.dir,
    })
}

/// `%LOCALAPPDATA%` (release only; the test host never touches the user's profile folders).
#[cfg(not(feature = "test-host"))]
fn local_app_data() -> Option<PathBuf> {
    use windows::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
    };
    // SAFETY: known-folder API allocates a CoTaskMem string that is freed exactly once below.
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None).ok()?;
        let owned = path.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(path.0 as *const _));
        owned.filter(|p| p.is_absolute())
    }
}

// ---------- external opener (recorder under test-host) ----------

fn queue_external(url: String) {
    let main = with_state(|s| {
        s.external.push(url);
        s.main
    });
    if let Some(main) = main {
        // Deferred out of the WebView2 callback; Electron also opens asynchronously.
        // SAFETY: posting to our own live window on its owning thread.
        if unsafe { PostMessageW(Some(main), WM_APP_EXTERNAL, WPARAM(0), LPARAM(0)) }.is_err() {
            event("external-queue-failed", None);
        }
    }
}

fn open_external(url: &str) {
    // Defense in depth: only https ever leaves the app (main.cjs:714, 720); the debug test-host
    // additionally allows its loopback fixture origin (login URLs), see `external_allowed`.
    let allowed = with_state(|s| s.config.external_allowed(url)).unwrap_or(false);
    if !allowed {
        event("external-rejected", None);
        return;
    }
    // The recorder never sees the single-use state (full URL only in login-url.txt).
    #[cfg(feature = "test-host")]
    event("external", Some(&crate::login::redact_login_url(url)));
    #[cfg(not(feature = "test-host"))]
    {
        use windows::Win32::UI::Shell::ShellExecuteW;
        // SAFETY: valid NUL-terminated strings for the duration of the call.
        let result = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                &HSTRING::from(url),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if result.0 as isize <= 32 {
            event("external-open-failed", None);
        } else {
            event("external-opened", None);
        }
    }
}

/// Test-host only: the full login URL goes to `<dir>/login-url.txt` (owned scratch), never to
/// events.log, and is deleted once no login is active.
fn record_login_url(url: &str) {
    if cfg!(feature = "test-host")
        && let Some(path) = RECORDER.with(|r| {
            r.borrow()
                .as_ref()
                .map(|p| p.with_file_name("login-url.txt"))
        })
        && std::fs::write(path, url).is_err()
    {
        event("login-url-write-failed", None);
    }
}
fn sync_login_url() {
    if !cfg!(feature = "test-host") {
        return;
    }
    let active =
        with_state(|s| s.login.as_ref().is_some_and(LoginFlow::is_active)).unwrap_or(false);
    if !active
        && let Some(path) = RECORDER.with(|r| {
            r.borrow()
                .as_ref()
                .map(|p| p.with_file_name("login-url.txt"))
        })
        && path.exists()
        && std::fs::remove_file(path).is_err()
    {
        event("login-url-remove-failed", None);
    }
}
/// UI-thread login handoff (WM_APP_LOGIN / WM_TIMER): the dispatcher borrow is released before
/// any COM call; a COM failure closes the WebView like every other handler; focus only when a
/// result was actually posted (parity main.cjs:108).
fn login_handoff(run: impl FnOnce(&mut Dispatcher, u64) -> Vec<Outbound>) {
    let handles = with_state(|s| {
        Some((
            s.dispatcher.clone()?,
            s.core.clone()?,
            s.controller.clone()?,
            s.main,
        ))
    })
    .flatten();
    let Some((dispatcher, core, controller, main)) = handles else {
        return;
    };
    let out = match dispatcher.try_borrow_mut() {
        Ok(mut d) => run(&mut d, now_ms()),
        Err(_) => {
            event("login-handoff-reentered", None); // the 1 s timer retries
            return;
        }
    };
    // Parity main.cjs:105-108: focus when the browser callback produced the posted result
    // (credential or missing-credential), not on expiry or cancel.
    let posted = out
        .iter()
        .any(|o| matches!(o, Outbound::Code("login-result-posted")))
        && out.iter().any(|o| {
            matches!(
                o,
                Outbound::Code("login-credential-claimed" | "login-callback-missing-credential")
            )
        });
    let outcome = deliver(&core, out);
    if fail_closed(&controller, main, outcome).is_ok() && posted {
        focus_main();
    }
    sync_login_url();
}

// ---------- single instance ----------

struct InstanceLock(HANDLE);
impl Drop for InstanceLock {
    fn drop(&mut self) {
        // SAFETY: handle owned by this guard.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn acquire_instance(identity: &InstanceIdentity) -> Result<Option<InstanceLock>> {
    // SAFETY: valid name; last error is read immediately after creation.
    unsafe {
        let handle = CreateMutexW(None, false, &HSTRING::from(&identity.mutex))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            return Ok(None);
        }
        Ok(Some(InstanceLock(handle)))
    }
}

/// Second instance: ask the first to focus itself (main.cjs:28-36) and, with a valid link,
/// forward the canonical link (UTF-16LE, at most 188 bytes) for the one-shot slot; then quit.
fn forward_focus(identity: &InstanceIdentity, link: Option<&ActivationLink>) {
    let class = HSTRING::from(&identity.class);
    // Owned buffer that outlives the synchronous send.
    let payload: Vec<u8> = link.map(ActivationLink::utf16le).unwrap_or_default();
    let data = match link {
        Some(_) => COPYDATASTRUCT {
            dwData: LINK_REQUEST,
            cbData: u32::try_from(payload.len()).unwrap_or(0),
            lpData: payload.as_ptr() as *mut _,
        },
        None => COPYDATASTRUCT {
            dwData: FOCUS_REQUEST,
            cbData: 0,
            lpData: std::ptr::null_mut(),
        },
    };
    for _ in 0..50 {
        // SAFETY: message-only window lookup and a bounded synchronous WM_COPYDATA; the
        // COPYDATASTRUCT and its payload buffer outlive the call.
        unsafe {
            if let Ok(target) = FindWindowExW(Some(HWND_MESSAGE), None, &class, PCWSTR::null()) {
                let mut pid = 0;
                GetWindowThreadProcessId(target, Some(&mut pid));
                // Let the first instance take the foreground (as Chromium's singleton does).
                let _ = AllowSetForegroundWindow(pid);
                SendMessageTimeoutW(
                    target,
                    WM_COPYDATA,
                    WPARAM(0),
                    LPARAM(&data as *const _ as isize),
                    SMTO_ABORTIFHUNG | SMTO_BLOCK,
                    5000,
                    None,
                );
                if link.is_some() {
                    event("single-instance-forwarded-link", None);
                } else {
                    event("single-instance-forwarded", None);
                }
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    event("single-instance-target-missing", None);
}

fn focus_main() {
    let Some(main) = main_window() else { return };
    // SAFETY: our own window on its owning thread.
    unsafe {
        if IsIconic(main).as_bool() {
            let _ = ShowWindow(main, SW_RESTORE);
        }
        let _ = ShowWindow(main, SW_SHOW);
        let _ = SetForegroundWindow(main);
    }
    if let Some(c) = controller() {
        // SAFETY: live controller on its UI thread.
        let _ = unsafe { c.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC) };
    }
    event("focus-request", None);
}

// ---------- window procedures ----------

fn resize_controller(main: HWND) {
    let Some(c) = controller() else { return };
    let mut rect = RECT::default();
    // SAFETY: our own window/controller on the UI thread.
    unsafe {
        if GetClientRect(main, &mut rect).is_ok() && c.SetBounds(rect).is_err() {
            event("controller-bounds-failed", None);
        }
    }
}

extern "system" fn main_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: Win32 delivers valid message parameters for these message ids to our window.
    unsafe {
        match msg {
            WM_GETMINMAXINFO => {
                let info = lparam.0 as *mut MINMAXINFO;
                if let Some(info) = info.as_mut() {
                    let (w, h) = min_track_size(GetDpiForWindow(hwnd));
                    info.ptMinTrackSize = POINT { x: w, y: h };
                }
                LRESULT(0)
            }
            WM_SIZE => {
                // Never hide the WebView when minimized: Electron keeps rendering for live
                // streams (backgroundThrottling:false).
                if wparam.0 as u32 != SIZE_MINIMIZED {
                    resize_controller(hwnd);
                }
                LRESULT(0)
            }
            WM_MOVE | WM_MOVING => {
                if let Some(c) = controller() {
                    let _ = c.NotifyParentWindowPositionChanged();
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_DPICHANGED => {
                if let Some(r) = (lparam.0 as *const RECT).as_ref() {
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                LRESULT(0)
            }
            WM_SETFOCUS => {
                if let Some(c) = controller() {
                    let _ = c.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
                }
                LRESULT(0)
            }
            WM_APP_EXTERNAL => {
                let pending = with_state(|s| std::mem::take(&mut s.external)).unwrap_or_default();
                for url in pending {
                    open_external(&url);
                }
                LRESULT(0)
            }
            WM_APP_LOGIN => {
                login_handoff(|d, now| d.drain_settlements(now));
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == LOGIN_TIMER => {
                login_handoff(|d, now| d.tick(now));
                LRESULT(0)
            }
            WM_DESTROY => {
                // Login first: no further wake-ups to this handle, settle and signal every
                // receiver thread and close their sockets; never waits here (bounded wait runs
                // after the message loop, in `run_window`).
                let parts = with_state(|s| {
                    s.dispatcher = None;
                    s.core = None;
                    (s.notify_target.clone(), s.login.clone())
                });
                if let Some((target, login)) = parts {
                    clear_notify_target(&target);
                    if let Some(login) = login {
                        for settle in login.shutdown() {
                            event(settle.code, None);
                        }
                    }
                }
                let _ = KillTimer(Some(hwnd), LOGIN_TIMER);
                if let Some(c) = with_state(|s| s.controller.take()).flatten()
                    && c.Close().is_err()
                {
                    event("controller-close-failed", None);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

extern "system" fn instance_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA {
        return copydata(lparam);
    }
    // SAFETY: default processing of our own window's message.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// WM_COPYDATA from a second instance (UI thread, inside the sender's SendMessageTimeout). The
/// header alone decides (`plan_copydata`) whether the payload may be read; the only read is one
/// bounded byte copy inside the reader closure. No lock or RefCell borrow is held across COM,
/// sockets or waits; a link is never queued (a second attempt would be a second activation).
fn copydata(lparam: LPARAM) -> LRESULT {
    if lparam.0 == 0 {
        event("instance-message-ignored", None);
        return LRESULT(0);
    }
    // SAFETY: for WM_COPYDATA, lparam points to a COPYDATASTRUCT valid during the call; only the
    // header is copied by value (unaligned-safe), lpData is used as a pointer value only.
    let header = unsafe { std::ptr::read_unaligned(lparam.0 as *const COPYDATASTRUCT) };
    let source = header.lpData as *const u8;
    let plan = plan_copydata(header.dwData, header.cbData, source.is_null());
    let Some(scheme) = LINK_SCHEME.get().copied() else {
        event("instance-message-ignored", None);
        return LRESULT(0);
    };
    let request = instance_request(plan, scheme, |len| {
        // SAFETY: `plan` admitted exactly this even 2..=512 byte length with a non-null pointer;
        // the sender's buffer is valid and read-only for the duration of WM_COPYDATA. Bytes are
        // decoded pairwise later: no u16 alignment is assumed.
        let bytes = unsafe { std::slice::from_raw_parts(source, len) };
        Some(bytes.to_vec())
    });
    match request {
        Some(InstanceRequest::Focus) => {
            focus_main();
            LRESULT(1)
        }
        Some(InstanceRequest::Link(link)) => {
            activate_link(&link);
            focus_main();
            LRESULT(1)
        }
        None => {
            event("instance-message-ignored", None);
            LRESULT(0)
        }
    }
}

/// One short borrow clones the flow handle (an Arc bump); the slot is consumed after the borrow
/// ended. A missing flow/state (startup, re-entered borrow, shutdown) drops the link.
fn activate_link(link: &ActivationLink) {
    let login = with_state(|s| s.login.clone()).flatten();
    let code = match login.map(|l| l.activate(link, now_ms())) {
        Some(Activation::Activated) => "link-activated",
        Some(Activation::Ignored) | None => "link-ignored",
    };
    event(code, None);
}

// ---------- window creation ----------

fn create_windows(identity: &InstanceIdentity) -> Result<(HWND, HWND)> {
    // SAFETY: classes/windows are created and used on this UI thread only.
    unsafe {
        let instance = GetModuleHandleW(PCWSTR::null())?.into();
        let icon = LoadIconW(Some(instance), ICON_ID).ok();
        if icon.is_none() {
            event("icon-resource-missing", None);
        }
        let small = LoadImageW(
            Some(instance),
            ICON_ID,
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_DEFAULTCOLOR,
        )
        .ok()
        .map(|h| HICON(h.0));
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(main_proc),
            hInstance: instance,
            hIcon: icon.unwrap_or_default(),
            hIconSm: small.or(icon).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: CreateSolidBrush(COLORREF(colorref(MAIN_WINDOW.background))),
            lpszClassName: MAIN_CLASS,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        let instance_class = HSTRING::from(&identity.class);
        let message_class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(instance_proc),
            hInstance: instance,
            lpszClassName: PCWSTR(instance_class.as_ptr()),
            ..Default::default()
        };
        if RegisterClassExW(&message_class) == 0 {
            return Err(windows::core::Error::from_thread());
        }

        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let work = if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            RECT {
                left: 0,
                top: 0,
                right: 1280,
                bottom: 800,
            }
        };
        let (mut dpi, mut dpi_y) = (96, 96);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y).is_err() {
            dpi = 96;
        }
        let r = initial_window_rect(
            Rect {
                left: work.left,
                top: work.top,
                right: work.right,
                bottom: work.bottom,
            },
            dpi,
        );
        let main = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            MAIN_CLASS,
            &HSTRING::from(MAIN_WINDOW.title),
            WS_OVERLAPPEDWINDOW,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            None,
            None, // no menu (main.cjs:665, 703)
            Some(instance),
            None,
        )?;
        let message = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(instance_class.as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance),
            None,
        );
        let message = match message {
            Ok(m) => m,
            Err(e) => {
                let _ = DestroyWindow(main);
                return Err(e);
            }
        };
        let _ = ShowWindow(main, SW_SHOW);
        let _ = UpdateWindow(main);
        Ok((main, message))
    }
}

// ---------- WebView2 ----------

// Environment/controller creation uses its own completion handler plus
// `webview2_com::wait_with_pump` instead of `wait_for_async_operation`: that helper `expect`s
// its channel send, so a completion delivered after the startup wait already ended (the user
// closed the window, or a later modal loop dispatches it) would panic inside the COM callback
// and abort. Here a late completion finds no receiver and is dropped quietly.

fn create_environment(
    user_data: &Path,
) -> std::result::Result<ICoreWebView2Environment, AsyncWaitEnd> {
    let options = CoreWebView2EnvironmentOptions::default();
    // SAFETY: options object is exclusively owned here before being handed to WebView2.
    unsafe { options.set_additional_browser_arguments(BROWSER_ARGUMENTS.to_owned()) };
    let options: ICoreWebView2EnvironmentOptions = options.into();
    let folder = HSTRING::from(user_data.as_os_str());
    let (tx, rx) = mpsc::channel();
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
        move |result, environment| {
            let _ = tx.send(result.and_then(|()| {
                environment.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))
            }));
            Ok(())
        },
    ));
    // SAFETY: arguments live for the call; the completion handler is ref-counted.
    unsafe {
        CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), &folder, &options, &handler)
    }
    .map_err(|_| AsyncWaitEnd::Failed)?;
    wait_end(webview2_com::wait_with_pump(rx))
}

fn create_controller(
    environment: &ICoreWebView2Environment,
    main: HWND,
) -> std::result::Result<ICoreWebView2Controller, AsyncWaitEnd> {
    let (tx, rx) = mpsc::channel();
    let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
        move |result, controller| {
            let _ = tx.send(result.and_then(|()| {
                controller.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))
            }));
            Ok(())
        },
    ));
    // SAFETY: `main` is our live top-level window on this thread; the handler is ref-counted.
    unsafe { environment.CreateCoreWebView2Controller(main, &handler) }
        .map_err(|_| AsyncWaitEnd::Failed)?;
    wait_end(webview2_com::wait_with_pump(rx))
}

/// Keeps the pinned crate's error taxonomy: only `TaskCanceled` (the startup pump retrieved
/// WM_QUIT) means "closed during startup"; every other error is a genuine failure.
fn wait_end<T>(waited: webview2_com::Result<Result<T>>) -> std::result::Result<T, AsyncWaitEnd> {
    match waited {
        Ok(completed) => completed.map_err(|_| AsyncWaitEnd::Failed),
        Err(webview2_com::Error::TaskCanceled) => Err(AsyncWaitEnd::QuitRequested),
        Err(_) => Err(AsyncWaitEnd::Failed),
    }
}

/// AddScriptToExecuteOnDocumentCreated is asynchronous: the script is ready to run only after
/// its completion handler ran (DOCUMENTED), so navigation waits for it like controller creation.
fn add_bridge_script(core: &ICoreWebView2, script: &str) -> std::result::Result<(), AsyncWaitEnd> {
    let (tx, rx) = mpsc::channel();
    let handler = AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(
        move |result, _id| {
            let _ = tx.send(result);
            Ok(())
        },
    ));
    // SAFETY: live core on its UI thread; the completion handler is ref-counted.
    unsafe { core.AddScriptToExecuteOnDocumentCreated(&HSTRING::from(script), &handler) }
        .map_err(|_| AsyncWaitEnd::Failed)?;
    wait_end(webview2_com::wait_with_pump(rx))
}

/// Posts replies to the sender's current top-level document (WebView2 discards a reply if a
/// navigation happened first) and logs fixed codes; suppressed counts only in the recorder.
fn deliver(core: &ICoreWebView2, out: Vec<Outbound>) -> Result<()> {
    for item in out {
        match item {
            // SAFETY: live core on its UI thread.
            Outbound::Reply(json) => unsafe { core.PostWebMessageAsJson(&HSTRING::from(json))? },
            // The credential result: its UTF-16 copy is transient; the owned UTF-8 buffer is
            // wiped (best effort) when `secret` drops at the end of this arm.
            Outbound::Secret(secret) => {
                // SAFETY: live core on its UI thread.
                unsafe { core.PostWebMessageAsJson(&HSTRING::from(secret.as_str()))? }
            }
            Outbound::OpenLogin(url) => {
                record_login_url(url.as_str());
                queue_external(url.as_str().to_owned());
            }
            Outbound::Code(code) => event(code, None),
            Outbound::Suppressed(n) => event("bridge-drops-suppressed", Some(&n.to_string())),
            Outbound::CallbackSuppressed(n) => {
                event("callback-codes-suppressed", Some(&n.to_string()))
            }
        }
    }
    Ok(())
}

fn reentered<T>(_: T) -> windows::core::Error {
    windows::core::Error::from_hresult(E_UNEXPECTED)
}

fn read_pwstr(read: impl FnOnce(&mut PWSTR) -> Result<()>) -> Result<String> {
    let mut value = PWSTR::null();
    read(&mut value)?;
    Ok(take_pwstr(value))
}

// The portable table must match the pinned bindings exactly (build fails otherwise).
const _: () = {
    assert!(COREWEBVIEW2_PERMISSION_KIND_MICROPHONE.0 == webview2_permission_kind::MICROPHONE);
    assert!(COREWEBVIEW2_PERMISSION_KIND_CAMERA.0 == webview2_permission_kind::CAMERA);
    assert!(
        COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ.0 == webview2_permission_kind::CLIPBOARD_READ
    );
    assert!(COREWEBVIEW2_PERMISSION_KIND_AUTOPLAY.0 == webview2_permission_kind::AUTOPLAY);
};

fn permission_kind(kind: COREWEBVIEW2_PERMISSION_KIND) -> Permission {
    webview2_permission(kind.0)
}

fn args_or_pointer<T>(args: Option<T>) -> Result<T> {
    args.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))
}

/// Applies settings and registers every guard BEFORE the first navigation. Any failure is
/// returned (caller closes the controller); a later handler failure closes the controller and
/// the window via `super::close_on_error`, never leaving an unguarded page.
fn install(
    controller: &ICoreWebView2Controller,
    core: &ICoreWebView2,
    config: &Rc<Config>,
    dispatcher: &Rc<RefCell<Dispatcher>>,
    main: HWND,
) -> Result<Guards> {
    let mut guards = Guards::default();
    let mut token = 0;
    // SAFETY: live controller/core on the owning UI thread; every HRESULT is propagated.
    unsafe {
        let settings = core.Settings()?;
        settings.SetAreDefaultContextMenusEnabled(false)?; // Electron shows no context menu
        settings.SetAreDevToolsEnabled(false)?; // no DevTools accelerator without a menu
        settings.SetIsStatusBarEnabled(false)?;
        settings.SetAreHostObjectsAllowed(false)?; // never AddHostObjectToScript
        settings.SetIsWebMessageEnabled(true)?; // bridge v1 JSON messages (CP2 S1)
        let settings2 = settings.cast::<ICoreWebView2Settings2>()?;
        let default_ua = read_pwstr(|p| settings2.UserAgent(p))?;
        settings2.SetUserAgent(&HSTRING::from(user_agent(&default_ua)))?;
        let settings4 = settings.cast::<ICoreWebView2Settings4>()?;
        settings4.SetIsGeneralAutofillEnabled(false)?;
        settings4.SetIsPasswordAutosaveEnabled(false)?;
        let [r, g, b] = MAIN_WINDOW.background;
        controller
            .cast::<ICoreWebView2Controller2>()?
            .SetDefaultBackgroundColor(COREWEBVIEW2_COLOR {
                A: 255,
                R: r,
                G: g,
                B: b,
            })?;
        guards.settings = true;

        let c = controller.clone();
        let cfg = config.clone();
        core.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                let outcome = args_or_pointer(args).and_then(|args| {
                    let uri = read_pwstr(|p| args.Uri(p))?;
                    match cfg.navigation(&uri) {
                        Navigation::Allow => Ok(()),
                        Navigation::External => {
                            args.SetCancel(true)?;
                            event("navigation-external", None);
                            queue_external(uri);
                            Ok(())
                        }
                        Navigation::Deny => {
                            args.SetCancel(true)?;
                            event("navigation-denied", None);
                            Ok(())
                        }
                    }
                });
                fail_closed(&c, main, outcome)
            })),
            &mut token,
        )?;
        guards.navigation = true;

        let c = controller.clone();
        let cfg = config.clone();
        core.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                let outcome = args_or_pointer(args).and_then(|args| {
                    // Never an app window (main.cjs:713-716): deny first, then decide.
                    args.SetHandled(true)?;
                    let uri = read_pwstr(|p| args.Uri(p))?;
                    event("popup-denied", Some(&uri));
                    if cfg.popup(&uri) == Navigation::External {
                        queue_external(uri);
                    }
                    Ok(())
                });
                fail_closed(&c, main, outcome)
            })),
            &mut token,
        )?;
        guards.new_window = true;

        let c = controller.clone();
        let cfg = config.clone();
        core.add_PermissionRequested(
            &PermissionRequestedEventHandler::create(Box::new(move |sender, args| {
                let outcome = args_or_pointer(args).and_then(|args| {
                    let sender = args_or_pointer(sender)?;
                    let top = read_pwstr(|p| sender.Source(p))?;
                    let requester = read_pwstr(|p| args.Uri(p))?;
                    let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                    args.PermissionKind(&mut kind)?;
                    let permission = permission_kind(kind);
                    let decision = cfg.shell_permission(&top, &requester, permission);
                    // The handler stays authoritative on every request (no profile memory).
                    args.cast::<ICoreWebView2PermissionRequestedEventArgs3>()?
                        .SetSavesInProfile(false)?;
                    args.SetState(match decision {
                        PermissionDecision::Allow => COREWEBVIEW2_PERMISSION_STATE_ALLOW,
                        PermissionDecision::Deny => COREWEBVIEW2_PERMISSION_STATE_DENY,
                    })?;
                    event("permission", Some(&format!("{permission:?} {decision:?}")));
                    Ok(())
                });
                fail_closed(&c, main, outcome)
            })),
            &mut token,
        )?;
        guards.permissions = true;

        let c = controller.clone();
        let cfg = config.clone();
        core.cast::<ICoreWebView2_27>()?.add_ScreenCaptureStarting(
            &ScreenCaptureStartingEventHandler::create(Box::new(move |_, args| {
                let outcome = args_or_pointer(args).and_then(|args| match cfg.screen_capture() {
                    CaptureDecision::NativeCancel => {
                        args.SetCancel(true)?;
                        event("screen-capture-cancelled", None);
                        Ok(())
                    }
                });
                fail_closed(&c, main, outcome)
            })),
            &mut token,
        )?;
        guards.screen_capture = true;

        // Bridge v1 attribution. DOCUMENTED: ContentLoading fires before any content, including
        // AddScriptToExecuteOnDocumentCreated scripts, and FrameId is valid from the first
        // ContentLoading (0 = invalid). A missing id disables only this document (fail closed).
        let c = controller.clone();
        let d = dispatcher.clone();
        core.add_ContentLoading(
            &ContentLoadingEventHandler::create(Box::new(move |sender, _| {
                let outcome = args_or_pointer(sender).and_then(|sender| {
                    let mut id = 0;
                    let frame = sender
                        .cast::<ICoreWebView2_20>()
                        .and_then(|s| s.FrameId(&mut id));
                    let out = {
                        let mut d = d.try_borrow_mut().map_err(reentered)?;
                        let mut out = d.begin_document();
                        out.extend(match frame {
                            Ok(()) => d.set_main_frame(id),
                            Err(_) => d.disable_document("bridge-frame-id-unavailable"),
                        });
                        out
                    };
                    deliver(&sender, out)
                });
                fail_closed(&c, main, outcome)
            })),
            &mut token,
        )?;

        // DOCUMENTED: ICoreWebView2's WebMessageReceived is raised for the top-level document;
        // iframe messages would need ICoreWebView2Frame's event, never subscribed. Attribution is
        // Source + host-owned frame id/generation, checked by the dispatcher before parsing. No
        // RefCell borrow is held across a COM call.
        let c = controller.clone();
        let d = dispatcher.clone();
        core.add_WebMessageReceived(
            &WebMessageReceivedEventHandler::create(Box::new(move |sender, args| {
                let outcome = args_or_pointer(args).and_then(|args| {
                    let sender = args_or_pointer(sender)?;
                    let source = read_pwstr(|p| args.Source(p))?;
                    let json = read_pwstr(|p| args.WebMessageAsJson(p))?;
                    let now = now_ms();
                    let out = {
                        let mut d = d.try_borrow_mut().map_err(reentered)?;
                        let attr = d.main_frame_request(&source);
                        d.receive(&attr, &json, now)
                    };
                    deliver(&sender, out)
                });
                fail_closed(&c, main, outcome)
            })),
            &mut token,
        )?;
        guards.web_message = true;

        core.add_DocumentTitleChanged(
            &DocumentTitleChangedEventHandler::create(Box::new(move |sender, _| {
                let title = args_or_pointer(sender)
                    .and_then(|s| read_pwstr(|p| s.DocumentTitle(p)))
                    .map(|t| window_title(&t))
                    .unwrap_or_else(|_| MAIN_WINDOW.title.to_owned());
                event("title", Some(&title));
                if SetWindowTextW(main, &HSTRING::from(title)).is_err() {
                    event("title-update-failed", None);
                }
                Ok(())
            })),
            &mut token,
        )?;

        controller.add_AcceleratorKeyPressed(
            &AcceleratorKeyPressedEventHandler::create(Box::new(move |sender, args| {
                let (Some(sender), Some(args)) = (sender, args) else {
                    return Ok(());
                };
                let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                let mut key = 0;
                args.KeyEventKind(&mut kind)?;
                args.VirtualKey(&mut key)?;
                let input = KeyInput {
                    virtual_key: key,
                    key_down: kind == COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                        || kind == COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN,
                    control: GetKeyState(i32::from(VK_CONTROL.0)) < 0,
                };
                if accelerator(input) == KeyAction::Reload {
                    args.SetHandled(true)?;
                    sender.CoreWebView2()?.Reload()?;
                    event("reload", None);
                }
                Ok(())
            })),
            &mut token,
        )?;

        core.add_ProcessFailed(
            &ProcessFailedEventHandler::create(Box::new(move |_, args| {
                // Open Question E.2 default: log a fixed code, keep the window (Electron's
                // dead-window behavior). No automatic restart/reload is invented here.
                let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                if let Some(args) = args {
                    let _ = args.ProcessFailedKind(&mut kind);
                }
                event("process-failed", Some(&kind.0.to_string()));
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(guards)
}

/// A failed security decision closes the WebView (aborting if even that fails) and the app.
fn fail_closed(
    controller: &ICoreWebView2Controller,
    main: HWND,
    outcome: Result<()>,
) -> Result<()> {
    let result = super::close_on_error(controller, outcome);
    if result.is_err() {
        event("security-handler-failed", None);
        // SAFETY: posting to our own window; WM_CLOSE destroys it and quits the loop.
        let _ = unsafe { PostMessageW(Some(main), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
    result
}

// ---------- entry point ----------

/// Removes this thread's pending quit request, if any. A failed startup destroys the main
/// window (WM_DESTROY → PostQuitMessage) before the error is reported, and a pending WM_QUIT
/// ends any modal loop, including MessageBox's, before the user can read it (review F1).
/// PeekMessage always retrieves WM_QUIT regardless of the filter; the filter keeps every other
/// posted message queued. Returns whether a quit request was removed.
fn discard_pending_quit() -> bool {
    let mut removed = false;
    let mut message = MSG::default();
    for _ in 0..8 {
        // SAFETY: plain queue query on the calling thread.
        if !unsafe { PeekMessageW(&mut message, None, WM_QUIT, WM_QUIT, PM_REMOVE) }.as_bool() {
            break;
        }
        removed |= message.message == WM_QUIT;
    }
    removed
}

/// Debug-only check that no WM_QUIT is pending (PM_NOREMOVE leaves the queue unchanged).
fn quit_pending() -> bool {
    let mut message = MSG::default();
    // SAFETY: plain queue query on the calling thread.
    unsafe { PeekMessageW(&mut message, None, WM_QUIT, WM_QUIT, PM_NOREMOVE) }.as_bool()
}

/// Reports a genuine startup failure after all teardown (WebView closed, windows destroyed,
/// COM uninitialized). The test host records `fatal-dialog <code>` on exactly this path
/// instead of showing modal UI; release shows the dialog.
fn report_startup_failure(test_host: bool, code: &str) {
    event(code, None);
    if discard_pending_quit() {
        event("fatal-quit-discarded", None);
    }
    debug_assert!(
        !quit_pending(),
        "WM_QUIT would dismiss the startup error dialog"
    );
    event("fatal-dialog", Some(code));
    if !test_host {
        // SAFETY: modal dialog with constant strings; the main window is already destroyed.
        unsafe {
            MessageBoxW(
                None,
                w!(
                    "O Nimbo não conseguiu abrir. Verifique se o Microsoft Edge WebView2 Runtime está instalado e tente novamente."
                ),
                w!("Nimbo"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: balances the successful CoInitializeEx on this thread.
        unsafe { CoUninitialize() }
    }
}

pub fn run(args: &[OsString]) -> ExitCode {
    // SAFETY: process-wide identity/DPI calls before any window exists.
    unsafe {
        if SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(crate::policy::APP_ID)).is_err() {
            event("app-user-model-id-failed", None);
        }
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_err() {
            event("dpi-awareness-unchanged", None);
        }
    }
    let launch = match launch(args) {
        Ok(launch) => launch,
        Err(code) => {
            event(code, None);
            return ExitCode::from(64);
        }
    };
    let _ = LINK_SCHEME.set(launch.scheme);
    #[cfg(feature = "test-host")]
    if let Some(op) = launch.scheme_op {
        return test_host_scheme(&launch, op);
    }
    let lock = match acquire_instance(&launch.identity) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            forward_focus(&launch.identity, launch.link.as_ref());
            return ExitCode::SUCCESS;
        }
        Err(_) => {
            report_startup_failure(launch.test_host, "single-instance-failed");
            return ExitCode::FAILURE;
        }
    };
    if launch.link.is_some() {
        // Cold start through a link: normal start, the link is dropped (no slot exists yet).
        event("link-cold-start", None);
    }
    #[cfg(not(feature = "test-host"))]
    register_scheme();
    let result = run_window(&launch);
    drop(lock);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) if failure.reports_error() => {
            report_startup_failure(launch.test_host, failure.code());
            ExitCode::from(failure.exit_status())
        }
        Err(failure) => {
            // The user closed the window while WebView2 was starting: quiet quit, like
            // Electron (main.cjs:730). No dialog; the host adds no message pumping, and a late
            // WebView2 completion (e.g. dispatched during COM teardown) is dropped, not a panic.
            event(failure.code(), None);
            ExitCode::from(failure.exit_status())
        }
    }
}

/// Release first instance only: conservative per-user registration; never fatal, login does
/// not depend on it.
#[cfg(not(feature = "test-host"))]
fn register_scheme() {
    if let Some(code) = crate::scheme::native::register_release() {
        event(code, None);
    }
}

/// `--test-host-scheme register|unregister`: only this directory's isolated key, no window.
#[cfg(feature = "test-host")]
fn test_host_scheme(launch: &Launch, op: crate::shell::SchemeOp) -> ExitCode {
    use crate::scheme::native;
    let url = launch.config.start_url();
    let (code, status) = match op {
        crate::shell::SchemeOp::Register => {
            match native::register_test_host(launch.scheme, url, &launch.dir) {
                Ok(code) => (code, 0),
                Err(code) => (Some(code), 65),
            }
        }
        crate::shell::SchemeOp::Unregister => {
            let code = native::unregister_test_host(launch.scheme, url, &launch.dir);
            (
                Some(code),
                if code == "scheme-unregistered" { 0 } else { 65 },
            )
        }
    };
    if let Some(code) = code {
        event(code, None);
    }
    ExitCode::from(status)
}

fn run_window(launch: &Launch) -> std::result::Result<(), StartupFailure> {
    use StartupFailure::Fatal;
    // SAFETY: STA required by WebView2; balanced by `Apartment`.
    if unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_err() {
        return Err(Fatal("com-init-failed"));
    }
    let _apartment = Apartment;
    std::fs::create_dir_all(&launch.user_data).map_err(|_| Fatal("user-data-unavailable"))?;
    let (main, message) = create_windows(&launch.identity).map_err(|_| Fatal("window-failed"))?;
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            main,
            controller: None,
            external: Vec::new(),
            config: launch.config.clone(),
            dispatcher: None,
            core: None,
            login: None,
            notify_target: Arc::new(Mutex::new(Some(main.0 as isize))),
        })
    });
    let started = start_webview(launch, main);
    let result = match started {
        Ok(()) => {
            message_loop();
            // Bounded supervision of the receiver threads, off the UI message pump (it has
            // ended) and before process exit; recorded, never blocking past the bound.
            if let Some(login) = with_state(|s| s.login.clone()).flatten() {
                let left = login.wait_idle(LOGIN_SHUTDOWN_WAIT);
                event(
                    "shutdown",
                    Some(&format!(
                        "workers={} acceptors={}",
                        left.workers, left.acceptors
                    )),
                );
            }
            Ok(())
        }
        Err(failure) => {
            // Liveness is probed BEFORE our own cleanup: a window already destroyed inside the
            // startup pump was closed by the user (review R1: e.g. the controller completes with
            // E_ABORT inside that DestroyWindow), so the failure settles to a quiet close. Never
            // call DestroyWindow on a stale handle. Otherwise WM_DESTROY closes any controller
            // (its PostQuitMessage is discarded before a dialog by `report_startup_failure`).
            // SAFETY: our own window on its owning thread.
            let alive = unsafe { IsWindow(Some(main)).as_bool() };
            let failure = failure.settle(alive);
            if alive {
                // SAFETY: as above; the window is still alive.
                let _ = unsafe { DestroyWindow(main) };
            }
            Err(failure)
        }
    };
    // SAFETY: our own message-only window.
    let _ = unsafe { DestroyWindow(message) };
    STATE.with(|s| s.borrow_mut().take());
    result
}

fn start_webview(launch: &Launch, main: HWND) -> std::result::Result<(), StartupFailure> {
    use StartupFailure::Fatal;
    let environment = create_environment(&launch.user_data)
        .map_err(|end| StartupFailure::from_wait(end, "webview2-unavailable"))?;
    // A close processed while the environment completion was already queued: never hand a
    // destroyed parent to WebView2 (review R1).
    // SAFETY: our own window on its owning thread.
    if !unsafe { IsWindow(Some(main)) }.as_bool() {
        return Err(StartupFailure::ClosedDuringStartup);
    }
    let controller = create_controller(&environment, main)
        .map_err(|end| StartupFailure::from_wait(end, "webview2-controller-failed"))?;
    with_state(|s| s.controller = Some(controller.clone()));
    // SAFETY: live controller on its UI thread.
    let core = unsafe { controller.CoreWebView2() }.map_err(|_| Fatal("webview2-core-failed"))?;
    let target = with_state(|s| s.notify_target.clone()).ok_or(Fatal("state-missing"))?;
    let login = LoginFlow::new(
        Hooks::new(
            Arc::new(bcrypt_rng),
            login_notifier(target),
            Arc::new(now_ms),
        ),
        launch.scheme,
    );
    let dispatcher = Rc::new(RefCell::new(Dispatcher::new(
        launch.config.clone(),
        login.clone(),
    )));
    with_state(|s| {
        s.dispatcher = Some(dispatcher.clone());
        s.core = Some(core.clone());
        s.login = Some(login);
    });
    let mut guards = install(&controller, &core, &launch.config, &dispatcher, main)
        .map_err(|_| Fatal("guard-install-failed"))?;
    // The bridge script must be registered (completion received) before the first navigation; a
    // close during this wait settles like the CP1 controller wait (review R1).
    add_bridge_script(&core, &bridge::script(&launch.config.app_origin()))
        .map_err(|end| StartupFailure::from_wait(end, "bridge-script-failed"))?;
    guards.bridge_script = true;
    // SAFETY: our own window on its owning thread.
    if !unsafe { IsWindow(Some(main)) }.as_bool() {
        return Err(StartupFailure::ClosedDuringStartup);
    }
    Startup::new(guards)
        .release_navigation()
        .map_err(|_| Fatal("guards-incomplete"))?;
    resize_controller(main);
    // SAFETY: as above; guards were all registered before this first navigation.
    unsafe {
        controller
            .SetIsVisible(true)
            .map_err(|_| Fatal("webview2-visibility-failed"))?;
        core.Navigate(&HSTRING::from(launch.config.start_url()))
            .map_err(|_| Fatal("navigate-failed"))?;
    }
    event("started", None);
    // SAFETY: our own live window on its owning thread; the timer is killed in WM_DESTROY.
    if unsafe { SetTimer(Some(main), LOGIN_TIMER, 1000, None) } == 0 {
        // Fail closed: without the timer an expired login would never settle.
        return Err(Fatal("login-timer-failed"));
    }
    Ok(())
}

fn message_loop() {
    let mut message = MSG::default();
    // SAFETY: standard UI-thread message loop for windows owned by this thread.
    unsafe {
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}
