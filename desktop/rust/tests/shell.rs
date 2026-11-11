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
    assert_eq!(classify_args(&os(&[])), ArgSummary::default());
    assert_eq!(
        classify_args(&os(&["golive://auth/complete?state=x"])),
        ArgSummary {
            link: LinkArg::GoliveDeferred,
            ignored: 0
        }
    );
    assert_eq!(
        classify_args(&os(&["GOLIVE://auth", "--flag", "x"])),
        ArgSummary {
            link: LinkArg::GoliveDeferred,
            ignored: 2
        }
    );
    for args in [
        &["https://golive-ad118.web.app/"][..],
        &["--test-host-url", "http://127.0.0.1:1/"],
        &["golive:auth"],
        &["golive://user:pw@auth"],
        &[" golive://auth"],
        &["golive://auth\n"],
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
    assert_eq!(
        instance_request(FOCUS_REQUEST, 0),
        Some(InstanceRequest::Focus)
    );
    for (data, len) in [(FOCUS_REQUEST, 1), (FOCUS_REQUEST + 1, 0), (0, 0)] {
        assert_eq!(instance_request(data, len), None, "{data:x} {len}");
    }
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
