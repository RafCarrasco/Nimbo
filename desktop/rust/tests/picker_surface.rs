use nimbo_desktop::picker::{PICKER_URL, document_resource, initial_bounds};
use nimbo_desktop::shell::{Rect, scale};
#[test]
fn picker_minimum_size_preserves_legacy_limits_without_exceeding_small_displays() {
    let work = Rect {
        left: -1600,
        top: 0,
        right: 0,
        bottom: 1000,
    };
    for dpi in [96, 144] {
        assert_eq!(
            nimbo_desktop::picker::minimum_size(work, dpi).unwrap(),
            (scale(680, dpi), scale(500, dpi))
        );
    }
    let small = Rect {
        left: 0,
        top: 0,
        right: 600,
        bottom: 400,
    };
    assert_eq!(
        nimbo_desktop::picker::minimum_size(small, 96).unwrap(),
        (small.right - scale(40, 96), small.bottom - scale(40, 96))
    );
    assert!(nimbo_desktop::picker::minimum_size(Rect { right: 0, ..small }, 96).is_err());
}
#[test]
fn embedded_picker_serves_only_the_exact_document_without_mutating_the_legacy_html() {
    let html = document_resource(PICKER_URL, "GET").unwrap();
    assert_eq!(html, include_str!("../../capture-picker.html").as_bytes());
    for (uri, method) in [
        ("https://example.org/", "GET"),
        ("https://nimbo-picker.invalid/private", "GET"),
        (PICKER_URL, "POST"),
        ("https://nimbo-picker.invalid/?x=1", "GET"),
    ] {
        assert!(document_resource(uri, method).is_none());
    }
}
#[test]
fn native_picker_bounds_are_centered_and_clamped_to_the_current_work_area() {
    let work = Rect {
        left: -1920,
        top: -100,
        right: 0,
        bottom: 980,
    };
    let bounds = initial_bounds(work, 96).unwrap();
    assert_eq!(bounds.right - bounds.left, scale(880, 96));
    assert_eq!(bounds.bottom - bounds.top, scale(640, 96));
    assert_eq!((bounds.left - work.left), (work.right - bounds.right));
    assert_eq!((bounds.top - work.top), (work.bottom - bounds.bottom));
    let small = Rect {
        left: 0,
        top: 0,
        right: 800,
        bottom: 600,
    };
    let bounds = initial_bounds(small, 144).unwrap();
    assert!(
        bounds.left >= small.left
            && bounds.top >= small.top
            && bounds.right <= small.right
            && bounds.bottom <= small.bottom
    );
    assert!(
        initial_bounds(
            Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 100
            },
            96
        )
        .is_err()
    );
}
