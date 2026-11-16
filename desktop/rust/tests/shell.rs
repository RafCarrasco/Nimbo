use nimbo_desktop::login::{ActivationLink, Scheme};
use nimbo_desktop::shell::*;
use std::ffi::OsString;

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn geometry_is_electron_dip_scaled_and_centered() {
    let work = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    };
    assert_eq!(
        initial_window_rect(work, 96),
        Rect {
            left: 320,
            top: 120,
            right: 1600,
            bottom: 920
        }
    );
    // 150% scaling: 1920x1200 physical outer size, centered in a 2560x1400 work area.
    let wide = Rect {
        left: -2560,
        top: 0,
        right: 0,
        bottom: 1400,
    };
    assert_eq!(
        initial_window_rect(wide, 144),
        Rect {
            left: -2240,
            top: 100,
            right: -320,
            bottom: 1300
        }
    );
    // Too small a work area: keep the title bar reachable at the work-area origin.
    let small = Rect {
        left: 10,
        top: 20,
        right: 1034,
        bottom: 788,
    };
    let r = initial_window_rect(small, 96);
    assert_eq!((r.left, r.top), (10, 20));
    assert_eq!((r.right - r.left, r.bottom - r.top), (1280, 800));
    assert_eq!(min_track_size(96), (850, 590));
    assert_eq!(min_track_size(120), (1063, 738));
    assert_eq!(min_track_size(192), (1700, 1180));
    assert_eq!(scale(1, 0), 1, "invalid DPI falls back to 96");
}

#[test]
fn title_follows_document_with_identity_fallback() {
    assert_eq!(window_title("Nimbo 0.4.1"), "Nimbo 0.4.1");
    assert_eq!(window_title("Sala 1 · Nimbo"), "Sala 1 · Nimbo");
    assert_eq!(window_title(""), "Nimbo");
    assert_eq!(window_title("   "), "Nimbo");
    assert_eq!(
        window_title("a\0b"),
        "ab",
        "NUL would truncate the native title"
    );
}

#[test]
fn background_is_electron_color_as_colorref() {
    assert_eq!(colorref([0x0b, 0x10, 0x20]), 0x0020_100b);
}

#[test]
fn argv_accepts_only_first_golive_link_and_never_keeps_payload() {
    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";
    let link = format!("golive://auth?state={HEX}");
    let parsed = ActivationLink::parse(&link, Scheme::Release).unwrap();
    assert_eq!(classify_args(&os(&[])), ArgSummary::default());
    assert_eq!(
        classify_args(&os(&["golive://auth/complete?state=x"])),
        ArgSummary {
            link: LinkArg::Malformed,
            ignored: 0
        }
    );
    assert_eq!(
        classify_args(&os(&[&link])),
        ArgSummary {
            link: LinkArg::Link(parsed),
            ignored: 0
        }
    );
    assert_eq!(
        classify_args(&os(&[
            &format!("GOLIVE://auth/?state={HEX}"),
            "--flag",
            "x"
        ])),
        ArgSummary {
            link: LinkArg::Link(parsed),
            ignored: 2
        }
    );
    // Plan §3.3.5: a first argument starting with `golive://` that fails the grammar is
    // Malformed (fixed code `link-ignored`), never retained.
    for malformed in [
        "golive://user:pw@auth",
        "golive://auth\n",
        "GOLIVE://auth",
        "golive://auth?state=0",
        "golive://secret-state",
        "golive://auth?state=0123456789ABCDEF0123456789abcdef0123456789abcdef",
    ] {
        assert_eq!(
            classify_args(&os(&[malformed])).link,
            LinkArg::Malformed,
            "{malformed}"
        );
    }
    // A valid link that is not the FIRST argument is only an ignored argument.
    assert_eq!(
        classify_args(&os(&["x", &link])),
        ArgSummary {
            link: LinkArg::None,
            ignored: 2
        }
    );
    let text = format!("{:?}", classify_args(&os(&[&link])));
    assert!(
        !text.contains(&HEX[..8]),
        "Debug of ArgSummary carries no state"
    );
    for args in [
        &["https://golive-ad118.web.app/"][..],
        &["--test-host-url", "http://127.0.0.1:1/"],
        &["golive:auth"],
        &[" golive://auth"],
        &["--", "golive://auth"],
        &["nimbo://auth"],
        &["C:\\file.txt"],
    ] {
        let summary = classify_args(&os(args));
        assert_eq!(summary.link, LinkArg::None, "{args:?}");
        assert_eq!(summary.ignored, args.len(), "{args:?}");
    }
    // Debug output carries no payload (no credential/state in logs).
    let text = format!("{:?}", classify_args(&os(&["golive://secret-state"])));
    assert!(!text.contains("secret"));
}

#[cfg(unix)]
#[test]
fn non_utf8_argument_is_ignored() {
    use std::os::unix::ffi::OsStringExt;
    let summary = classify_args(&[OsString::from_vec(b"golive://\xff".to_vec())]);
    assert_eq!(
        summary,
        ArgSummary {
            link: LinkArg::None,
            ignored: 1
        }
    );
}

#[test]
fn single_instance_identity_and_focus_message() {
    let id = InstanceIdentity::release();
    assert_eq!(id.mutex, "Local\\app.nimbo.desktop");
    assert_eq!(id.class, "NimboDeepLink");
    assert_eq!(plan_copydata(FOCUS_REQUEST, 0, true), CopyDataPlan::Focus);
    for (data, len) in [(FOCUS_REQUEST, 1), (FOCUS_REQUEST + 1, 0), (0, 0)] {
        assert_eq!(
            plan_copydata(data, len, true),
            CopyDataPlan::Reject,
            "{data:x} {len}"
        );
    }
    assert_ne!(LINK_REQUEST, FOCUS_REQUEST);
}

/// Review F2: a WebView2 startup wait that ends because the startup pump retrieved WM_QUIT
/// (the user closed the visible window) is a normal quit, never a "WebView2 missing" error.
#[test]
fn startup_closed_by_user_is_quiet_success_not_a_webview2_error() {
    let closed = StartupFailure::from_wait(AsyncWaitEnd::QuitRequested, "webview2-unavailable");
    assert_eq!(closed, StartupFailure::ClosedDuringStartup);
    assert_eq!(closed.code(), "closed-during-startup");
    assert!(!closed.reports_error(), "no dialog after a user close");
    assert_eq!(closed.exit_status(), 0);
    assert_eq!(
        StartupFailure::from_wait(AsyncWaitEnd::QuitRequested, "webview2-controller-failed"),
        StartupFailure::ClosedDuringStartup
    );
}

/// Every genuine startup failure keeps its fixed code, reports the error dialog and exits 1.
#[test]
fn genuine_startup_failures_report_and_exit_one() {
    for code in FATAL_CODES {
        let failure = StartupFailure::Fatal(code);
        assert_eq!(
            StartupFailure::from_wait(AsyncWaitEnd::Failed, code),
            failure
        );
        assert_eq!(failure.code(), code);
        assert!(failure.reports_error(), "{code}");
        assert_eq!(failure.exit_status(), 1, "{code}");
    }
}

/// CP2 S1: navigation also requires the awaited bridge script and the web-message handlers.
#[test]
fn navigation_requires_bridge_script_and_web_message_guards() {
    let cp1 = Guards {
        navigation: true,
        new_window: true,
        permissions: true,
        screen_capture: true,
        settings: true,
        ..Guards::default()
    };
    for (bridge_script, web_message) in [(false, false), (true, false), (false, true)] {
        let g = Guards {
            bridge_script,
            web_message,
            ..cp1
        };
        assert_eq!(
            Startup::new(g).release_navigation(),
            Err(StartupError::MissingNativeGuards),
            "{g:?}"
        );
    }
    let all = Guards {
        bridge_script: true,
        web_message: true,
        ..cp1
    };
    assert_eq!(Startup::new(all).release_navigation(), Ok(()));
    let failed = StartupFailure::from_wait(AsyncWaitEnd::QuitRequested, "bridge-script-failed");
    assert_eq!(failed, StartupFailure::ClosedDuringStartup);
}

/// SOURCE-POLICY PROBE: text match on host.rs, NOT native evidence. WebView2 runtime ordering,
/// frame attribution and script injection stay unverified until the Windows operator run.
#[test]
fn host_source_installs_bridge_before_navigation_in_documented_order() {
    let src = include_str!("../src/shell/windows/host.rs");
    let code = |name: &str| {
        let start = src
            .find(&format!("fn {name}("))
            .unwrap_or_else(|| panic!("fn {name} missing"));
        let end = src[start..].find("\n}\n").map_or(src.len(), |e| start + e);
        src[start..end]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .flat_map(str::split_whitespace)
            .collect::<String>()
    };
    let install = code("install");
    let pos = |hay: &str, needle: &str| {
        hay.find(needle)
            .unwrap_or_else(|| panic!("missing {needle}"))
    };
    let host_objects = pos(&install, "SetAreHostObjectsAllowed(false)?");
    let web_message = pos(&install, "SetIsWebMessageEnabled(true)?");
    let settings_done = pos(&install, "guards.settings=true;");
    let loading = pos(&install, "add_ContentLoading(");
    let received = pos(&install, "add_WebMessageReceived(");
    let web_guard = pos(&install, "guards.web_message=true;");
    assert!(host_objects < web_message && web_message < settings_done);
    assert!(settings_done < loading && loading < received && received < web_guard);
    // Code only (the CP1 comment "never AddHostObjectToScript" must not match).
    let all_code: String = src
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .flat_map(str::split_whitespace)
        .collect();
    assert!(!all_code.contains("SetIsWebMessageEnabled(false)"));
    assert!(!all_code.contains("AddHostObjectToScript"));
    assert!(install.contains("ICoreWebView2_20>()") && install.contains("FrameId("));
    assert!(install.contains("\"bridge-frame-id-unavailable\""));

    let script = code("add_bridge_script");
    assert!(script.contains("AddScriptToExecuteOnDocumentCreated("));
    assert!(script.contains("wait_end(webview2_com::wait_with_pump(rx))"));
    assert!(!script.contains("wait_for_async_operation"));

    let start = code("start_webview");
    let installed = pos(&start, "install(");
    let awaited = pos(&start, "add_bridge_script(");
    let classified = pos(
        &start,
        "StartupFailure::from_wait(end,\"bridge-script-failed\")",
    );
    let guard = pos(&start, "guards.bridge_script=true;");
    let release = pos(&start, ".release_navigation()");
    let navigate = pos(&start, "Navigate(");
    assert!(installed < awaited && awaited < classified && classified < guard);
    assert!(guard < release && release < navigate);
}

const FATAL_CODES: [&str; 12] = [
    "bridge-script-failed",
    "webview2-unavailable",
    "webview2-controller-failed",
    "webview2-core-failed",
    "guard-install-failed",
    "guards-incomplete",
    "webview2-visibility-failed",
    "navigate-failed",
    "window-failed",
    "com-init-failed",
    "user-data-unavailable",
    "single-instance-failed",
];

/// Review R1: a user close during controller creation makes WebView2 complete with E_ABORT
/// inside DestroyWindow, before the pump can see WM_QUIT. The failure is therefore settled by
/// the liveness of our own main window, never by the error code/HRESULT: a dead window means
/// the user closed it (quiet exit 0); a live window keeps the genuine error (dialog, exit 1).
#[test]
fn startup_failure_is_settled_by_main_window_liveness_not_by_code() {
    for code in FATAL_CODES {
        let alive = StartupFailure::Fatal(code).settle(true);
        assert_eq!(alive, StartupFailure::Fatal(code), "{code}: live window");
        assert!(alive.reports_error(), "{code}");
        assert_eq!(alive.exit_status(), 1, "{code}");

        let dead = StartupFailure::Fatal(code).settle(false);
        assert_eq!(
            dead,
            StartupFailure::ClosedDuringStartup,
            "{code}: window already destroyed by a user close"
        );
        assert!(
            !dead.reports_error(),
            "{code}: no dialog after a user close"
        );
        assert_eq!(dead.exit_status(), 0, "{code}");
        assert_eq!(dead.code(), "closed-during-startup");
    }
    // Idempotent: a close stays a close, whatever the liveness probe says.
    for alive in [true, false] {
        assert_eq!(
            StartupFailure::ClosedDuringStartup.settle(alive),
            StartupFailure::ClosedDuringStartup
        );
        let once = StartupFailure::Fatal("webview2-controller-failed").settle(alive);
        assert_eq!(once.settle(alive), once);
    }
    // Variant, not string: a code that merely looks like a close stays fatal while the window
    // lives, and an abort-looking code is not special-cased.
    for code in ["closed-during-startup", "E_ABORT", "0x80004004"] {
        assert_eq!(
            StartupFailure::Fatal(code).settle(true),
            StartupFailure::Fatal(code)
        );
    }
}

/// Source-ordering check ONLY (portable, Linux): it does not execute or prove the native
/// WebView2/DestroyWindow behavior. The startup Err branch must settle the failure with the
/// real `IsWindow(main)` result taken BEFORE the host's own cleanup DestroyWindow (after it,
/// every failure would look like a close and be hidden), and a dead window must not reach
/// controller creation.
#[test]
fn host_source_settles_startup_failure_by_liveness_before_own_cleanup() {
    let src = include_str!("../src/shell/windows/host.rs");
    let body = |name: &str| {
        let start = [format!("fn {name}("), format!("fn {name}<")]
            .iter()
            .find_map(|needle| src.find(needle.as_str()))
            .unwrap_or_else(|| panic!("fn {name} missing"));
        let end = src[start..].find("\n}\n").map_or(src.len(), |e| start + e);
        // Code only: comment lines are dropped, then whitespace (rustfmt-insensitive).
        src[start..end]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .flat_map(str::split_whitespace)
            .collect::<String>()
    };
    let run_window = body("run_window");
    let err = run_window
        .find("Err(failure)=>")
        .expect("startup Err branch");
    let branch = &run_window[err..];
    let alive = branch
        .find("letalive=unsafe{IsWindow(Some(main)).as_bool()};")
        .expect("liveness probe of our own main window");
    let settle = branch
        .find("failure.settle(alive)")
        .expect("failure settled by liveness");
    let destroy = branch
        .find("DestroyWindow(main)")
        .expect("own cleanup DestroyWindow");
    assert!(
        alive < settle && alive < destroy,
        "probe before own cleanup"
    );
    assert!(
        branch[..destroy].contains("ifalive{"),
        "cleanup only if alive"
    );
    assert!(
        !run_window.contains("E_ABORT") && !body("wait_end").contains("E_ABORT"),
        "no HRESULT special case"
    );

    let start = body("start_webview");
    let environment = start.find("create_environment(").expect("environment");
    let probe = start
        .find("IsWindow(Some(main))")
        .expect("liveness checked before controller creation");
    let controller = start.find("create_controller(").expect("controller");
    assert!(environment < probe && probe < controller);
}

/// Source-ordering check ONLY (portable, Linux): it does not execute or prove the native
/// MessageBox/WM_QUIT/WebView2 behavior, which stays NOT VERIFIED until the Windows operator run.
#[test]
fn host_source_orders_quit_discard_before_dialog_and_keeps_task_canceled() {
    let src = include_str!("../src/shell/windows/host.rs");
    let body = |name: &str| {
        let start = src
            .find(&format!("fn {name}"))
            .unwrap_or_else(|| panic!("fn {name} missing"));
        let end = src[start..].find("\n}\n").map_or(src.len(), |e| start + e);
        // Whitespace-insensitive, so rustfmt line breaks do not matter.
        src[start..end].split_whitespace().collect::<String>()
    };
    let fatal = body("report_startup_failure");
    let discard = fatal
        .find("discard_pending_quit()")
        .expect("quit discarded");
    let assert = fatal
        .find("debug_assert!(!quit_pending()")
        .expect("debug assert");
    let record = fatal
        .find("\"fatal-dialog\"")
        .expect("recorded dialog event");
    let dialog = fatal.find("MessageBoxW(").expect("dialog");
    assert!(discard < assert && assert < record && record < dialog);
    for name in ["create_environment", "create_controller"] {
        let f = body(name);
        assert!(
            !f.contains("wait_for_async_operation"),
            "{name}: helper panics on late completion"
        );
        assert!(f.contains("wait_with_pump(rx)"), "{name}");
    }
    assert!(
        body("wait_end")
            .contains("Err(webview2_com::Error::TaskCanceled)=>Err(AsyncWaitEnd::QuitRequested)")
    );
}

#[cfg(feature = "test-host")]
#[test]
fn test_host_identity_and_args_are_isolated() {
    use std::path::Path;
    let a = InstanceIdentity::test_host(Path::new("/scratch/a"));
    let b = InstanceIdentity::test_host(Path::new("/scratch/b"));
    let release = InstanceIdentity::release();
    assert_ne!(a.mutex, release.mutex);
    assert_ne!(a.class, release.class);
    assert_ne!(a.mutex, b.mutex);
    assert_eq!(a, InstanceIdentity::test_host(Path::new("/scratch/a")));
    assert!(a.mutex.starts_with("Local\\app.nimbo.desktop.test-host."));

    let abs = if cfg!(windows) {
        "C:\\scratch\\t"
    } else {
        "/scratch/t"
    };
    let ok = parse_test_host_args(&os(&[
        "--test-host-url",
        "http://127.0.0.1:4000/",
        "--test-host-dir",
        abs,
    ]))
    .unwrap();
    assert_eq!(ok.config.start_url(), "http://127.0.0.1:4000/");
    assert_eq!(ok.dir, Path::new(abs));
    assert_eq!((ok.link, ok.scheme_op), (None, None));

    // S3: an isolated link of THIS directory's scheme and the registry operation flags.
    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";
    let scheme = Scheme::test_host(Path::new(abs));
    let iso_link = format!("{}://auth?state={HEX}", scheme.name());
    let base = [
        "--test-host-url",
        "http://127.0.0.1:4000/",
        "--test-host-dir",
        abs,
    ];
    let with = |extra: &[&str]| parse_test_host_args(&os(&[&base[..], extra].concat()));
    let linked = with(&["--test-host-link", &iso_link]).unwrap();
    assert_eq!(
        linked.link,
        Some(ActivationLink::parse(&iso_link, scheme).unwrap())
    );
    assert_eq!(
        with(&["--test-host-scheme", "register"]).unwrap().scheme_op,
        Some(SchemeOp::Register)
    );
    assert_eq!(
        with(&["--test-host-scheme", "unregister"])
            .unwrap()
            .scheme_op,
        Some(SchemeOp::Unregister)
    );
    let other = Scheme::test_host(Path::new("/scratch/other")).name();
    for bad in [
        vec![
            "--test-host-link".to_owned(),
            format!("golive://auth?state={HEX}"),
        ],
        vec![
            "--test-host-link".to_owned(),
            format!("{other}://auth?state={HEX}"),
        ],
        vec![
            "--test-host-link".to_owned(),
            format!("{}://auth?state=0", scheme.name()),
        ],
        vec!["--test-host-scheme".to_owned(), "other".to_owned()],
        vec!["--test-host-scheme".to_owned(), "Register".to_owned()],
        vec![
            "--test-host-link".to_owned(),
            iso_link.clone(),
            "--test-host-link".to_owned(),
            iso_link.clone(),
        ],
    ] {
        let extra: Vec<&str> = bad.iter().map(String::as_str).collect();
        assert!(with(&extra).is_err(), "{extra:?}");
    }
    // The flag order does not matter: the link is checked against the FINAL directory.
    let reordered = parse_test_host_args(&os(&[
        "--test-host-link",
        &iso_link,
        "--test-host-url",
        "http://127.0.0.1:4000/",
        "--test-host-dir",
        abs,
    ]))
    .unwrap();
    assert!(reordered.link.is_some());
    for bad in [
        &[][..],
        &["--test-host-url", "http://127.0.0.1:4000/"],
        &["--test-host-dir", abs],
        &[
            "--test-host-url",
            "https://golive-ad118.web.app/",
            "--test-host-dir",
            abs,
        ],
        &[
            "--test-host-url",
            "http://127.0.0.1:4000/",
            "--test-host-dir",
            "relative",
        ],
        &[
            "--test-host-url",
            "http://127.0.0.1:4000/",
            "--test-host-dir",
            abs,
            "--extra",
        ],
        &[
            "--test-host-url",
            "http://127.0.0.1:4000/",
            "--test-host-url",
            "http://127.0.0.1:4001/",
            "--test-host-dir",
            abs,
        ],
        &[
            "golive://auth",
            "--test-host-url",
            "http://127.0.0.1:4000/",
            "--test-host-dir",
            abs,
        ],
        &[
            "--test-host-url",
            "http://127.0.0.1:4000/",
            "--test-host-dir",
        ],
    ] {
        assert!(parse_test_host_args(&os(bad)).is_err(), "{bad:?}");
    }
}

/// SOURCE-POLICY PROBE: text match on host.rs, NOT native evidence (plan §6.3). The WM_COPYDATA
/// handler decides from the header alone (plan_copydata) before its single bounded payload copy,
/// which lives only inside the reader closure handed to instance_request; link/scheme codes never
/// carry a detail; the release scheme registration runs only outside test-host builds, after the
/// single-instance lock; the slot is consumed outside any `with_state` borrow.
#[test]
fn host_source_plans_copydata_before_any_payload_read_and_never_logs_link() {
    let src = include_str!("../src/shell/windows/host.rs");
    let body = |name: &str| {
        let start = src
            .find(&format!("fn {name}("))
            .unwrap_or_else(|| panic!("fn {name} missing"));
        let end = src[start..].find("\n}\n").map_or(src.len(), |e| start + e);
        src[start..end]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .flat_map(str::split_whitespace)
            .collect::<String>()
    };
    let flat: String = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .flat_map(str::split_whitespace)
        .collect();
    // instance_proc delegates WM_COPYDATA to copydata().
    assert!(body("instance_proc").contains("ifmsg==WM_COPYDATA{returncopydata(lparam);}"));
    let cd = body("copydata");
    let null = cd.find("iflparam.0==0{").expect("null lparam guard");
    let header = cd.find("read_unaligned(").expect("header read by value");
    let plan = cd.find("plan_copydata(").expect("plan from the header");
    let request = cd.find("instance_request(plan,").expect("plan applied");
    let raw = cd.find("from_raw_parts(").expect("single bounded copy");
    assert!(null < header && header < plan && plan < request && request < raw);
    assert_eq!(cd.matches("from_raw_parts(").count(), 1);
    assert_eq!(
        flat.matches("from_raw_parts(").count(),
        1,
        "no other payload read"
    );
    assert!(
        cd[request..].starts_with("instance_request(plan,scheme,|len|"),
        "read inside the reader closure"
    );
    assert!(
        cd[raw..].starts_with("from_raw_parts(source,len)"),
        "length from the plan"
    );
    assert!(
        !cd[..plan].contains("lpData.") && !cd[..plan].contains("cbData/"),
        "nothing derived before the plan"
    );
    // Link/scheme codes never carry a detail.
    for prefix in [
        "event(\"link-",
        "event(\"scheme-",
        "event(\"single-instance-forwarded",
    ] {
        for (i, _) in flat.match_indices(prefix) {
            let call = &flat[i..i + flat[i..].find(')').unwrap() + 1];
            assert!(call.ends_with(",None)"), "{call}");
        }
    }
    let activate = body("activate_link");
    assert!(activate.contains("event(code,None);"));
    // forward_focus sends the link request only with the owned buffer.
    let forward = body("forward_focus");
    assert!(forward.contains("LINK_REQUEST") && forward.contains("FOCUS_REQUEST"));
    assert!(forward.contains("\"single-instance-forwarded-link\""));
    assert!(
        forward.find("AllowSetForegroundWindow(").unwrap()
            < forward.find("SendMessageTimeoutW(").unwrap()
    );
    // Registration: release only, after the single-instance lock, before the window.
    let run = body("run");
    let lock = run.find("acquire_instance(").expect("lock");
    let register = run
        .find("#[cfg(not(feature=\"test-host\"))]register_scheme();")
        .expect("release-only registration call");
    let window = run.find("run_window(").expect("window");
    assert!(lock < register && register < window);
    assert!(run[lock..register].contains("forward_focus(&launch.identity,launch.link.as_ref())"));
    assert_eq!(flat.matches("register_release(").count(), 1);
    assert!(body("register_scheme").contains("event(code,None)"));
    assert!(!flat.contains("deferred-golive-link"));
    // The slot is consumed after a single short borrow cloned the flow, never inside it.
    let borrow = activate
        .find("letlogin=with_state(|s|s.login.clone()).flatten();")
        .expect("clone-then-release borrow");
    let consume = activate.find(".activate(link,now_ms())").expect("activate");
    assert!(borrow < consume);
    assert_eq!(activate.matches("with_state(").count(), 1);
    assert!(
        body("copydata")
            .find("activate_link(&link);focus_main();")
            .is_some()
    );
}
