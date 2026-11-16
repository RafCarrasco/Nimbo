//! Read-only Win32 metadata enumeration. Not installed as a web command or a capture grant.
//! Call from the metadata worker. All HWND/HMONITOR strings are descriptive, never capabilities.
//! Geometry follows the calling DPI context; use the host's PER_MONITOR_AWARE_V2 context.
//! This backend never changes process/thread DPI settings on behalf of a caller.
use super::enumeration::{Collector, wide_text};
use super::{DesktopRect, NativeMonitor, NativeProcess, NativeWindow};
use std::mem::size_of;
use windows::Win32::{
    Foundation::{
        CloseHandle, E_FAIL, ERROR_CANCELLED, ERROR_NO_MORE_FILES, HANDLE, HWND, LPARAM, RECT,
        SetLastError,
    },
    Graphics::{
        Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
        Gdi::*,
    },
    System::{Diagnostics::ToolHelp::*, Threading::*},
    UI::WindowsAndMessaging::*,
};
use windows::core::{BOOL, Error, HRESULT, PWSTR, Result};

fn failure(message: &'static str) -> Error {
    Error::new(E_FAIL, message)
}
fn rectangle(rect: RECT) -> Result<DesktopRect> {
    DesktopRect::new(rect.left, rect.top, rect.right, rect.bottom).map_err(failure)
}

// Own only handles returned by OpenProcess/CreateToolhelp32Snapshot, never window/monitor handles.
struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: this wrapper adopts each successful native owned handle exactly once.
        if unsafe { CloseHandle(self.0) }.is_err() {
            eprintln!("source-metadata-handle-close-failed");
        }
    }
}

/// Complete process snapshot or an error; unexpected iteration failures do not return partial rows.
pub fn enumerate_processes() -> Result<Vec<NativeProcess>> {
    // SAFETY: process-only snapshot; no VM reads, module enumeration, injection or elevation.
    let snapshot = OwnedHandle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)? });
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: initialized correctly sized output record, owned live snapshot.
    unsafe {
        Process32FirstW(snapshot.0, &mut entry)?;
    }
    let mut rows = Vec::new();
    loop {
        let name = wide_text(&entry.szExeFile, None).map_err(failure)?;
        let stem = if name.to_ascii_lowercase().ends_with(".exe") {
            &name[..name.len() - 4]
        } else {
            &name
        };
        rows.push(NativeProcess {
            pid: entry.th32ProcessID,
            name: stem.to_owned(),
        });
        // SAFETY: same initialized output record and live snapshot until end of iteration.
        match unsafe { Process32NextW(snapshot.0, &mut entry) } {
            Ok(()) => {}
            Err(e) if e.code() == HRESULT::from_win32(ERROR_NO_MORE_FILES.0) => break,
            Err(e) => return Err(e),
        }
    }
    Ok(rows)
}

unsafe extern "system" fn monitor_callback(
    monitor: HMONITOR,
    _dc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> BOOL {
    if data.0 == 0 {
        return BOOL(0);
    }
    // SAFETY: enumerate_monitors passes a stable stack Collector for this synchronous call.
    let rows = unsafe { &mut *(data.0 as *mut Collector<NativeMonitor>) };
    let read = || -> Result<NativeMonitor> {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: EXW begins with MONITORINFO; full EX size requests the device-name output.
        if !unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
            return Err(failure("source-monitor-info-failed"));
        }
        Ok(NativeMonitor {
            handle: (monitor.0 as isize).to_string(),
            device_name: wide_text(&info.szDevice, None).map_err(failure)?,
            bounds: rectangle(info.monitorInfo.rcMonitor)?,
            work_area: rectangle(info.monitorInfo.rcWork)?,
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        })
    };
    match read() {
        Ok(row) => {
            rows.push(row);
            BOOL(1)
        }
        Err(_) => {
            rows.fail("source-monitor-metadata-failed");
            BOOL(0)
        }
    }
}

pub fn enumerate_monitors() -> Result<Vec<NativeMonitor>> {
    let mut rows = Collector::new();
    // SAFETY: null DC/clip enumerate the virtual desktop. The callback is synchronous and owns no HDC.
    let success = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(monitor_callback),
            LPARAM((&mut rows as *mut Collector<NativeMonitor>) as isize),
        )
    }
    .as_bool();
    rows.finish(success).map_err(failure)
}

fn process_path(pid: u32) -> String {
    // Metadata-only, least query rights. Protected/unavailable paths stay empty like the legacy helper.
    let process = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(handle) => OwnedHandle(handle),
        Err(_) => return String::new(),
    };
    let mut buffer = [0u16; 1024];
    let mut length = buffer.len() as u32;
    // SAFETY: owned live process handle, initialized fixed buffer and bounded output length.
    if unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    }
    .is_err()
    {
        return String::new();
    }
    wide_text(&buffer, Some(length as usize)).unwrap_or_default()
}

fn read_window(hwnd: HWND, excluded: &[u32]) -> Result<Option<NativeWindow>> {
    let mut pid = 0;
    // SAFETY: query APIs accept invalid handles; closed windows/zero owner are skipped.
    if unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) } == 0
        || pid == 0
        || excluded.contains(&pid)
    {
        return Ok(None);
    }
    let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
    let mut title = [0u16; 512];
    // SAFETY: initialized bounded buffer. Excluded PIDs are rejected BEFORE text/path queries.
    let length: usize = unsafe { GetWindowTextW(hwnd, &mut title) }
        .try_into()
        .map_err(|_| failure("source-window-text-failed"))?;
    if length == 0 && !visible {
        return Ok(None);
    }
    let mut class = [0u16; 256];
    let class_length: usize = unsafe { GetClassNameW(hwnd, &mut class) }
        .try_into()
        .map_err(|_| failure("source-window-class-failed"))?;
    let mut cloaked = 0u32;
    // Legacy metadata semantics: unavailable DWM attribute is not interpreted as a capture grant.
    let is_cloaked = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    }
    .is_ok()
        && cloaked != 0;
    let minimized = unsafe { IsIconic(hwnd) }.as_bool();
    let mut window = RECT::default();
    let mut monitor = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let fullscreen = !minimized
        && unsafe { GetWindowRect(hwnd, &mut window) }.is_ok()
        && unsafe {
            GetMonitorInfoW(
                MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            )
        }
        .as_bool()
        && rectangle(window).is_ok_and(|w| rectangle(monitor.rcMonitor).is_ok_and(|m| w.covers(m)));
    let row = NativeWindow {
        hwnd: (hwnd.0 as isize).to_string(),
        pid,
        title: wide_text(&title, Some(length)).map_err(failure)?,
        class_name: wide_text(&class, Some(class_length)).map_err(failure)?,
        path: process_path(pid),
        visible,
        minimized,
        fullscreen,
        owned: unsafe { GetWindow(hwnd, GW_OWNER) }.is_ok(),
        tool: unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } & WS_EX_TOOLWINDOW.0 as i32 != 0,
        cloaked: is_cloaked,
    };
    let mut after_pid = 0;
    // A disappearing/reassigned HWND is not a selectable identity. Full source validation remains later.
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool()
        || unsafe { GetWindowThreadProcessId(hwnd, Some(&mut after_pid)) } == 0
        || after_pid != pid
    {
        return Ok(None);
    }
    Ok(Some(row))
}

struct WindowContext<'a> {
    rows: Collector<NativeWindow>,
    excluded: &'a [u32],
}
unsafe extern "system" fn window_callback(hwnd: HWND, data: LPARAM) -> BOOL {
    if data.0 == 0 {
        return BOOL(0);
    }
    // SAFETY: caller retains this stack context and borrowed PID set throughout synchronous EnumWindows.
    let context = unsafe { &mut *(data.0 as *mut WindowContext<'_>) };
    match read_window(hwnd, context.excluded) {
        Ok(Some(row)) => {
            context.rows.push(row);
            BOOL(1)
        }
        Ok(None) => BOOL(1),
        Err(_) => {
            context.rows.fail("source-window-metadata-failed");
            // EnumWindows documents that an intentionally stopped callback sets its failure code.
            unsafe {
                SetLastError(ERROR_CANCELLED);
            }
            BOOL(0)
        }
    }
}

/// Read-only desktop metadata. Caller supplies the native app/WebView process tree to exclude.
/// No caller-provided HWND, registry edit, focus/restore or capture is performed here.
pub fn enumerate_windows(excluded_pids: &[u32]) -> Result<Vec<NativeWindow>> {
    let mut context = WindowContext {
        rows: Collector::new(),
        excluded: excluded_pids,
    };
    // SAFETY: stable stack context, synchronous API, no retained pointers or global callback state.
    let success = unsafe {
        EnumWindows(
            Some(window_callback),
            LPARAM((&mut context as *mut WindowContext<'_>) as isize),
        )
    }
    .is_ok();
    context.rows.finish(success).map_err(failure)
}
