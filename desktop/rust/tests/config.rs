use nimbo_desktop::policy::{APP_ID, Config, HOSTED_URL, MAIN_WINDOW, Navigation};

#[test]
fn current_nimbo_identity_and_window_are_not_old_golive() {
    assert_eq!(APP_ID, "app.nimbo.desktop");
    assert_eq!(MAIN_WINDOW.title, "Nimbo");
    assert_eq!((MAIN_WINDOW.width, MAIN_WINDOW.height), (1280, 800));
    assert_eq!((MAIN_WINDOW.min_width, MAIN_WINDOW.min_height), (850, 590));
    assert_eq!(MAIN_WINDOW.background, [0x0b, 0x10, 0x20]);
    const { assert!(!MAIN_WINDOW.menu) };
    assert_eq!(Config::release().start_url(), HOSTED_URL);
    assert_eq!(HOSTED_URL, "https://golive-ad118.web.app/");
}

#[test]
fn loopback_is_explicit_and_never_a_release_override() {
    assert!(Config::for_test("http://127.0.0.1:5173/").is_ok());
    assert!(Config::for_test("http://localhost:5173/").is_ok());
    for url in [
        "http://0.0.0.0:5173/",
        "https://evil.test/",
        "http://localhost.evil:5173/",
        "http://user@localhost:5173/",
        "http://127.0.0.1/",
        "file:///test",
    ] {
        assert!(Config::for_test(url).is_err(), "{url}");
    }
    let local = Config::for_test("http://127.0.0.1:5173/").unwrap();
    assert_eq!(
        local.navigation("http://127.0.0.1:5173/room"),
        Navigation::Allow
    );
    assert_ne!(
        local.navigation("http://127.0.0.1:5174/"),
        Navigation::Allow
    );
    assert_ne!(
        Config::release().navigation(local.start_url()),
        Navigation::Allow
    );
}

#[cfg(feature = "test-host")]
#[test]
fn test_host_config_is_loopback_only_like_for_test() {
    assert!(Config::test_host("http://127.0.0.1:5173/").is_ok());
    for url in [
        "https://golive-ad118.web.app/",
        "http://0.0.0.0:5173/",
        "http://127.0.0.1/",
        "http://127.0.0.1:5173/x",
        "http://user@127.0.0.1:5173/",
        "http://localhost.evil:5173/",
    ] {
        assert!(Config::test_host(url).is_err(), "{url}");
    }
}

/// CP2 S2-core external opener rule (plan §5.7): https always; the loopback app origin ONLY in
/// the debug test-host build (where login URLs target the loopback fixture).
#[test]
fn external_allowed_is_https_only_in_release_and_adds_the_fixture_origin_under_test_host() {
    let release = Config::release();
    for url in [
        "https://golive-ad118.web.app/desktop-login?port=50000&state=x",
        "https://example.org/",
    ] {
        assert!(release.external_allowed(url), "{url}");
    }
    let local = Config::for_test("http://127.0.0.1:4173/").unwrap();
    for url in [
        "http://127.0.0.1:4173/desktop-login?port=50000&state=x",
        "http://127.0.0.1:4174/desktop-login",
        "http://localhost:4173/",
        "file:///C:/x",
        "javascript:alert(1)",
        "nimbo://x",
        "http://example.org/",
        " https://example.org/",
    ] {
        assert!(!release.external_allowed(url), "{url}");
        if !url.starts_with("http://127.0.0.1:4173/") {
            assert!(!local.external_allowed(url), "{url}");
        }
    }
    let fixture = "http://127.0.0.1:4173/desktop-login?port=50000&state=x";
    assert_eq!(
        local.external_allowed(fixture),
        cfg!(feature = "test-host"),
        "loopback branch exists only in test-host builds"
    );
}
