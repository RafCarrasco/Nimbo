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
    /// The bridge v1 script's AddScriptToExecuteOnDocumentCreated completion succeeded.
    pub bridge_script: bool,
    /// ContentLoading (generation/main frame id) and WebMessageReceived handlers registered.
    pub web_message: bool,
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
        if g.navigation
            && g.new_window
            && g.permissions
            && g.screen_capture
            && g.settings
            && g.bridge_script
            && g.web_message
        {
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
    /// Settles a WebView2 startup failure by the liveness of our own main window, probed
    /// before the host's own cleanup destroys it. Before the message loop only a user close
    /// (DefWindowProc's WM_CLOSE → DestroyWindow) destroys the window; WebView2 then completes
    /// controller creation with E_ABORT inside that DestroyWindow, before the startup pump can
    /// retrieve WM_QUIT (review R1). The error code/HRESULT is deliberately not consulted: an
    /// abort can have other causes, and while the window lives every failure stays fatal.
    pub fn settle(self, window_alive: bool) -> Self {
        match self {
            Self::Fatal(_) if !window_alive => Self::ClosedDuringStartup,
            other => other,
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
    /// A valid release activation link (state only; Debug never shows it).
    Link(crate::login::ActivationLink),
    /// A first argument starting with `golive://` that fails the activation grammar: logged as
    /// the fixed code `link-ignored`, never retained.
    Malformed,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArgSummary {
    pub link: LinkArg,
    /// Arguments ignored (logged only as the fixed code `ignored-argument`).
    pub ignored: usize,
}

fn classify_first(raw: &str) -> LinkArg {
    use crate::login::{ActivationLink, Scheme};
    if !raw
        .get(..9)
        .is_some_and(|p| p.eq_ignore_ascii_case("golive://"))
    {
        return LinkArg::None;
    }
    match ActivationLink::parse(raw, Scheme::Release) {
        Ok(link) => LinkArg::Link(link),
        Err(_) => LinkArg::Malformed,
    }
}

/// Release argv (after the executable): only the FIRST argument may be a `golive://` link;
/// everything else is ignored. No test or URL override exists in release.
pub fn classify_args(args: &[OsString]) -> ArgSummary {
    let mut summary = ArgSummary::default();
    for (i, arg) in args.iter().enumerate() {
        let link = match arg.to_str() {
            Some(raw) if i == 0 => classify_first(raw),
            _ => LinkArg::None,
        };
        if link == LinkArg::None {
            summary.ignored += 1;
        } else {
            summary.link = link;
        }
    }
    summary
}

/// `dwData` of the WM_COPYDATA a second instance sends to the first one's message window.
pub const FOCUS_REQUEST: usize = 0x4E49_4D42;

/// `dwData` of a second instance forwarding an activation link (UTF-16LE in `lpData`).
pub const LINK_REQUEST: usize = 0x4E49_4D4C;
/// 256 UTF-16 units: the largest link payload ever copied.
pub const MAX_LINK_BYTES: u32 = 512;
pub const MIN_LINK_BYTES: u32 = 2;

/// Decision taken from the three WM_COPYDATA header fields alone, BEFORE `lpData` is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyDataPlan {
    Focus,
    /// Read exactly `len` bytes (even, `MIN_LINK_BYTES..=MAX_LINK_BYTES`, non-null pointer).
    CopyLink {
        len: usize,
    },
    Reject,
}
/// Total table over (dwData, cbData, lpData null?). `cbData` is only compared, never
/// transformed, before the decision.
pub fn plan_copydata(data: usize, len: u32, lp_data_is_null: bool) -> CopyDataPlan {
    match data {
        FOCUS_REQUEST if len == 0 => CopyDataPlan::Focus,
        LINK_REQUEST
            if (MIN_LINK_BYTES..=MAX_LINK_BYTES).contains(&len)
                && len.is_multiple_of(2)
                && !lp_data_is_null =>
        {
            CopyDataPlan::CopyLink { len: len as usize }
        }
        _ => CopyDataPlan::Reject,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceRequest {
    Focus,
    /// Debug prints `Link(ActivationLink(..))`: no state.
    Link(crate::login::ActivationLink),
}
/// Applies a plan: `read` (the single bounded copy of the sender's buffer) is called exactly
/// once, and only for `CopyLink`; the bytes must be lossless UTF-16LE without NUL/BOM and a
/// valid link of `scheme`.
pub fn instance_request(
    plan: CopyDataPlan,
    scheme: crate::login::Scheme,
    read: impl FnOnce(usize) -> Option<Vec<u8>>,
) -> Option<InstanceRequest> {
    let len = match plan {
        CopyDataPlan::Focus => return Some(InstanceRequest::Focus),
        CopyDataPlan::Reject => return None,
        CopyDataPlan::CopyLink { len } => len,
    };
    let bytes = read(len).filter(|b| b.len() == len)?;
    // Pairwise little-endian decode: no u16 alignment assumption on the sender's buffer.
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u16::from_le_bytes(*p));
    let text: String = char::decode_utf16(units).collect::<Result<_, _>>().ok()?;
    if text.contains(['\0', '\u{feff}']) {
        return None;
    }
    crate::login::ActivationLink::parse(&text, scheme)
        .ok()
        .map(InstanceRequest::Link)
}

/// FNV-1a 64 over `seed`: stable within this binary, no hashing dependency. Shared by the
/// test-host instance identity and its isolated activation scheme.
pub fn test_host_hash(seed: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in seed {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
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
        let hash = test_host_hash(dir.as_os_str().as_encoded_bytes());
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
    /// `--test-host-link <uri>`: must parse with this directory's isolated scheme.
    pub link: Option<crate::login::ActivationLink>,
    /// `--test-host-scheme register|unregister`: registry operation, then exit (no window).
    pub scheme_op: Option<SchemeOp>,
}
#[cfg(feature = "test-host")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemeOp {
    Register,
    Unregister,
}

#[cfg(feature = "test-host")]
pub fn parse_test_host_args(args: &[OsString]) -> Result<TestHostArgs, &'static str> {
    let mut url = None;
    let mut dir = None;
    let mut link = None;
    let mut op = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or("test-host flag without value")?;
        let slot = match flag.to_str() {
            Some("--test-host-url") => &mut url,
            Some("--test-host-dir") => &mut dir,
            Some("--test-host-link") => &mut link,
            Some("--test-host-scheme") => &mut op,
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
    let scheme = crate::login::Scheme::test_host(&dir);
    let link = match link {
        None => None,
        Some(raw) => Some(
            raw.to_str()
                .and_then(|raw| crate::login::ActivationLink::parse(raw, scheme).ok())
                .ok_or("--test-host-link must be this directory's isolated link")?,
        ),
    };
    let scheme_op = match op.map(|o| o.to_str()) {
        None => None,
        Some(Some("register")) => Some(SchemeOp::Register),
        Some(Some("unregister")) => Some(SchemeOp::Unregister),
        Some(_) => return Err("--test-host-scheme must be register or unregister"),
    };
    Ok(TestHostArgs {
        config: crate::policy::Config::test_host(url)?,
        dir,
        link,
        scheme_op,
    })
}

#[cfg(windows)]
pub mod windows;
