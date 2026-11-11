//! Portable host-shell decisions (Electron desktop/main.cjs parity). The Windows window,
//! WebView2 and message loop live in `shell::windows`; nothing here claims they ran.
use crate::policy::MAIN_WINDOW;
use std::ffi::OsString;

#[derive(Debug, PartialEq, Eq)]
pub enum StartupError {
    MissingNativeGuards,
}

/// Every native guard that must be registered before the first navigation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Guards {
    pub navigation: bool,
    pub new_window: bool,
    pub permissions: bool,
    pub screen_capture: bool,
    /// Context menu / DevTools / host objects / web messages settings applied.
    pub settings: bool,
}

#[derive(Debug)]
pub struct Startup {
    guards: Guards,
}
impl Startup {
    pub fn new(guards: Guards) -> Self {
        Self { guards }
    }
    /// Navigation is allowed only after all guards are installed. Media stays unavailable
    /// through `policy::Features`, not through a process-level refusal.
    pub fn release_navigation(&self) -> Result<(), StartupError> {
        let g = self.guards;
        if g.navigation && g.new_window && g.permissions && g.screen_capture && g.settings {
            Ok(())
        } else {
            Err(StartupError::MissingNativeGuards)
        }
    }
}

/// How a WebView2 asynchronous startup wait (environment or controller creation, pumped by
/// `webview2_com::wait_with_pump`) ended without a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsyncWaitEnd {
    /// The startup pump retrieved WM_QUIT (`webview2_com::Error::TaskCanceled`): the visible
    /// main window was closed and destroyed before WebView2 finished starting.
    QuitRequested,
    /// The WebView2 call or its completion reported an error.
    Failed,
}

/// Why the host stopped before reaching its message loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupFailure {
    /// A normal user close during startup: quiet exit 0, no dialog (Electron simply quits,
    /// main.cjs:730).
    ClosedDuringStartup,
    /// A genuine failure with its fixed log code: error dialog and exit 1.
    Fatal(&'static str),
}
impl StartupFailure {
    /// Classifies an asynchronous WebView2 wait; `fatal_code` names the failing step.
    pub fn from_wait(end: AsyncWaitEnd, fatal_code: &'static str) -> Self {
        match end {
            AsyncWaitEnd::QuitRequested => Self::ClosedDuringStartup,
            AsyncWaitEnd::Failed => Self::Fatal(fatal_code),
        }
    }
    pub fn code(&self) -> &'static str {
        match self {
            Self::ClosedDuringStartup => "closed-during-startup",
            Self::Fatal(code) => code,
        }
    }
    /// Whether the native error dialog must be shown (never after a user close).
    pub fn reports_error(&self) -> bool {
        matches!(self, Self::Fatal(_))
    }
    pub fn exit_status(&self) -> u8 {
        if self.reports_error() { 1 } else { 0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// DIP to physical pixels (Electron sizes are DIPs). DPI 0 is treated as 96.
pub fn scale(dip: u32, dpi: u32) -> i32 {
    let dpi = if dpi == 0 { 96 } else { dpi };
    let px = (u64::from(dip) * u64::from(dpi) + 48) / 96;
    i32::try_from(px).unwrap_or(i32::MAX)
}

/// 1280x800 outer window centered in the work area (Electron default `center`). If the work
/// area is smaller, the window keeps its size but its origin stays inside the work area.
pub fn initial_window_rect(work: Rect, dpi: u32) -> Rect {
    let w = scale(MAIN_WINDOW.width, dpi);
    let h = scale(MAIN_WINDOW.height, dpi);
    let left = (work.left + (work.right - work.left - w) / 2).max(work.left);
    let top = (work.top + (work.bottom - work.top - h) / 2).max(work.top);
    Rect {
        left,
        top,
        right: left + w,
        bottom: top + h,
    }
}

/// minWidth/minHeight 850x590 DIP (main.cjs:698-699).
pub fn min_track_size(dpi: u32) -> (i32, i32) {
    (
        scale(MAIN_WINDOW.min_width, dpi),
        scale(MAIN_WINDOW.min_height, dpi),
    )
}

/// Native title follows `document.title`; an empty title keeps the "Nimbo" identity.
pub fn window_title(document_title: &str) -> String {
    let title: String = document_title.chars().filter(|&c| c != '\0').collect();
    if title.trim().is_empty() {
        MAIN_WINDOW.title.to_owned()
    } else {
        title
    }
}

/// Win32 COLORREF (0x00BBGGRR) for `#0b1020`.
pub fn colorref(rgb: [u8; 3]) -> u32 {
    u32::from(rgb[0]) | (u32::from(rgb[1]) << 8) | (u32::from(rgb[2]) << 16)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LinkArg {
    #[default]
    None,
    /// A syntactically valid `golive://` first argument. Recognized for a later checkpoint only:
    /// the payload is deliberately NOT retained, acted on or logged in checkpoint 1.
    GoliveDeferred,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArgSummary {
    pub link: LinkArg,
    /// Arguments ignored (logged only as the fixed code `ignored-argument`).
    pub ignored: usize,
}

fn is_golive_link(raw: &str) -> bool {
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) || raw.contains('\\') {
        return false;
    }
    raw.get(..9)
        .is_some_and(|p| p.eq_ignore_ascii_case("golive://"))
        && url::Url::parse(raw).is_ok_and(|u| {
            u.scheme() == "golive"
                && u.has_host()
                && u.username().is_empty()
                && u.password().is_none()
        })
}

/// Release argv (after the executable): only the FIRST argument may be a `golive://` link;
/// everything else is ignored. No test or URL override exists in release.
pub fn classify_args(args: &[OsString]) -> ArgSummary {
    let mut summary = ArgSummary::default();
    for (i, arg) in args.iter().enumerate() {
        if i == 0 && arg.to_str().is_some_and(is_golive_link) {
            summary.link = LinkArg::GoliveDeferred;
        } else {
            summary.ignored += 1;
        }
    }
    summary
}

/// `dwData` of the WM_COPYDATA a second instance sends to the first one's message window.
pub const FOCUS_REQUEST: usize = 0x4E49_4D42;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceRequest {
    Focus,
}
/// Only the exact empty focus request is accepted; anything else is ignored (no payload
/// channel exists in checkpoint 1).
pub fn instance_request(data: usize, len: u32) -> Option<InstanceRequest> {
    (data == FOCUS_REQUEST && len == 0).then_some(InstanceRequest::Focus)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceIdentity {
    pub mutex: String,
    pub class: String,
}
impl InstanceIdentity {
    pub fn release() -> Self {
        Self {
            mutex: format!("Local\\{}", crate::policy::APP_ID),
            class: "NimboDeepLink".to_owned(),
        }
    }
    /// Isolated per test directory so a test host never focuses (or is focused by) a real app.
    #[cfg(feature = "test-host")]
    pub fn test_host(dir: &std::path::Path) -> Self {
        // FNV-1a: stable within this binary, no hashing dependency.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in dir.as_os_str().as_encoded_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self {
            mutex: format!("Local\\{}.test-host.{hash:016x}", crate::policy::APP_ID),
            class: format!("NimboDeepLink.TestHost.{hash:016x}"),
        }
    }
}

/// Debug-only test host launch: loopback fixture URL plus an owned scratch directory that
/// holds the WebView2 profile, host log and external-open recorder.
#[cfg(feature = "test-host")]
#[derive(Debug)]
pub struct TestHostArgs {
    pub config: crate::policy::Config,
    pub dir: std::path::PathBuf,
}

#[cfg(feature = "test-host")]
pub fn parse_test_host_args(args: &[OsString]) -> Result<TestHostArgs, &'static str> {
    let mut url = None;
    let mut dir = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or("test-host flag without value")?;
        let slot = match flag.to_str() {
            Some("--test-host-url") => &mut url,
            Some("--test-host-dir") => &mut dir,
            _ => return Err("unknown test-host argument"),
        };
        if slot.replace(value).is_some() {
            return Err("duplicate test-host flag");
        }
    }
    let url = url
        .and_then(|u| u.to_str())
        .ok_or("--test-host-url required")?;
    let dir = std::path::PathBuf::from(dir.ok_or("--test-host-dir required")?);
    if !dir.is_absolute() {
        return Err("--test-host-dir must be absolute");
    }
    Ok(TestHostArgs {
        config: crate::policy::Config::test_host(url)?,
        dir,
    })
}

#[cfg(windows)]
pub mod windows;
