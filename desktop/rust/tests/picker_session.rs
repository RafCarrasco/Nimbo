use nimbo_desktop::{
    picker::{PICKER_URL, PickerRequest, PickerSession},
    policy::{Config, NativeRequest},
};
fn main_request() -> NativeRequest<'static> {
    NativeRequest {
        window: "main",
        source: "https://golive-ad118.web.app/",
        frame_id: Some(5),
        main_frame_id: Some(5),
        parent_frame_id: None,
        document_generation: 9,
    }
}
fn request() -> PickerRequest<'static> {
    PickerRequest {
        window: 123,
        source: PICKER_URL,
        frame_id: 7,
        parent_frame_id: None,
        document_generation: 1,
    }
}
fn catalog(name: &str) -> nimbo_desktop::sources::Catalog {
    catalog_pid(name, 2)
}
fn catalog_pid(name: &str, pid: u32) -> nimbo_desktop::sources::Catalog {
    use nimbo_desktop::sources::{Catalog, NativeWindow};
    Catalog::from_native(
        &[],
        &[NativeWindow {
            hwnd: "11".into(),
            pid,
            title: name.into(),
            path: "C:\\Editor.exe".into(),
            class_name: "Editor".into(),
            visible: true,
            owned: false,
            tool: false,
            cloaked: false,
            minimized: false,
            fullscreen: false,
        }],
        &[],
        &[],
    )
    .unwrap()
}
#[test]
fn superseded_refreshes_cannot_replace_newer_or_reloaded_catalogs() {
    let cfg = Config::release();
    let main = main_request();
    let mut s = PickerSession::new(&cfg, &main, 123, true).unwrap();
    s.bind_document(7).unwrap();
    let old = s.begin_refresh(&cfg, &main, &request()).unwrap();
    let current = s.begin_refresh(&cfg, &main, &request()).unwrap();
    assert!(!s.finish_refresh(old, Ok(catalog("Old"))).unwrap());
    assert!(s.finish_refresh(current, Ok(catalog("Current"))).unwrap());
    assert_eq!(
        s.list(&cfg, &main, &request()).unwrap()["sources"][0]["name"],
        "Current"
    );
    assert_eq!(s.list(&cfg, &main, &request()).unwrap()["audio"], true);
    let pending = s.begin_refresh(&cfg, &main, &request()).unwrap();
    s.bind_document(8).unwrap();
    assert!(!s.finish_refresh(pending, Ok(catalog("Late"))).unwrap());
    let mut reloaded = request();
    reloaded.frame_id = 8;
    reloaded.document_generation = 2;
    assert!(s.list(&cfg, &main, &reloaded).is_err());
}
#[test]
fn an_enumeration_failure_revokes_previously_presented_sources() {
    let cfg = Config::release();
    let main = main_request();
    let mut s = PickerSession::new(&cfg, &main, 123, false).unwrap();
    s.bind_document(7).unwrap();
    let t = s.begin_refresh(&cfg, &main, &request()).unwrap();
    s.finish_refresh(t, Ok(catalog("Old"))).unwrap();
    let t = s.begin_refresh(&cfg, &main, &request()).unwrap();
    assert!(
        s.finish_refresh(t, Err("native-enumeration-failed"))
            .is_err()
    );
    assert!(s.list(&cfg, &main, &request()).is_err());
}
#[test]
fn failed_native_frame_registration_revokes_the_previous_document() {
    let cfg = Config::release();
    let main = main_request();
    let mut s = PickerSession::new(&cfg, &main, 123, false).unwrap();
    s.bind_document(7).unwrap();
    assert!(s.authorized(&cfg, &main, &request()));
    assert!(s.bind_document(0).is_err());
    assert!(!s.authorized(&cfg, &main, &request()));
}
// Characterization of revocation boundaries; these tests are not claimed as pre-implementation RED.
#[test]
fn removed_or_failed_catalogs_never_revive_retired_tokens_or_accept_native_id_aliases() {
    for fail in [false, true] {
        let cfg = Config::release();
        let main = main_request();
        let mut s = PickerSession::new(&cfg, &main, 123, false).unwrap();
        s.bind_document(7).unwrap();
        let t = s.begin_refresh(&cfg, &main, &request()).unwrap();
        s.finish_refresh(t, Ok(catalog("Editor"))).unwrap();
        let old = s.list(&cfg, &main, &request()).unwrap()["sources"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let t = s.begin_refresh(&cfg, &main, &request()).unwrap();
        if fail {
            assert!(s.finish_refresh(t, Err("fixture-failure")).is_err());
        } else {
            s.finish_refresh(
                t,
                Ok(nimbo_desktop::sources::Catalog::from_native(&[], &[], &[], &[]).unwrap()),
            )
            .unwrap();
        }
        let t = s.begin_refresh(&cfg, &main, &request()).unwrap();
        s.finish_refresh(t, Ok(catalog("Editor"))).unwrap();
        let current = s.list(&cfg, &main, &request()).unwrap()["sources"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(old, current);
        assert!(current.len() <= 128);
        for forged in [
            old,
            "window:11:0".into(),
            format!("{current}:extra"),
            format!("0{current}"),
        ] {
            assert!(
                s.choose(&cfg, &main, &request(), &forged, false, &catalog("Editor"))
                    .is_err()
            );
        }
        assert_eq!(
            s.choose(&cfg, &main, &request(), &current, false, &catalog("Editor"))
                .unwrap()
                .candidate()
                .id(),
            "window:11:0"
        );
    }
}
#[test]
fn a_new_picker_document_cannot_reuse_old_document_presentation_tokens() {
    let cfg = Config::release();
    let main = main_request();
    let mut s = PickerSession::new(&cfg, &main, 123, false).unwrap();
    s.bind_document(7).unwrap();
    let t = s.begin_refresh(&cfg, &main, &request()).unwrap();
    s.finish_refresh(t, Ok(catalog("Editor"))).unwrap();
    let old = s.list(&cfg, &main, &request()).unwrap()["sources"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut current_request = request();
    current_request.frame_id = 8;
    current_request.document_generation = s.bind_document(8).unwrap();
    let t = s.begin_refresh(&cfg, &main, &current_request).unwrap();
    s.finish_refresh(t, Ok(catalog("Editor"))).unwrap();
    let current = s.list(&cfg, &main, &current_request).unwrap()["sources"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(old, current);
    assert!(
        s.choose(
            &cfg,
            &main,
            &current_request,
            &old,
            false,
            &catalog("Editor")
        )
        .is_err()
    );
    assert!(
        s.choose(
            &cfg,
            &main,
            &current_request,
            &current,
            false,
            &catalog("Editor")
        )
        .is_ok()
    );
}
#[test]
fn unchanged_sources_keep_their_presented_token_when_only_the_title_changes() {
    let cfg = Config::release();
    let main = main_request();
    let mut s = PickerSession::new(&cfg, &main, 123, false).unwrap();
    s.bind_document(7).unwrap();
    let t = s.begin_refresh(&cfg, &main, &request()).unwrap();
    s.finish_refresh(t, Ok(catalog("Old title"))).unwrap();
    let old = s.list(&cfg, &main, &request()).unwrap()["sources"][0]["id"].clone();
    let t = s.begin_refresh(&cfg, &main, &request()).unwrap();
    s.finish_refresh(t, Ok(catalog("New title"))).unwrap();
    let rows = s.list(&cfg, &main, &request()).unwrap();
    assert_eq!(rows["sources"][0]["name"], "New title");
    assert_eq!(
        rows["sources"][0]["id"], old,
        "periodic refresh must not reset the user's selected unchanged source"
    );
}
#[test]
fn stale_ui_item_cannot_select_a_reassigned_source_after_a_background_refresh() {
    let cfg = Config::release();
    let main = main_request();
    let mut s = PickerSession::new(&cfg, &main, 123, false).unwrap();
    s.bind_document(7).unwrap();
    let t = s.begin_refresh(&cfg, &main, &request()).unwrap();
    s.finish_refresh(t, Ok(catalog_pid("Old editor", 2)))
        .unwrap();
    let old_id = s.list(&cfg, &main, &request()).unwrap()["sources"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let t = s.begin_refresh(&cfg, &main, &request()).unwrap();
    s.finish_refresh(t, Ok(catalog_pid("New editor", 3)))
        .unwrap();
    // Native refresh is complete, but the renderer may still send its old clicked card.
    assert!(
        s.choose(
            &cfg,
            &main,
            &request(),
            &old_id,
            false,
            &catalog_pid("New editor", 3)
        )
        .is_err(),
        "stale card selected the newly reassigned native window"
    );
    let current_id = s.list(&cfg, &main, &request()).unwrap()["sources"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(old_id, current_id);
    assert!(
        s.choose(
            &cfg,
            &main,
            &request(),
            &current_id,
            false,
            &catalog_pid("New editor", 3)
        )
        .is_ok()
    );
}
#[test]
fn returned_choice_stays_bound_to_its_original_main_document() {
    let cfg = Config::release();
    let main = main_request();
    let mut s = PickerSession::new(&cfg, &main, 123, true).unwrap();
    s.bind_document(7).unwrap();
    let t = s.begin_refresh(&cfg, &main, &request()).unwrap();
    s.finish_refresh(t, Ok(catalog("Editor"))).unwrap();
    let wire_id = s.list(&cfg, &main, &request()).unwrap()["sources"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let choice = s
        .choose(&cfg, &main, &request(), &wire_id, true, &catalog("Editor"))
        .unwrap();
    assert!(choice.authorized(&cfg, &main));
    let mut changed = main_request();
    changed.document_generation += 1;
    assert!(
        !choice.authorized(&cfg, &changed),
        "returned selection must not survive main reload"
    );
    let mut changed = main_request();
    changed.frame_id = Some(6);
    changed.main_frame_id = Some(6);
    assert!(
        !choice.authorized(&cfg, &changed),
        "same generation on another native main frame is not the owner"
    );
    let mut changed = main_request();
    changed.parent_frame_id = Some(1);
    assert!(!choice.authorized(&cfg, &changed));
    let mut changed = main_request();
    changed.source = "https://example.org/";
    assert!(!choice.authorized(&cfg, &changed));
    let mut changed = main_request();
    changed.window = "oauth";
    assert!(!choice.authorized(&cfg, &changed));
}
#[test]
fn choosing_a_presented_id_consumes_the_picker_exactly_once() {
    let cfg = Config::release();
    let main = main_request();
    let mut s = PickerSession::new(&cfg, &main, 123, false).unwrap();
    s.bind_document(7).unwrap();
    let t = s.begin_refresh(&cfg, &main, &request()).unwrap();
    s.finish_refresh(t, Ok(catalog("Editor"))).unwrap();
    let wire_id = s.list(&cfg, &main, &request()).unwrap()["sources"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        s.choose(
            &cfg,
            &main,
            &request(),
            "window:12:0",
            true,
            &catalog("Editor")
        )
        .is_err()
    );
    let chosen = s
        .choose(&cfg, &main, &request(), &wire_id, true, &catalog("Editor"))
        .unwrap();
    assert_eq!(chosen.candidate().id(), "window:11:0");
    assert!(chosen.audio());
    assert!(s.list(&cfg, &main, &request()).is_err());
    assert!(
        s.choose(&cfg, &main, &request(), &wire_id, true, &catalog("Editor"))
            .is_err()
    );
}
#[test]
fn a_source_reassigned_since_presentation_cannot_be_chosen() {
    use nimbo_desktop::sources::{Catalog, NativeWindow};
    let cfg = Config::release();
    let main = main_request();
    let mut s = PickerSession::new(&cfg, &main, 123, false).unwrap();
    s.bind_document(7).unwrap();
    let t = s.begin_refresh(&cfg, &main, &request()).unwrap();
    s.finish_refresh(t, Ok(catalog("Editor"))).unwrap();
    let wire_id = s.list(&cfg, &main, &request()).unwrap()["sources"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let fresh = Catalog::from_native(
        &[],
        &[NativeWindow {
            hwnd: "11".into(),
            pid: 3,
            title: "Editor".into(),
            path: "C:\\Editor.exe".into(),
            class_name: "Editor".into(),
            visible: true,
            owned: false,
            tool: false,
            cloaked: false,
            minimized: false,
            fullscreen: false,
        }],
        &[],
        &[],
    )
    .unwrap();
    assert!(
        s.choose(&cfg, &main, &request(), &wire_id, false, &fresh)
            .is_err()
    );
    assert!(s.authorized(&cfg, &main, &request()));
    assert!(
        s.choose(&cfg, &main, &request(), &wire_id, false, &catalog("Editor"))
            .is_ok()
    );
}
#[test]
fn a_terminal_choice_cannot_be_reopened_by_a_late_document_callback() {
    let cfg = Config::release();
    let main = main_request();
    let mut s = PickerSession::new(&cfg, &main, 123, false).unwrap();
    s.bind_document(7).unwrap();
    let t = s.begin_refresh(&cfg, &main, &request()).unwrap();
    s.finish_refresh(t, Ok(catalog("Editor"))).unwrap();
    let wire_id = s.list(&cfg, &main, &request()).unwrap()["sources"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    s.choose(&cfg, &main, &request(), &wire_id, false, &catalog("Editor"))
        .unwrap();
    assert!(s.bind_document(8).is_err());
}
#[test]
fn cancel_requires_native_attribution_and_invalidates_pending_work() {
    let cfg = Config::release();
    let main = main_request();
    let mut s = PickerSession::new(&cfg, &main, 123, false).unwrap();
    s.bind_document(7).unwrap();
    let t = s.begin_refresh(&cfg, &main, &request()).unwrap();
    let mut iframe = request();
    iframe.parent_frame_id = Some(7);
    assert!(s.cancel(&cfg, &main, &iframe).is_err());
    assert!(s.authorized(&cfg, &main, &request()));
    s.cancel(&cfg, &main, &request()).unwrap();
    assert!(!s.finish_refresh(t, Ok(catalog("Late"))).unwrap());
    assert!(s.bind_document(8).is_err());
    assert!(!s.authorized(&cfg, &main, &request()));
}
#[test]
fn only_the_bound_picker_and_unchanged_native_opener_are_authorized() {
    let cfg = Config::release();
    let main = main_request();
    let mut session = PickerSession::new(&cfg, &main, 123, false).unwrap();
    assert_eq!(session.bind_document(7).unwrap(), 1);
    assert!(session.authorized(&cfg, &main, &request()));
    let mut wrong = request();
    wrong.parent_frame_id = Some(7);
    assert!(!session.authorized(&cfg, &main, &wrong));
    wrong = request();
    wrong.frame_id = 8;
    assert!(!session.authorized(&cfg, &main, &wrong));
    wrong = request();
    wrong.window = 124;
    assert!(!session.authorized(&cfg, &main, &wrong));
    wrong = request();
    wrong.source = "https://example.org/";
    assert!(!session.authorized(&cfg, &main, &wrong));
    wrong = request();
    wrong.document_generation = 2;
    assert!(!session.authorized(&cfg, &main, &wrong));
    let mut changed = main_request();
    changed.document_generation = 10;
    assert!(!session.authorized(&cfg, &changed, &request()));
    changed = main_request();
    changed.frame_id = Some(6);
    changed.main_frame_id = Some(6);
    assert!(!session.authorized(&cfg, &changed, &request()));
    let mut bad = main_request();
    bad.parent_frame_id = Some(1);
    assert!(PickerSession::new(&cfg, &bad, 123, false).is_err());
    assert!(PickerSession::new(&cfg, &main, 0, false).is_err());
}
