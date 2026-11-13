//! Portable decisions, not claims of installed native hooks or media parity.
use url::Url;

// A release-profile binary can never carry test hooks. build.rs additionally rejects any
// release-derived profile (PROFILE=release) even when debug assertions are forced on.
#[cfg(all(feature = "test-host", not(debug_assertions)))]
compile_error!(
    "NIMBO_TEST_HOST_REQUIRES_DEBUG_ASSERTIONS: the test-host feature is debug-only and must never reach a release binary"
);

pub const APP_ID: &str = "app.nimbo.desktop";
pub const HOSTED_URL: &str = "https://golive-ad118.web.app/";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// desktop/main.cjs:24-27. `backgroundThrottling:false` (706) is covered by the same switches.
pub const BROWSER_ARGUMENTS: &str = "--disable-renderer-backgrounding --disable-background-timer-throttling --disable-backgrounding-occluded-windows --disable-features=CalculateNativeWinOcclusion,IntensiveWakeUpThrottling";
/// CP2 S1 installs the `nimboDesktop` v1 surface (bridge.rs); media/update features stay
/// unavailable through `Features`; signIn uses the S2-core loopback receiver (login.rs).
pub const BRIDGE_INSTALLED: bool = true;

/// Bridge v1 method allowlist: the Electron preload channels plus `nimbo:capture:stop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeMethod {
    SignIn,
    SignInCancel,
    CaptureAudio,
    AudioStart,
    AudioStop,
    UpdateWatch,
    UpdateInstall,
    GameStart,
    GameStop,
    CaptureStop,
}
/// Exact argument shape per method; anything else is malformed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgShape {
    Empty,
    Bool,
    /// One string of at most `MAX_STRING_ARG_BYTES` UTF-8 bytes.
    ShortString,
}
pub const MAX_STRING_ARG_BYTES: usize = 128;
const METHODS: [(&str, BridgeMethod, ArgShape); 10] = [
    ("nimbo:signIn", BridgeMethod::SignIn, ArgShape::Empty),
    (
        "nimbo:signIn:cancel",
        BridgeMethod::SignInCancel,
        ArgShape::Empty,
    ),
    (
        "nimbo:capture:audio",
        BridgeMethod::CaptureAudio,
        ArgShape::Bool,
    ),
    (
        "nimbo:audio:start",
        BridgeMethod::AudioStart,
        ArgShape::Empty,
    ),
    (
        "nimbo:audio:stop",
        BridgeMethod::AudioStop,
        ArgShape::ShortString,
    ),
    (
        "nimbo:update:watch",
        BridgeMethod::UpdateWatch,
        ArgShape::Empty,
    ),
    (
        "nimbo:update:install",
        BridgeMethod::UpdateInstall,
        ArgShape::Empty,
    ),
    ("nimbo:game:start", BridgeMethod::GameStart, ArgShape::Empty),
    (
        "nimbo:game:stop",
        BridgeMethod::GameStop,
        ArgShape::ShortString,
    ),
    (
        "nimbo:capture:stop",
        BridgeMethod::CaptureStop,
        ArgShape::Empty,
    ),
];
pub const BRIDGE_METHOD_NAMES: [&str; 10] = {
    let mut names = [""; 10];
    let mut i = 0;
    while i < METHODS.len() {
        names[i] = METHODS[i].0;
        i += 1;
    }
    names
};
impl BridgeMethod {
    /// Exact, case-sensitive match against the allowlist.
    pub fn parse(name: &str) -> Option<Self> {
        METHODS.iter().find(|m| m.0 == name).map(|m| m.1)
    }
    fn entry(self) -> &'static (&'static str, BridgeMethod, ArgShape) {
        METHODS
            .iter()
            .find(|m| m.1 == self)
            .expect("every variant is listed")
    }
    pub fn name(self) -> &'static str {
        self.entry().0
    }
    pub fn args(self) -> ArgShape {
        self.entry().2
    }
}
/// Methods per media/update feature. Exhaustive on purpose (no wildcard): a new `Feature`
/// variant fails to compile until its bridge methods are classified here.
pub const fn feature_methods(feature: Feature) -> &'static [BridgeMethod] {
    use BridgeMethod as M;
    match feature {
        Feature::Capture => &[M::CaptureAudio, M::CaptureStop],
        Feature::Audio => &[M::AudioStart, M::AudioStop],
        Feature::GameWatch => &[M::GameStart, M::GameStop],
        Feature::Update => &[M::UpdateWatch, M::UpdateInstall],
    }
}

/// Electron puts `Nimbo/x.y.z` in the UA; the page derives `APP_VERSION`/title from it
/// (src/lib/version.ts:6, src/main.tsx:11).
pub fn user_agent(default: &str) -> String {
    let suffix = format!(" Nimbo/{APP_VERSION}");
    if default.ends_with(&suffix) {
        default.to_owned()
    } else {
        format!("{default}{suffix}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInput {
    /// Win32 virtual-key code (layout-mapped, like Electron's `input.key`).
    pub virtual_key: u32,
    /// KeyDown or SystemKeyDown (Electron `keyDown`, auto-repeat included).
    pub key_down: bool,
    pub control: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Host reloads and marks the key handled (avoids a second WebView2 built-in reload).
    Reload,
    /// Leave to WebView2 defaults until the parity matrix (task 1.4) decides otherwise.
    Default,
}
const VK_F5: u32 = 0x74;
const VK_R: u32 = 0x52;
/// desktop/main.cjs:722-725.
pub fn accelerator(k: KeyInput) -> KeyAction {
    if k.key_down && (k.virtual_key == VK_F5 || (k.control && k.virtual_key == VK_R)) {
        KeyAction::Reload
    } else {
        KeyAction::Default
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Capture,
    Audio,
    GameWatch,
    Update,
}
/// Per-feature availability replacing the old process-level "capture not proven" refusal.
/// No constructor can mark a feature available until its checkpoint is proven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Features {
    _private: (),
}
impl Features {
    pub const fn current() -> Self {
        Self { _private: () }
    }
    pub const fn available(&self, feature: Feature) -> bool {
        match feature {
            Feature::Capture | Feature::Audio | Feature::GameWatch | Feature::Update => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureDecision {
    /// Cancel WebView2's own screen-capture UI: getDisplayMedia() rejects.
    NativeCancel,
}

#[derive(Debug, PartialEq, Eq)]
pub struct WindowConfig {
    pub title: &'static str,
    pub width: u32,
    pub height: u32,
    pub min_width: u32,
    pub min_height: u32,
    pub background: [u8; 3],
    pub menu: bool,
}
pub const MAIN_WINDOW: WindowConfig = WindowConfig {
    title: "Nimbo",
    width: 1280,
    height: 800,
    min_width: 850,
    min_height: 590,
    background: [0x0b, 0x10, 0x20],
    menu: false,
};

#[derive(Debug, PartialEq, Eq)]
pub enum Navigation {
    Allow,
    External,
    Deny,
}

#[derive(Debug)]
pub struct Config {
    start: Url,
}

// Reject ambiguous inputs before the WHATWG parser normalizes whitespace/credentials.
fn clean_url(raw: &str) -> Option<Url> {
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) || raw.contains('\\') {
        return None;
    }
    Url::parse(raw)
        .ok()
        .filter(|u| u.has_host() && u.username().is_empty() && u.password().is_none())
}

impl Config {
    pub fn release() -> Self {
        Self {
            start: Url::parse(HOSTED_URL).expect("constant URL"),
        }
    }
    /// Explicit isolated test configuration; the release executable never reads NIMBO_URL.
    pub fn for_test(raw: &str) -> Result<Self, &'static str> {
        let u = clean_url(raw).ok_or("invalid test URL")?;
        if u.scheme() != "http"
            || !matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
            || u.port().is_none()
            || u.path() != "/"
            || u.query().is_some()
            || u.fragment().is_some()
        {
            return Err("test URL requires explicit loopback HTTP port and root path");
        }
        Ok(Self { start: u })
    }
    /// Debug-only isolated host fixture (same loopback rules as `for_test`).
    #[cfg(feature = "test-host")]
    pub fn test_host(raw: &str) -> Result<Self, &'static str> {
        Self::for_test(raw)
    }
    pub fn start_url(&self) -> &str {
        self.start.as_str()
    }
    /// Serialized app origin (`scheme://host[:port]`), substituted into the injected bridge.
    pub fn app_origin(&self) -> String {
        self.start.origin().ascii_serialization()
    }
    pub fn navigation(&self, raw: &str) -> Navigation {
        let Some(u) = clean_url(raw) else {
            return Navigation::Deny;
        };
        if u.origin() == self.start.origin() {
            return Navigation::Allow;
        }
        // A different port/scheme on the app host is not an external-link escape hatch.
        if u.scheme() == "https" && u.host_str() != self.start.host_str() {
            Navigation::External
        } else {
            Navigation::Deny
        }
    }
    /// External opener rule (plan §5.7): https always (main.cjs:714, 720); under the debug-only
    /// `test-host` build ALSO the loopback app origin, where login URLs point. Release builds
    /// cannot reach the loopback branch.
    pub fn external_allowed(&self, raw: &str) -> bool {
        let Some(u) = clean_url(raw) else {
            return false;
        };
        #[cfg(feature = "test-host")]
        if u.origin() == self.start.origin() && self.start.scheme() == "http" {
            return true;
        }
        u.scheme() == "https"
    }
    pub fn popup(&self, raw: &str) -> Navigation {
        if clean_url(raw).is_some_and(|u| u.scheme() == "https") {
            Navigation::External
        } else {
            Navigation::Deny
        }
    }
    pub fn authorize(&self, r: &NativeRequest<'_>, current_generation: u64) -> bool {
        r.window == "main"
            && self.navigation(r.source) == Navigation::Allow
            && r.frame_id.is_some_and(|id| id != 0)
            && r.frame_id == r.main_frame_id
            && r.parent_frame_id.is_none()
            && current_generation != 0
            && r.document_generation == current_generation
    }
    pub fn permission(&self, r: &NativeRequest<'_>, generation: u64, p: Permission) -> bool {
        self.authorize(r, generation)
            && matches!(
                p,
                Permission::Camera
                    | Permission::Microphone
                    | Permission::Fullscreen
                    | Permission::SanitizedClipboardWrite
            )
    }
    pub fn command(&self, r: &NativeRequest<'_>, generation: u64, command: &str) -> bool {
        self.authorize(r, generation) && BridgeMethod::parse(command).is_some()
    }
    /// WebView2 PermissionRequested decision (main.cjs:662-678). Both the current top-level
    /// document and the requesting origin must be the app origin; never a WebView2 prompt.
    /// `display-capture` stays denied until the native picker is proven. `Autoplay` is allowed
    /// under the same app-origin rule (CP1-F3-autoplay); no process-wide autoplay switch.
    pub fn shell_permission(
        &self,
        top_level: &str,
        requester: &str,
        p: Permission,
    ) -> PermissionDecision {
        let allowed_kind = matches!(
            p,
            Permission::Camera
                | Permission::Microphone
                | Permission::Fullscreen
                | Permission::SanitizedClipboardWrite
                | Permission::Autoplay
        );
        if allowed_kind
            && self.navigation(top_level) == Navigation::Allow
            && self.navigation(requester) == Navigation::Allow
        {
            PermissionDecision::Allow
        } else {
            PermissionDecision::Deny
        }
    }
    /// WebView2 ScreenCaptureStarting: always cancelled while `Feature::Capture` is unavailable.
    pub fn screen_capture(&self) -> CaptureDecision {
        CaptureDecision::NativeCancel
    }
}

/// Must be populated by native event/frame ownership, NEVER deserialized from a web message.
/// A source URI alone cannot prove that a request is from the main frame.
#[derive(Debug, Clone)]
pub struct NativeRequest<'a> {
    pub window: &'a str,
    pub source: &'a str,
    pub frame_id: Option<u32>,
    pub main_frame_id: Option<u32>,
    pub parent_frame_id: Option<u32>,
    pub document_generation: u64,
}
#[derive(Debug, Clone, Copy)]
pub enum Permission {
    Camera,
    Microphone,
    Fullscreen,
    SanitizedClipboardWrite,
    DisplayCapture,
    ClipboardRead,
    /// WebView2 AUTOPLAY: `<audio>/<video>` autoplay/`play()` and Web Audio `start()` without a
    /// user gesture. Allowed for the app origin only (user decision CP1-F3-autoplay: Electron's
    /// `autoplayPolicy: no-user-gesture-required` parity for live sound).
    Autoplay,
    Other,
}

/// `COREWEBVIEW2_PERMISSION_KIND` values the shell distinguishes (WebView2 SDK enum; pinned
/// webview2-com-sys 0.39.1 bindings.rs:571-597). The Windows host const-asserts each one
/// against the bindings, so the portable mapping below is the single source of truth.
pub mod webview2_permission_kind {
    pub const MICROPHONE: i32 = 1;
    pub const CAMERA: i32 = 2;
    pub const CLIPBOARD_READ: i32 = 6;
    pub const AUTOPLAY: i32 = 9;
}
/// Every other kind (unknown, geolocation, notifications, sensors, downloads, files, fonts,
/// MIDI, window management, storage, future values) is `Other` and therefore denied.
pub fn webview2_permission(kind: i32) -> Permission {
    use webview2_permission_kind as k;
    match kind {
        k::CAMERA => Permission::Camera,
        k::MICROPHONE => Permission::Microphone,
        k::CLIPBOARD_READ => Permission::ClipboardRead,
        k::AUTOPLAY => Permission::Autoplay,
        _ => Permission::Other,
    }
}
