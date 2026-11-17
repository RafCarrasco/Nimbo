#![cfg(windows)]
//! Interactive component fixture: real WebView2 attribution/DOM, synthetic empty metadata only.
//! This does not exercise WGC, real source enumeration, hosted auth, or full application integration.
use nimbo_desktop::{
    picker::{
        WireMethod,
        windows::{NativePickerSurface, OwnerDocument},
    },
    policy::Config,
};
use std::{
    cell::Cell,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};
use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows::{
    Win32::{
        Foundation::{E_POINTER, HWND, RECT},
        System::WinRT::{RO_INIT_SINGLETHREADED, RoInitialize, RoUninitialize},
        UI::{Shell::SHCreateMemStream, WindowsAndMessaging::*},
    },
    core::{HSTRING, Interface, PCWSTR, PWSTR, w},
};
type TestResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Window(HWND);
impl Drop for Window {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.0) };
    }
}
struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}
fn pump_until(mut done: impl FnMut() -> bool) -> TestResult<()> {
    let until = Instant::now() + Duration::from_secs(15);
    while !done() {
        if Instant::now() >= until {
            return Err("native picker fixture timeout".into());
        }
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}
fn controller(env: &ICoreWebView2Environment, hwnd: HWND) -> TestResult<ICoreWebView2Controller> {
    let (tx, rx) = mpsc::channel();
    let env = env.clone();
    CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            env.CreateCoreWebView2Controller(hwnd, &handler)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(move |result, c| {
            result?;
            let _ = tx.send(c.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER)));
            Ok(())
        }),
    )?;
    Ok(rx.recv()??)
}
fn script(core: &ICoreWebView2, code: &str) -> TestResult<String> {
    let (tx, rx) = mpsc::channel();
    let core = core.clone();
    let code = HSTRING::from(code);
    ExecuteScriptCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            core.ExecuteScript(&code, &handler)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(move |result, value| {
            result?;
            let _ = tx.send(value);
            Ok(())
        }),
    )?;
    Ok(rx.recv()?)
}
#[test]
#[ignore = "interactive Windows/WebView2; NIMBO_PICKER_TEST_DIR must identify an owned scratch directory"]
fn embedded_picker_has_real_native_frame_attribution_and_the_legacy_empty_state() -> TestResult<()>
{
    unsafe {
        RoInitialize(RO_INIT_SINGLETHREADED)?;
    }
    let _apartment = Apartment;
    let root = std::env::var_os("NIMBO_PICKER_TEST_DIR")
        .ok_or("set NIMBO_PICKER_TEST_DIR to an owned scratch directory")?;
    let profile = std::path::PathBuf::from(root).join(format!("picker-{}", std::process::id()));
    std::fs::create_dir(&profile)?;
    let environment = {
        let (tx, rx) = mpsc::channel();
        let path = HSTRING::from(profile.as_os_str());
        CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| unsafe {
                CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), &path, None, &handler)
                    .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |result, env| {
                result?;
                let _ = tx.send(env.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER)));
                Ok(())
            }),
        )?;
        rx.recv()??
    };
    let parent = Window(unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!("Nimbo picker parent fixture"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            0,
            0,
            900,
            700,
            None,
            None,
            None,
            None,
        )?
    });
    let child = Window(unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST,
            w!("STATIC"),
            w!("Nimbo picker fixture — no capture"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            30,
            30,
            880,
            640,
            Some(parent.0),
            None,
            None,
            None,
        )?
    });
    let parent_controller = controller(&environment, parent.0)?;
    let parent_core = unsafe { parent_controller.CoreWebView2()? };
    let parent_url = "http://127.0.0.1:4173/";
    let mut token = 0;
    let env = environment.clone();
    unsafe {
        parent_core.AddWebResourceRequestedFilter(
            &HSTRING::from("http://127.0.0.1:4173/*"),
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
        )?;
        parent_core.add_WebResourceRequested(
            &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                let args = args.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
                let stream = SHCreateMemStream(Some(
                    b"<!doctype html><title>Nimbo owned parent fixture</title>",
                ))
                .ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
                args.SetResponse(&env.CreateWebResourceResponse(
                    &stream,
                    200,
                    w!("OK"),
                    w!("Content-Type: text/html; charset=utf-8"),
                )?)?;
                Ok(())
            })),
            &mut token,
        )?;
    }
    let loaded = Rc::new(Cell::new(false));
    let seen = loaded.clone();
    unsafe {
        parent_core.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(Box::new(move |_, _| {
                seen.set(true);
                Ok(())
            })),
            &mut token,
        )?;
        parent_core.Navigate(&HSTRING::from(parent_url))?;
    }
    pump_until(|| loaded.get())?;
    let owner_core = parent_core.clone();
    let owner = Rc::new(move || {
        let mut source = PWSTR::null();
        let mut frame = 0;
        unsafe {
            owner_core.Source(&mut source).ok()?;
            let source = take_pwstr(source);
            owner_core
                .cast::<ICoreWebView2_20>()
                .ok()?
                .FrameId(&mut frame)
                .ok()?;
            Some(OwnerDocument {
                source,
                frame_id: frame,
                generation: 1,
            })
        }
    });
    let child_controller = controller(&environment, child.0)?;
    unsafe {
        child_controller.SetBounds(RECT {
            left: 0,
            top: 0,
            right: 850,
            bottom: 590,
        })?;
        child_controller.SetIsVisible(true)?;
    }
    let listed = Rc::new(Cell::new(false));
    let saw = listed.clone();
    let failed = Rc::new(Cell::new(false));
    let failure = failed.clone();
    let cfg = Rc::new(Config::for_test(parent_url)?);
    let opener = owner().ok_or("native parent unavailable")?;
    let model = nimbo_desktop::picker::PickerSession::new(&cfg, &opener.request(), 42, false)?;
    let surface = unsafe {
        NativePickerSurface::attach(
            environment.clone(),
            child_controller,
            cfg.clone(),
            owner.clone(),
            model,
            Rc::new(move |surface, call| {
                if matches!(call.method, WireMethod::List) {
                    saw.set(true);
                    surface.reply(
                        call.id,
                        call.generation,
                        Ok(serde_json::json!({"sources":[],"audio":false})),
                    )?;
                }
                Ok(())
            }),
            Rc::new(move || failure.set(true)),
        )?
    };
    pump_until(|| listed.get() || failed.get())?;
    assert!(!failed.get());
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let text = script(surface.core(), "document.body.innerText")?;
        if text.contains("Nenhuma tela encontrada") {
            break;
        }
        assert!(Instant::now() < until, "legacy empty state did not render");
    }
    surface.close();
    // Full native dialog path: real monitor/window/process provider, no WGC session or remote stream.
    let cancelled = Rc::new(Cell::new(false));
    let outcome = cancelled.clone();
    let dialog = unsafe {
        nimbo_desktop::picker::windows::NativePickerDialog::open(
            nimbo_desktop::picker::windows::DialogOptions {
                environment,
                config: cfg,
                owner,
                parent: parent.0,
                audio: false,
                on_done: Box::new(move |choice| {
                    assert!(choice.is_none());
                    outcome.set(true);
                }),
            },
        )?
    };
    pump_until(|| dialog.is_ready() || cancelled.get())?;
    assert!(dialog.is_ready());
    pump_until(|| dialog.has_catalog() || cancelled.get())?;
    assert!(
        dialog.has_catalog(),
        "real native catalog was not delivered"
    );
    let owned_hwnd = dialog.hwnd();
    let mut limits = MINMAXINFO::default();
    unsafe {
        SendMessageW(
            owned_hwnd,
            WM_GETMINMAXINFO,
            None,
            Some(windows::Win32::Foundation::LPARAM(
                (&mut limits as *mut MINMAXINFO) as isize,
            )),
        );
    }
    let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(owned_hwnd) };
    let mut info = windows::Win32::Graphics::Gdi::MONITORINFO {
        cbSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::MONITORINFO>() as u32,
        ..Default::default()
    };
    assert!(
        unsafe {
            windows::Win32::Graphics::Gdi::GetMonitorInfoW(
                windows::Win32::Graphics::Gdi::MonitorFromWindow(
                    owned_hwnd,
                    windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST,
                ),
                &mut info,
            )
        }
        .as_bool()
    );
    let expected = nimbo_desktop::picker::minimum_size(
        nimbo_desktop::shell::Rect {
            left: info.rcWork.left,
            top: info.rcWork.top,
            right: info.rcWork.right,
            bottom: info.rcWork.bottom,
        },
        dpi,
    )?;
    assert_eq!((limits.ptMinTrackSize.x, limits.ptMinTrackSize.y), expected);
    let _cancel_completion = script(
        &dialog.core().ok_or("dialog core unavailable")?,
        "window.capturePicker.cancel(); 'cancel requested'",
    );
    pump_until(|| cancelled.get())?;
    assert_eq!(
        dialog.completion(),
        Some(nimbo_desktop::picker::windows::DialogEnd::CancelledFromPicker)
    );
    assert!(!dialog.is_open());
    assert!(
        !unsafe { IsWindow(Some(owned_hwnd)) }.as_bool(),
        "owned dialog HWND survived cancellation"
    );
    assert_eq!(dialog.metadata_shutdown_succeeded(), Some(true));
    assert!(unsafe { IsWindowEnabled(parent.0) }.as_bool());
    unsafe {
        parent_controller.Close()?;
    }
    // Keep only this owned fixture profile for operator inspection; do not delete live browser data.
    Ok(())
}
