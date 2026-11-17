use nimbo_desktop::sources::{Catalog, DesktopRect, NativeMonitor, NativeProcess, NativeWindow};
use serde_json::json;
fn window(hwnd: &str, pid: u32, title: &str) -> NativeWindow {
    NativeWindow {
        hwnd: hwnd.into(),
        pid,
        title: title.into(),
        path: "C:\\Editor.exe".into(),
        class_name: "Editor".into(),
        visible: true,
        owned: false,
        tool: false,
        cloaked: false,
        minimized: false,
        fullscreen: false,
    }
}
fn monitor() -> NativeMonitor {
    NativeMonitor {
        handle: "42".into(),
        device_name: "DISPLAY1".into(),
        bounds: DesktopRect::new(-1920, 0, 0, 1080).unwrap(),
        work_area: DesktopRect::new(-1920, 0, 0, 1040).unwrap(),
        primary: true,
    }
}
#[test]
fn resolution_accepts_only_an_exact_id_present_in_the_presented_catalog() {
    let c = Catalog::from_native(&[], &[window("11", 2, "Editor")], &[], &[]).unwrap();
    assert_eq!(c.resolve("window:11:0").unwrap().id(), "window:11:0");
    for id in [
        "11",
        "window:12:0",
        "window:011:0",
        "game:2",
        "window:11:0:extra",
    ] {
        assert!(c.resolve(id).is_none());
    }
    let excluded = Catalog::from_native(&[], &[window("11", 2, "Editor")], &[], &[2]).unwrap();
    assert!(excluded.resolve("window:11:0").is_none());
}
#[test]
fn a_reused_window_id_with_a_different_native_pid_is_not_the_same_source() {
    let a = Catalog::from_native(&[], &[window("11", 2, "Old")], &[], &[]).unwrap();
    let changed_title =
        Catalog::from_native(&[], &[window("11", 2, "New title")], &[], &[]).unwrap();
    let reused = Catalog::from_native(&[], &[window("11", 3, "Old")], &[], &[]).unwrap();
    let selected = a.resolve("window:11:0").unwrap();
    assert!(selected.same_source(&changed_title.resolve("window:11:0").unwrap()));
    assert!(!selected.same_source(&reused.resolve("window:11:0").unwrap()));
}
#[test]
fn duplicate_native_source_ids_are_not_silently_overwritten() {
    assert!(Catalog::from_native(&[monitor(), monitor()], &[], &[], &[]).is_err());
}
#[test]
fn provider_failure_never_produces_a_partial_catalog_or_runs_later_queries() {
    use nimbo_desktop::sources::MetadataProvider;
    use std::cell::Cell;
    struct Fixture {
        fail: bool,
        calls: Cell<u32>,
    }
    impl MetadataProvider for Fixture {
        type Error = &'static str;
        fn monitors(&self) -> Result<Vec<NativeMonitor>, Self::Error> {
            self.calls.set(self.calls.get() + 1);
            Ok(vec![monitor()])
        }
        fn windows(&self, _own: &[u32]) -> Result<Vec<NativeWindow>, Self::Error> {
            self.calls.set(self.calls.get() + 1);
            if self.fail {
                Err("fixture API failure")
            } else {
                Ok(vec![window("11", 2, "Editor")])
            }
        }
        fn processes(&self) -> Result<Vec<NativeProcess>, Self::Error> {
            self.calls.set(self.calls.get() + 1);
            Ok(vec![])
        }
    }
    let ok = Fixture {
        fail: false,
        calls: Cell::new(0),
    };
    assert_eq!(
        Catalog::collect(&ok, &[])
            .unwrap()
            .list()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(ok.calls.get(), 3);
    let fail = Fixture {
        fail: true,
        calls: Cell::new(0),
    };
    assert!(Catalog::collect(&fail, &[]).is_err());
    assert_eq!(fail.calls.get(), 2);
}
#[test]
fn native_catalog_combines_screens_and_legacy_rules_without_leaking_own_windows() {
    let c = Catalog::from_native(
        &[monitor()],
        &[window("10", 1, "Nimbo"), window("11", 2, "Editor 東京")],
        &[NativeProcess {
            pid: 8,
            name: "cs2".into(),
        }],
        &[1],
    )
    .unwrap();
    let rows = c.list();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["id"], "screen:42:0");
    assert_eq!(rows[0]["type"], "screen");
    assert_eq!(rows[1]["id"], "window:11:0");
    assert_eq!(rows[1]["name"], "Editor 東京");
    assert_eq!(rows[2]["id"], "game:8");
    assert_eq!(rows[2]["process"], json!(true));
}
