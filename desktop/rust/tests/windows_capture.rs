#![cfg(windows)]
//! Ignored hardware test captures a real window created and owned by this test process.
//! Ordinary Windows CI compiles it; only an interactive desktop can execute it.
use nimbo_desktop::frame_route::windows::WgcCapture;
use windows::Win32::Foundation::HWND;

#[test]
fn arbitrary_null_window_is_rejected_without_capture() {
    // SAFETY: invalid handle is deliberately passed to exercise validation before WGC activation.
    assert!(unsafe { WgcCapture::for_owned_window(HWND::default()) }.is_err());
}

#[test]
#[ignore = "requires an interactive Windows desktop with WGC/D3D11; no headless success substitute"]
fn own_window_produces_real_changing_pixels_and_closes() -> windows::core::Result<()> {
    use std::time::{Duration, Instant};
    use windows::Win32::System::WinRT::{RO_INIT_SINGLETHREADED, RoInitialize, RoUninitialize};
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { RoUninitialize() }
        }
    }
    struct Window(HWND);
    impl Drop for Window {
        fn drop(&mut self) {
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }
    // SAFETY: test owns COM apartment and HWND, keeps both alive through capture shutdown.
    unsafe {
        RoInitialize(RO_INIT_SINGLETHREADED)?;
        let _apartment = Apartment;
        let window = Window(CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!("Nimbo real WGC frame A"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            40,
            40,
            480,
            240,
            None,
            None,
            None,
            None,
        )?);
        let mut capture = WgcCapture::for_owned_window(window.0)?;
        let first = wait_frame(&mut capture, None)?;
        assert!(first.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        assert!(
            first
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[..3] != first[..3]),
            "real rendered window should not be uniform"
        );
        SetWindowTextW(
            window.0,
            w!("Nimbo changed source pixels BBBBBBBBBBBBBBBBBBBBBBBBBBB"),
        )?;
        let changed = wait_frame(&mut capture, Some(&first))?;
        assert_ne!(first, changed, "source repaint must reach actual readback");
        println!(
            "REAL_WGC_CHANGED_PIXELS {}x{} bytes={}",
            capture.layout().width(),
            capture.layout().height(),
            changed.len()
        );
        capture.stop()?;
        assert!(capture.next_rgba()?.is_none());
    }
    fn wait_frame(
        capture: &mut WgcCapture,
        previous: Option<&[u8]>,
    ) -> windows::core::Result<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // SAFETY: dispatch only this thread's ordinary test-window messages.
            unsafe {
                let mut message = MSG::default();
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            if let Some(pixels) = capture.next_rgba()?
                && previous != Some(pixels.as_slice())
            {
                return Ok(pixels);
            }
            assert!(
                Instant::now() < deadline,
                "no real changing WGC frame within five seconds"
            );
            std::thread::sleep(Duration::from_millis(16));
        }
    }
    Ok(())
}
