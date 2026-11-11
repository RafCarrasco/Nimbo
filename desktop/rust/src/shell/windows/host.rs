//! Checkpoint-1 Windows host shell: one HWND + WebView2 that opens the hosted Nimbo with the
//! Electron window/navigation/popup/permission/UA/reload/single-instance semantics.
//!
//! NOT in this checkpoint: `nimboDesktop` bridge (none installed, not even a stub), login,
//! deep-link handling or protocol registration, capture/audio/game watch, updater/installer.
//! A debug-only `test-host` build replaces the hosted URL with a loopback fixture, keeps every
//! file under an owned scratch directory and records external opens instead of launching them.
use crate::policy::{
    BROWSER_ARGUMENTS, CaptureDecision, Config, KeyAction, KeyInput, MAIN_WINDOW, Navigation,
    Permission, PermissionDecision, accelerator, user_agent,
};
use crate::shell::{
    AsyncWaitEnd, Guards, InstanceIdentity, InstanceRequest, Rect, Startup, StartupFailure,
    colorref, initial_window_rect, instance_request, min_track_size, window_title,
};
use std::{
    cell::RefCell,
    ffi::OsString,
    path::{Path, PathBuf},
    process::ExitCode,
    rc::Rc,
    sync::mpsc,
};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::*;
use windows::Win32::{
    Foundation::{
        COLORREF, CloseHandle, E_POINTER, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM,
        LRESULT, POINT, RECT, WPARAM,
    },
    Graphics::Gdi::{
        CreateSolidBrush, GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
        UpdateWindow,
    },
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
}

#[cfg(not(feature = "test-host"))]
fn launch(args: &[OsString]) -> std::result::Result<Launch, &'static str> {
    let summary = crate::shell::classify_args(args);
    if summary.link == crate::shell::LinkArg::GoliveDeferred {
        // Recognized syntax only. Deep-link auth is a later, separately approved checkpoint.
        event("deferred-golive-link", None);
    }
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
    // Defense in depth: only https ever leaves the app (main.cjs:714, 720).
    let https = url::Url::parse(url).is_ok_and(|u| u.scheme() == "https");
    if !https {
        event("external-rejected", None);
        return;
    }
    #[cfg(feature = "test-host")]
    event("external", Some(url));
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

/// Second instance: ask the first to focus itself (main.cjs:28-36), then quit.
fn forward_focus(identity: &InstanceIdentity) {
    let class = HSTRING::from(&identity.class);
    for _ in 0..50 {
        // SAFETY: message-only window lookup and a bounded synchronous WM_COPYDATA without
        // payload; the COPYDATASTRUCT outlives the call.
        unsafe {
            if let Ok(target) = FindWindowExW(Some(HWND_MESSAGE), None, &class, PCWSTR::null()) {
                let mut pid = 0;
                GetWindowThreadProcessId(target, Some(&mut pid));
                // Let the first instance take the foreground (as Chromium's singleton does).
                let _ = AllowSetForegroundWindow(pid);
                let data = COPYDATASTRUCT {
                    dwData: crate::shell::FOCUS_REQUEST,
                    cbData: 0,
                    lpData: std::ptr::null_mut(),
                };
                SendMessageTimeoutW(
                    target,
                    WM_COPYDATA,
                    WPARAM(0),
                    LPARAM(&data as *const _ as isize),
                    SMTO_ABORTIFHUNG | SMTO_BLOCK,
                    5000,
                    None,
                );
                event("single-instance-forwarded", None);
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
            WM_DESTROY => {
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
        // SAFETY: for WM_COPYDATA, lparam points to a COPYDATASTRUCT valid during the call.
        let request = unsafe { (lparam.0 as *const COPYDATASTRUCT).as_ref() }
            .and_then(|d| instance_request(d.dwData, d.cbData));
        return match request {
            Some(InstanceRequest::Focus) => {
                focus_main();
                LRESULT(1)
            }
            None => {
                event("instance-message-ignored", None);
                LRESULT(0)
            }
        };
    }
    // SAFETY: default processing of our own window's message.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
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

fn read_pwstr(read: impl FnOnce(&mut PWSTR) -> Result<()>) -> Result<String> {
    let mut value = PWSTR::null();
    read(&mut value)?;
    Ok(take_pwstr(value))
}

fn permission_kind(kind: COREWEBVIEW2_PERMISSION_KIND) -> Permission {
    match kind {
        COREWEBVIEW2_PERMISSION_KIND_CAMERA => Permission::Camera,
        COREWEBVIEW2_PERMISSION_KIND_MICROPHONE => Permission::Microphone,
        COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ => Permission::ClipboardRead,
        _ => Permission::Other,
    }
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
        settings.SetIsWebMessageEnabled(false)?; // no bridge in checkpoint 1
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
    let lock = match acquire_instance(&launch.identity) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            forward_focus(&launch.identity);
            return ExitCode::SUCCESS;
        }
        Err(_) => {
            report_startup_failure(launch.test_host, "single-instance-failed");
            return ExitCode::FAILURE;
        }
    };
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
        })
    });
    let started = start_webview(launch, main);
    let result = match started {
        Ok(()) => {
            message_loop();
            Ok(())
        }
        Err(failure) => {
            // A user close already destroyed the window inside the startup pump; never call
            // DestroyWindow on a stale handle. Otherwise WM_DESTROY closes any controller (its
            // PostQuitMessage is discarded before a dialog by `report_startup_failure`).
            // SAFETY: our own window on its owning thread.
            unsafe {
                if IsWindow(Some(main)).as_bool() {
                    let _ = DestroyWindow(main);
                }
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
    let controller = create_controller(&environment, main)
        .map_err(|end| StartupFailure::from_wait(end, "webview2-controller-failed"))?;
    with_state(|s| s.controller = Some(controller.clone()));
    // SAFETY: live controller on its UI thread.
    let core = unsafe { controller.CoreWebView2() }.map_err(|_| Fatal("webview2-core-failed"))?;
    let guards = install(&controller, &core, &launch.config, main)
        .map_err(|_| Fatal("guard-install-failed"))?;
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
