use nimbo_desktop::policy::{Feature, Features};
use nimbo_desktop::shell::{Guards, Startup, StartupError};

#[test]
fn missing_or_failed_native_handlers_never_allow_navigation() {
    // Every incomplete combination of the five required guards refuses navigation.
    for mask in 0u8..31 {
        let bit = |n: u8| mask & (1 << n) != 0;
        let state = Startup::new(Guards {
            navigation: bit(0),
            new_window: bit(1),
            permissions: bit(2),
            screen_capture: bit(3),
            settings: bit(4),
        });
        assert_eq!(
            state.release_navigation(),
            Err(StartupError::MissingNativeGuards),
            "{mask:05b}"
        );
    }
}

#[test]
fn installed_guards_allow_navigation_but_never_capture() {
    // Checkpoint 1 replaces the process-level CaptureNotProven refusal with per-feature flags:
    // the shell may open the site; capture remains natively cancelled and unavailable.
    let state = Startup::new(Guards {
        navigation: true,
        new_window: true,
        permissions: true,
        screen_capture: true,
        settings: true,
    });
    assert_eq!(state.release_navigation(), Ok(()));
    assert!(!Features::current().available(Feature::Capture));
}
