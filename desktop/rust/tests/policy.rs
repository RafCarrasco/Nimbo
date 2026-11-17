use nimbo_desktop::policy::*;

fn main_frame() -> NativeRequest<'static> {
    NativeRequest {
        window: "main",
        source: HOSTED_URL,
        frame_id: Some(7),
        main_frame_id: Some(7),
        parent_frame_id: None,
        document_generation: 1,
    }
}

#[test]
fn origin_navigation_and_popup_matrix() {
    let c = Config::release();
    for url in [
        HOSTED_URL,
        "https://golive-ad118.web.app:443/room?x=1",
        "https://GOLIVE-AD118.web.app/room",
    ] {
        assert_eq!(c.navigation(url), Navigation::Allow, "{url}");
        assert_eq!(
            c.popup(url),
            Navigation::External,
            "popups never become app windows"
        );
    }
    for url in [
        "https://golive-ad118.web.app.evil.test/",
        "https://example.org/",
    ] {
        assert_eq!(c.navigation(url), Navigation::External);
    }
    for url in [
        "https://golive-ad118.web.app:444/",
        "http://golive-ad118.web.app/",
        "https://user@golive-ad118.web.app/",
        "https://golive-ad118.web.app@evil.test/",
        "javascript:alert(1)",
        "data:text/html,x",
        "blob:https://golive-ad118.web.app/x",
        "file:///c:/",
        "about:blank",
        "garbage",
        " https://golive-ad118.web.app/",
        "https://golive-ad118.web.app/\n",
    ] {
        assert_ne!(c.navigation(url), Navigation::Allow, "{url}");
    }
    assert_eq!(c.popup("https://user@example.org/"), Navigation::Deny);
    assert_eq!(c.popup("http://example.org/"), Navigation::Deny);
}

#[test]
fn native_authorization_requires_host_owned_frame_window_and_generation() {
    let c = Config::release();
    assert!(c.authorize(&main_frame(), 1));
    for request in [
        NativeRequest {
            source: "https://evil.test/",
            ..main_frame()
        },
        NativeRequest {
            window: "picker",
            ..main_frame()
        },
        NativeRequest {
            parent_frame_id: Some(7),
            frame_id: Some(8),
            ..main_frame()
        },
        NativeRequest {
            parent_frame_id: Some(9),
            ..main_frame()
        },
        NativeRequest {
            frame_id: None,
            ..main_frame()
        },
        NativeRequest {
            main_frame_id: None,
            ..main_frame()
        },
        NativeRequest {
            frame_id: Some(0),
            main_frame_id: Some(0),
            ..main_frame()
        },
        NativeRequest {
            document_generation: 0,
            ..main_frame()
        },
        NativeRequest {
            document_generation: 2,
            ..main_frame()
        },
    ] {
        assert!(!c.authorize(&request, 1), "{request:?}");
        assert!(!c.permission(&request, 1, Permission::Camera));
        assert!(!c.command(&request, 1, "nimbo:capture:stop"));
    }
}

#[test]
fn deny_unknown_permissions_and_commands_and_unproven_capture() {
    let c = Config::release();
    let r = main_frame();
    for permission in [
        Permission::Camera,
        Permission::Microphone,
        Permission::Fullscreen,
        Permission::SanitizedClipboardWrite,
    ] {
        assert!(c.permission(&r, 1, permission));
    }
    for permission in [
        Permission::DisplayCapture,
        Permission::ClipboardRead,
        Permission::Other,
    ] {
        assert!(!c.permission(&r, 1, permission));
    }
    // CP2 S1: exactly the bridge v1 allowlist (Electron preload channels + capture:stop).
    for command in BRIDGE_METHOD_NAMES {
        assert!(c.command(&r, 1, command), "{command}");
    }
    for command in [
        "nimbo:capture:start",
        "spawn",
        "eval",
        "read_file",
        "plugin:shell|open",
        "nimbo:signin",
        "nimbo:signIn ",
        "nimbo:capture:stop ",
        "nimbo:update:ready",
        "nimbo:audio:data",
        "",
    ] {
        assert!(!c.command(&r, 1, command), "{command:?}");
    }
}

#[test]
fn bridge_allowlist_and_arg_shapes_are_exact() {
    assert_eq!(
        BRIDGE_METHOD_NAMES,
        [
            "nimbo:signIn",
            "nimbo:signIn:cancel",
            "nimbo:capture:audio",
            "nimbo:audio:start",
            "nimbo:audio:stop",
            "nimbo:update:watch",
            "nimbo:update:install",
            "nimbo:game:start",
            "nimbo:game:stop",
            "nimbo:capture:stop",
        ]
    );
    for name in BRIDGE_METHOD_NAMES {
        let method = BridgeMethod::parse(name).expect(name);
        assert_eq!(method.name(), name);
        let shape = match name {
            "nimbo:capture:audio" => ArgShape::Bool,
            "nimbo:audio:stop" | "nimbo:game:stop" => ArgShape::ShortString,
            _ => ArgShape::Empty,
        };
        assert_eq!(method.args(), shape, "{name}");
    }
    assert_eq!(MAX_STRING_ARG_BYTES, 128);
    assert_eq!(BridgeMethod::parse("NIMBO:SIGNIN"), None);
    // Exhaustive per Feature (no wildcard in policy.rs): a new Feature variant fails to compile
    // until it is classified; every feature method is in the allowlist and none is shared.
    let mut seen = Vec::new();
    for f in [
        Feature::Capture,
        Feature::Audio,
        Feature::GameWatch,
        Feature::Update,
    ] {
        for m in feature_methods(f) {
            assert!(BRIDGE_METHOD_NAMES.contains(&m.name()));
            assert!(!seen.contains(m), "{m:?} classified twice");
            seen.push(*m);
        }
    }
    assert_eq!(seen.len(), 8, "only signIn/cancel are feature-less (login)");
}

// ---- Checkpoint 1 shell policy (Electron desktop/main.cjs parity contract) ----

const VK_F5: u32 = 0x74;
const VK_R: u32 = 0x52;

fn key(virtual_key: u32, control: bool) -> KeyInput {
    KeyInput {
        virtual_key,
        key_down: true,
        control,
    }
}

#[test]
fn accelerators_reload_only_is_recognized() {
    // main.cjs:722-725: keyDown && (F5 || control && key == 'r'), other modifiers ignored.
    for input in [key(VK_F5, false), key(VK_F5, true), key(VK_R, true)] {
        assert_eq!(accelerator(input), KeyAction::Reload, "{input:?}");
    }
    let mut up = key(VK_F5, false);
    up.key_down = false;
    let mut ctrl_r_up = key(VK_R, true);
    ctrl_r_up.key_down = false;
    // F12, I, P, F, F11, +, -, 0 and plain R stay WebView2 default until the parity matrix decides.
    for input in [
        up,
        ctrl_r_up,
        key(VK_R, false),
        key(0x7B, false),
        key(0x49, true),
        key(0x50, true),
        key(0x46, true),
        key(0x7A, false),
        key(0xBB, true),
        key(0xBD, true),
        key(0x30, true),
        key(0x74 + 1, false),
    ] {
        assert_eq!(accelerator(input), KeyAction::Default, "{input:?}");
    }
}

#[test]
fn capture_permission_is_native_cancelled_until_proven() {
    let c = Config::release();
    assert_eq!(
        c.shell_permission(HOSTED_URL, HOSTED_URL, Permission::DisplayCapture),
        PermissionDecision::Deny
    );
    assert_eq!(c.screen_capture(), CaptureDecision::NativeCancel);
    assert!(!Features::current().available(Feature::Capture));
}

#[test]
fn shell_permission_matrix_is_app_origin_only() {
    let c = Config::release();
    let app = "https://golive-ad118.web.app/room";
    // Electron ALLOWED_PERMISSIONS minus display-capture (native picker not proven yet).
    // Plus explicit WebView2 AUTOPLAY (user decision CP1-F3-autoplay: Electron parity).
    for p in [
        Permission::Camera,
        Permission::Microphone,
        Permission::Fullscreen,
        Permission::SanitizedClipboardWrite,
        Permission::Autoplay,
    ] {
        assert_eq!(c.shell_permission(app, app, p), PermissionDecision::Allow);
        for (top, requester) in [
            (app, "https://evil.test/"),
            ("https://evil.test/", app),
            (app, "http://golive-ad118.web.app/"),
            (app, "https://golive-ad118.web.app:444/"),
            (app, "null"),
            (app, ""),
            ("about:blank", app),
        ] {
            assert_eq!(
                c.shell_permission(top, requester, p),
                PermissionDecision::Deny,
                "{top} {requester}"
            );
        }
    }
    for p in [
        Permission::DisplayCapture,
        Permission::ClipboardRead,
        Permission::Other,
    ] {
        assert_eq!(c.shell_permission(app, app, p), PermissionDecision::Deny);
    }
}

// ---- CP1-F3-autoplay (resolved user decision: keep Electron automatic sound, app origin only) ----

#[test]
fn autoplay_is_allowed_only_for_app_top_level_and_app_requester() {
    let c = Config::release();
    let app = "https://golive-ad118.web.app/live/abc";
    for (top, requester) in [
        (HOSTED_URL, HOSTED_URL),
        (app, app),
        (HOSTED_URL, "https://GOLIVE-AD118.web.app:443/x"),
    ] {
        assert_eq!(
            c.shell_permission(top, requester, Permission::Autoplay),
            PermissionDecision::Allow,
            "{top} {requester}"
        );
    }
    let foreign = [
        "https://evil.test/",
        "https://golive-ad118.web.app.evil.test/",
        "https://sub.golive-ad118.web.app/",
        "http://golive-ad118.web.app/",
        "https://golive-ad118.web.app:444/",
        "https://user@golive-ad118.web.app/",
        "wss://golive-ad118.web.app/",
        "blob:https://golive-ad118.web.app/0f0e",
        "data:text/html,x",
        "javascript:play()",
        "file:///c:/x.html",
        "about:blank",
        "about:srcdoc",
        "null",
        "",
        " https://golive-ad118.web.app/",
    ];
    for other in foreign {
        // Foreign/iframe/opaque requester inside the app document.
        assert_eq!(
            c.shell_permission(app, other, Permission::Autoplay),
            PermissionDecision::Deny,
            "requester {other:?}"
        );
        // App-origin requester but a foreign or stale top-level document.
        assert_eq!(
            c.shell_permission(other, app, Permission::Autoplay),
            PermissionDecision::Deny,
            "top {other:?}"
        );
        assert_eq!(
            c.shell_permission(other, other, Permission::Autoplay),
            PermissionDecision::Deny,
            "both {other:?}"
        );
    }
    // The app origin is the configured one: a loopback test origin is not the release origin.
    let t = Config::for_test("http://127.0.0.1:4173/").expect("test config");
    let local = "http://127.0.0.1:4173/";
    assert_eq!(
        t.shell_permission(local, local, Permission::Autoplay),
        PermissionDecision::Allow
    );
    for (top, requester) in [
        (local, "http://localhost:4173/frame"),
        (local, "http://127.0.0.1:4174/"),
        (local, HOSTED_URL),
        (HOSTED_URL, HOSTED_URL),
    ] {
        assert_eq!(
            t.shell_permission(top, requester, Permission::Autoplay),
            PermissionDecision::Deny,
            "{top} {requester}"
        );
    }
}

#[test]
fn autoplay_does_not_widen_other_permissions_or_capture() {
    let c = Config::release();
    for p in [
        Permission::Other,
        Permission::DisplayCapture,
        Permission::ClipboardRead,
    ] {
        assert_eq!(
            c.shell_permission(HOSTED_URL, HOSTED_URL, p),
            PermissionDecision::Deny,
            "{p:?}"
        );
    }
    assert_eq!(c.screen_capture(), CaptureDecision::NativeCancel);
    // Camera/microphone keep their existing app-origin rule; no foreign grant appears.
    for p in [Permission::Camera, Permission::Microphone] {
        assert_eq!(
            c.shell_permission(HOSTED_URL, "https://evil.test/", p),
            PermissionDecision::Deny
        );
    }
    // The native-request (bridge) authorization matrix is unchanged: autoplay is not a command
    // permission there.
    assert!(!c.permission(&main_frame(), 1, Permission::Autoplay));
    // No process-wide relaxation: the Electron switches stay exact, no --autoplay-policy.
    assert!(!BROWSER_ARGUMENTS.contains("autoplay"));
}

#[test]
fn webview2_permission_kind_mapping_is_explicit() {
    use nimbo_desktop::policy::webview2_permission_kind as k;
    // COREWEBVIEW2_PERMISSION_KIND numeric values (WebView2 SDK, pinned webview2-com-sys 0.39.1).
    assert_eq!(
        (k::MICROPHONE, k::CAMERA, k::CLIPBOARD_READ, k::AUTOPLAY),
        (1, 2, 6, 9)
    );
    let named = [
        (k::MICROPHONE, "Microphone"),
        (k::CAMERA, "Camera"),
        (k::CLIPBOARD_READ, "ClipboardRead"),
        (k::AUTOPLAY, "Autoplay"),
    ];
    for (kind, name) in named {
        assert_eq!(format!("{:?}", webview2_permission(kind)), name);
    }
    // UNKNOWN(0), GEOLOCATION(3), NOTIFICATIONS(4), OTHER_SENSORS(5), MULTIPLE_AUTOMATIC_DOWNLOADS(7),
    // FILE_READ_WRITE(8), LOCAL_FONTS(10), MIDI_SYSEX(11), WINDOW_MANAGEMENT(12),
    // PERSISTENT_STORAGE(13) and any future/invalid value stay Other => DENY.
    for kind in [0, 3, 4, 5, 7, 8, 10, 11, 12, 13, 14, -1, i32::MIN, i32::MAX] {
        let p = webview2_permission(kind);
        assert!(matches!(p, Permission::Other), "{kind} -> {p:?}");
        assert_eq!(
            Config::release().shell_permission(HOSTED_URL, HOSTED_URL, p),
            PermissionDecision::Deny
        );
    }
    // Exhaustive (no wildcard): a new Permission variant must be classified here first.
    for p in [
        Permission::Camera,
        Permission::Microphone,
        Permission::Fullscreen,
        Permission::SanitizedClipboardWrite,
        Permission::DisplayCapture,
        Permission::ClipboardRead,
        Permission::Autoplay,
        Permission::Other,
    ] {
        let native_kind = match p {
            Permission::Camera => Some(k::CAMERA),
            Permission::Microphone => Some(k::MICROPHONE),
            Permission::ClipboardRead => Some(k::CLIPBOARD_READ),
            Permission::Autoplay => Some(k::AUTOPLAY),
            // Not WebView2 PermissionRequested kinds (fullscreen/clipboard-write/display
            // capture use other WebView2 surfaces); Other is the deny bucket.
            Permission::Fullscreen
            | Permission::SanitizedClipboardWrite
            | Permission::DisplayCapture
            | Permission::Other => None,
        };
        if let Some(kind) = native_kind {
            assert_eq!(format!("{:?}", webview2_permission(kind)), format!("{p:?}"));
        }
    }
}

#[test]
fn host_maps_webview2_kinds_through_the_portable_table() {
    // SOURCE WIRING check only (not runtime proof): host.rs delegates to the portable mapping and
    // const-asserts every distinguished binding value, so a renamed/renumbered kind fails to build.
    let src: String = include_str!("../src/shell/windows/host.rs")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(
        src.contains("webview2_permission(kind.0)"),
        "host must use the portable mapping"
    );
    for (binding, portable) in [
        ("COREWEBVIEW2_PERMISSION_KIND_MICROPHONE.0", "MICROPHONE"),
        ("COREWEBVIEW2_PERMISSION_KIND_CAMERA.0", "CAMERA"),
        (
            "COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ.0",
            "CLIPBOARD_READ",
        ),
        ("COREWEBVIEW2_PERMISSION_KIND_AUTOPLAY.0", "AUTOPLAY"),
    ] {
        assert!(
            src.contains(&format!(
                "assert!({binding}==webview2_permission_kind::{portable})"
            )),
            "missing const assertion for {binding}"
        );
    }
    assert!(
        !src.contains("_=>Permission::Other"),
        "no host-local wildcard mapping"
    );
    assert!(!src.contains("autoplay-policy"));
    // The decision is still written with SavesInProfile(false) and an explicit ALLOW/DENY.
    for needle in [
        "SetSavesInProfile(false)",
        "PermissionDecision::Allow=>COREWEBVIEW2_PERMISSION_STATE_ALLOW",
        "PermissionDecision::Deny=>COREWEBVIEW2_PERMISSION_STATE_DENY",
    ] {
        assert!(src.contains(needle), "{needle}");
    }
}

#[test]
fn features_default_unavailable() {
    let features = Features::current();
    for f in [
        Feature::Capture,
        Feature::Audio,
        Feature::GameWatch,
        Feature::Update,
    ] {
        assert!(!features.available(f), "{f:?}");
    }
    // CP2 S1 installs the bridge v1 surface; every media/update feature stays unavailable.
    const { assert!(BRIDGE_INSTALLED) };
}

#[test]
fn user_agent_carries_nimbo_version_token_once() {
    let base = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36 Edg/140.0.0.0";
    let ua = user_agent(base);
    assert_eq!(ua, format!("{base} Nimbo/{}", env!("CARGO_PKG_VERSION")));
    // User-approved first Rust version (distribution decision D1); hosted package stays unchanged.
    assert_eq!(APP_VERSION, "0.5.0");
    assert!(ua.ends_with(" Nimbo/0.5.0"));
    assert_eq!(user_agent(&ua), ua, "idempotent");
}

#[test]
fn browser_arguments_are_the_electron_switches() {
    // main.cjs:24-27; backgroundThrottling:false is covered by the same switches.
    assert_eq!(
        BROWSER_ARGUMENTS,
        "--disable-renderer-backgrounding --disable-background-timer-throttling --disable-backgrounding-occluded-windows --disable-features=CalculateNativeWinOcclusion,IntensiveWakeUpThrottling"
    );
}
