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
    // Only resource cancellation is modeled at this checkpoint, not a real IPC dispatcher.
    assert!(c.command(&r, 1, "nimbo:capture:stop"));
    for command in [
        "nimbo:capture:start",
        "spawn",
        "eval",
        "read_file",
        "plugin:shell|open",
        "nimbo:signIn",
        "nimbo:capture:stop ",
    ] {
        assert!(!c.command(&r, 1, command));
    }
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
    for p in [
        Permission::Camera,
        Permission::Microphone,
        Permission::Fullscreen,
        Permission::SanitizedClipboardWrite,
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
    // No nimboDesktop bridge (not even a stub returning errors) is installed in checkpoint 1.
    const { assert!(!BRIDGE_INSTALLED) };
}

#[test]
fn user_agent_carries_nimbo_version_token_once() {
    let base = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36 Edg/140.0.0.0";
    let ua = user_agent(base);
    assert_eq!(ua, format!("{base} Nimbo/{}", env!("CARGO_PKG_VERSION")));
    assert_eq!(APP_VERSION, "0.4.1");
    assert!(ua.ends_with(" Nimbo/0.4.1"));
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
