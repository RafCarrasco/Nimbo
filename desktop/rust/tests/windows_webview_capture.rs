#![cfg(windows)]
//! Real WGC -> shared buffer -> canvas MediaStream experiment. NO remote app, login or IPC grant.
//! Compile in ordinary Windows CI; execute explicitly on an interactive desktop, serially.
use nimbo_desktop::{
    frame_route::{
        CANVAS_ADAPTER, FrameAck, FrameRoute,
        windows::{SharedFrameSink, WgcCapture},
    },
    policy::{Config, NativeRequest},
};
use std::{
    cell::Cell,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};
use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::{
    Win32::{
        Foundation::{E_POINTER, HWND, RECT},
        System::WinRT::{RO_INIT_SINGLETHREADED, RoInitialize, RoUninitialize},
        UI::WindowsAndMessaging::*,
    },
    core::{HSTRING, Interface, PCWSTR, PWSTR, w},
};

type TestResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn fail_closed<T>(result: windows::core::Result<T>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => {
            eprintln!("Diagnostic native security handler failed: {error}");
            // Do not return a callback HRESULT that the browser could ignore and proceed.
            std::process::abort();
        }
    }
}
struct Window(HWND);
impl Drop for Window {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.0) };
    }
}
struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() }
    }
}
struct Controller(ICoreWebView2Controller);
impl Drop for Controller {
    fn drop(&mut self) {
        if unsafe { self.0.Close() }.is_err() {
            std::process::abort();
        }
    }
}

fn script(core: &ICoreWebView2, code: &str) -> TestResult<String> {
    let core = core.clone();
    let code = HSTRING::from(code);
    let (tx, rx) = mpsc::channel();
    ExecuteScriptCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            core.ExecuteScript(&code, &handler)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(move |result, output| {
            result?;
            tx.send(output)
                .map_err(|_| windows::core::Error::from_hresult(E_POINTER))?;
            Ok(())
        }),
    )?;
    Ok(rx.recv()?)
}
fn dispatch() {
    // SAFETY: test owns this UI thread and its windows; no foreign process/thread is touched.
    unsafe {
        let mut message = MSG::default();
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

struct Fixture {
    url: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Fixture {
    fn start() -> std::io::Result<Self> {
        use std::io::{Read, Write};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let url = format!("http://127.0.0.1:{}/", listener.local_addr()?.port());
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        let _ = socket.set_read_timeout(Some(Duration::from_millis(100)));
                        let _ = socket.set_write_timeout(Some(Duration::from_millis(100)));
                        let mut request = [0u8; 2048];
                        if socket.read(&mut request).is_ok() {
                            let body = "<!doctype html><meta charset=utf-8><title>Nimbo local native capture test</title>";
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\nContent-Security-Policy: default-src 'none'; media-src blob:; frame-src 'none'; connect-src 'none'\r\nConnection: close\r\n\r\n{}",
                                body.len(),
                                body
                            );
                            let _ = socket.write_all(response.as_bytes());
                        }
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
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
#[ignore = "real interactive Windows WGC + WebView2 media test; run with --ignored --nocapture --test-threads=1"]
fn own_window_reaches_real_webview_video_track() -> TestResult<()> {
    // This fixed loopback document is the ONLY page allowed. No host WebMessageReceived handler
    // exists: test ACKs are read via host ExecuteScript in the known main frame, not page IPC.
    unsafe {
        RoInitialize(RO_INIT_SINGLETHREADED)?;
    }
    let _apartment = Apartment;
    let fixture = Fixture::start()?;
    let config = Config::for_test(&fixture.url)?;
    let profile_root = std::env::var_os("NIMBO_CAPTURE_TEST_DIR")
        .ok_or("set NIMBO_CAPTURE_TEST_DIR to an empty diagnostic scratch directory")?;
    let profile =
        std::path::PathBuf::from(profile_root).join(format!("nimbo-wgc-{}", std::process::id()));
    std::fs::create_dir(&profile)?;
    // Do not use the user's normal WebView profile. Caller retains diagnostic output/profile
    // until the WebView browser process exits; no deleting another process's profile.
    println!("ISOLATED_WEBVIEW_PROFILE {}", profile.display());
    // SAFETY: test owns both native windows until capture/controller shutdown.
    let source = Window(unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!("Nimbo LIVE native capture source"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            30,
            30,
            480,
            240,
            None,
            None,
            None,
            None,
        )?
    });
    let host = Window(unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!("Nimbo native frame receiver diagnostic"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            550,
            30,
            700,
            500,
            None,
            None,
            None,
            None,
        )?
    });
    let environment = {
        let (tx, rx) = mpsc::channel();
        let path = HSTRING::from(profile.as_os_str());
        CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| unsafe {
                CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), &path, None, &handler)
                    .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |result, environment| {
                result?;
                tx.send(environment.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER)))
                    .map_err(|_| windows::core::Error::from_hresult(E_POINTER))?;
                Ok(())
            }),
        )?;
        rx.recv()??
    };
    let controller = {
        let (tx, rx) = mpsc::channel();
        let env = environment.clone();
        let hwnd = host.0;
        CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| unsafe {
                env.CreateCoreWebView2Controller(hwnd, &handler)
                    .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |result, controller| {
                result?;
                tx.send(controller.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER)))
                    .map_err(|_| windows::core::Error::from_hresult(E_POINTER))?;
                Ok(())
            }),
        )?;
        Controller(rx.recv()??)
    };
    let core = unsafe { controller.0.CoreWebView2()? };
    let revoked = Rc::new(Cell::new(false));
    let mut token = 0;
    // All registration failures propagate before navigation. Controller drop closes on failure.
    unsafe {
        controller.0.SetBounds(RECT {
            left: 0,
            top: 0,
            right: 680,
            bottom: 450,
        })?;
        controller.0.SetIsVisible(true)?;
        let first = Rc::new(Cell::new(true));
        let allowed = fixture.url.clone();
        let flag = revoked.clone();
        core.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                let args =
                    fail_closed(args.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER)));
                let mut uri = PWSTR::null();
                fail_closed(args.Uri(&mut uri));
                let uri = take_pwstr(uri);
                if first.replace(false) && uri == allowed {
                    return Ok(());
                }
                flag.set(true);
                fail_closed(args.SetCancel(true));
                Ok(())
            })),
            &mut token,
        )?;
        core.add_FrameNavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(|_, args| {
                fail_closed(
                    args.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))
                        .and_then(|args| args.SetCancel(true)),
                );
                Ok(())
            })),
            &mut token,
        )?;
        core.add_PermissionRequested(
            &PermissionRequestedEventHandler::create(Box::new(|_, args| {
                fail_closed(
                    args.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))
                        .and_then(|args| args.SetState(COREWEBVIEW2_PERMISSION_STATE_DENY)),
                );
                Ok(())
            })),
            &mut token,
        )?;
        core.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(|_, args| {
                fail_closed(
                    args.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))
                        .and_then(|args| args.SetHandled(true)),
                );
                Ok(())
            })),
            &mut token,
        )?;
        core.cast::<ICoreWebView2_27>()?.add_ScreenCaptureStarting(
            &ScreenCaptureStartingEventHandler::create(Box::new(|_, args| {
                fail_closed(
                    args.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))
                        .and_then(|args| args.SetCancel(true)),
                );
                Ok(())
            })),
            &mut token,
        )?;
        core.Navigate(&HSTRING::from(&fixture.url))?;
    }
    let start = Instant::now();
    while script(
        &core,
        &format!(
            "document.readyState === 'complete' && location.href === '{}'",
            fixture.url
        ),
    )? != "true"
    {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "local diagnostic page did not load"
        );
        dispatch();
        std::thread::sleep(Duration::from_millis(20));
    }
    // Actual native main-frame ID, not a page-supplied field. Unknown/zero is denial.
    let mut frame = 0;
    unsafe {
        core.cast::<ICoreWebView2_20>()?.FrameId(&mut frame)?;
    }
    assert_ne!(frame, 0, "WebView2 has no attributable main frame");
    let capture = unsafe { WgcCapture::for_owned_window(source.0)? };
    let layout = capture.layout();
    let request = NativeRequest {
        window: "main",
        source: &fixture.url,
        frame_id: Some(frame),
        main_frame_id: Some(frame),
        parent_frame_id: None,
        document_generation: 1,
    };
    let route = FrameRoute::new(&config, &request, 1, 1, layout)?;
    // Inject the real adapter, then call the same getDisplayMedia API as the unchanged consumer.
    // ACK mailbox is test-owned main-frame JS ONLY; it grants no native command to any page.
    script(
        &core,
        &format!(
            r#"
      window.__acks = []; window.__videoFrames = 0;
      window.__dispose = ({CANVAS_ADAPTER})(chrome.webview,
        {{generation:1,session:1,width:{},height:{}}},
        m => {{ if(m.kind === 'nimbo-frame-ack') window.__acks.push([m.slot,m.sequence]); }});
      navigator.mediaDevices.getDisplayMedia({{video:true,audio:false}}).then(stream => {{
        window.__stream = stream;
        const video = document.createElement('video'); video.muted = true; video.autoplay = true;
        video.srcObject = stream; document.body.append(video);
        const observe = () => {{ window.__videoFrames++; video.requestVideoFrameCallback(observe); }};
        video.requestVideoFrameCallback(observe); video.play().catch(e => window.__failure = String(e));
      }}).catch(e => window.__failure = String(e));
      true
    "#,
            layout.width(),
            layout.height()
        ),
    )?;
    let mut sink =
        unsafe { SharedFrameSink::new(environment.cast()?, core.cast()?, route, capture) };
    let start = Instant::now();
    loop {
        assert!(!revoked.get(), "navigation revoked the capture document");
        let mut uri = PWSTR::null();
        unsafe {
            core.Source(&mut uri)?;
        }
        assert_eq!(take_pwstr(uri), fixture.url, "host source origin changed");
        let mut current_frame = 0;
        unsafe {
            core.cast::<ICoreWebView2_20>()?
                .FrameId(&mut current_frame)?;
        }
        assert_eq!(current_frame, frame);
        // Force real source repaints; static windows need not generate continuous WGC frames.
        unsafe {
            SetWindowTextW(
                source.0,
                &HSTRING::from(format!("Nimbo live source {}", start.elapsed().as_millis())),
            )?;
        }
        sink.pump(&config, &request, start.elapsed().as_millis().try_into()?)?;
        let ack = script(&core, "window.__acks.shift()?.join(':') ?? ''")?;
        if let Some(value) = ack
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .filter(|s| !s.is_empty())
        {
            let (slot, sequence) = value.split_once(':').ok_or("bad ACK")?;
            assert!(sink.acknowledge(
                &config,
                &request,
                FrameAck {
                    session: 1,
                    slot: slot.parse()?,
                    sequence: sequence.parse()?
                },
                start.elapsed().as_millis().try_into()?
            )?);
        }
        if script(
            &core,
            "Boolean(window.__stream instanceof MediaStream && window.__stream.getVideoTracks()[0] instanceof MediaStreamTrack && window.__stream.getVideoTracks()[0].readyState === 'live' && window.__videoFrames >= 3)",
        )? == "true"
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "no real MediaStream/video rendering; failure={}",
            script(&core, "window.__failure ?? null")?
        );
        dispatch();
        std::thread::sleep(Duration::from_millis(34));
    }
    println!(
        "REAL_WEBVIEW_MEDIASTREAM rendered_frames={} native_source_pid={} main_frame={frame}",
        script(&core, "window.__videoFrames")?,
        std::process::id()
    );
    script(
        &core,
        "window.__stream.getTracks().forEach(t => t.stop()); true",
    )?;
    assert_eq!(
        script(
            &core,
            "window.__stream.getVideoTracks()[0].readyState === 'ended'"
        )?,
        "true"
    );
    sink.stop()?;
    assert!(sink.pump(&config, &request, 0).is_err());
    drop(sink); // native producer/mappings closed BEFORE controller/window/apartment drop
    Ok(())
}
