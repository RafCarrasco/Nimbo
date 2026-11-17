#![cfg(windows)]
//! Native target checks. Execution is deferred, not waived.
//! No test grants capture or writes registry/profile data. Do not print other applications' titles.
use nimbo_desktop::sources::windows::{enumerate_monitors, enumerate_processes, enumerate_windows};
use std::collections::HashSet;
use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::*};
use windows::core::w;

#[test]
#[ignore = "native catalog provider; run on interactive Windows at final validation"]
fn native_provider_builds_a_catalog_with_real_screen_entries() {
    use nimbo_desktop::sources::{Catalog, windows::Win32Metadata};
    let catalog = Catalog::collect(&Win32Metadata, &[std::process::id()])
        .expect("native catalog collection failed");
    let list = catalog.list();
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|s| s["type"] == "screen")
    );
}

#[test]
fn process_snapshot_contains_the_current_process_without_an_exe_suffix() -> windows::core::Result<()>
{
    let rows = enumerate_processes()?;
    let own = rows
        .iter()
        .find(|p| p.pid == std::process::id())
        .expect("native snapshot must include current process");
    assert!(!own.name.is_empty());
    assert!(!own.name.to_ascii_lowercase().ends_with(".exe"));
    Ok(())
}

#[test]
#[ignore = "requires interactive Windows desktop; execute at final native validation"]
fn monitors_have_real_unique_handles_and_valid_rectangles() -> windows::core::Result<()> {
    let rows = enumerate_monitors()?;
    assert!(
        !rows.is_empty(),
        "interactive desktop must expose a monitor"
    );
    assert_eq!(
        rows.iter().map(|m| &m.handle).collect::<HashSet<_>>().len(),
        rows.len()
    );
    assert!(rows.iter().any(|m| m.primary));
    for monitor in rows {
        assert!(!monitor.device_name.is_empty());
        assert!(monitor.bounds.covers(monitor.work_area));
    }
    Ok(())
}

#[test]
#[ignore = "native metadata handle accounting; run serially at final Windows validation"]
fn repeated_metadata_queries_do_not_leak_owned_process_or_snapshot_handles()
-> windows::core::Result<()> {
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
    let _ = enumerate_processes()?;
    let _ = enumerate_windows(&[std::process::id()])?;
    let mut before = 0;
    // SAFETY: pseudo-handle is queried, never closed; output counters are initialized.
    unsafe {
        GetProcessHandleCount(GetCurrentProcess(), &mut before)?;
    }
    for _ in 0..3 {
        let _ = enumerate_processes()?;
        let _ = enumerate_windows(&[std::process::id()])?;
    }
    let mut after = 0;
    unsafe {
        GetProcessHandleCount(GetCurrentProcess(), &mut after)?;
    }
    assert_eq!(
        after, before,
        "native metadata queries leaked owned handles"
    );
    Ok(())
}

#[test]
#[ignore = "creates an owned GUI fixture on interactive Windows; no screenshot/capture grant"]
fn owned_window_metadata_is_found_then_excluded_by_its_native_pid() -> windows::core::Result<()> {
    struct Window(HWND);
    impl Drop for Window {
        fn drop(&mut self) {
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }
    // SAFETY: predefined STATIC class; this test owns the HWND for its entire scope.
    let window = Window(unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!("Nimbo enum fixture 東京 🎮"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            40,
            40,
            480,
            240,
            None,
            None,
            None,
            None,
        )?
    });
    let key = (window.0.0 as isize).to_string();
    let rows = enumerate_windows(&[])?;
    let own = rows
        .iter()
        .find(|w| w.hwnd == key)
        .expect("owned fixture must be enumerated");
    assert_eq!(own.pid, std::process::id());
    assert_eq!(own.title, "Nimbo enum fixture 東京 🎮");
    assert!(own.visible && !own.minimized);
    assert!(!own.class_name.is_empty());
    assert!(
        !own.path.is_empty(),
        "limited self-process metadata query must work"
    );
    let excluded = enumerate_windows(&[std::process::id()])?;
    assert!(excluded.iter().all(|w| w.pid != std::process::id()));
    Ok(())
}
