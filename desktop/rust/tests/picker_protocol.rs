use nimbo_desktop::{
    picker::{PICKER_URL, PickerRequest, PickerSession, WireAction},
    policy::{Config, NativeRequest},
};
use serde_json::json;
#[test]
fn picker_wire_is_attributed_bounded_and_correlated_to_one_document() {
    let cfg = Config::release();
    let main = NativeRequest {
        window: "main",
        source: "https://golive-ad118.web.app/",
        frame_id: Some(1),
        main_frame_id: Some(1),
        parent_frame_id: None,
        document_generation: 4,
    };
    let mut session = PickerSession::new(&cfg, &main, 99, false).unwrap();
    session.bind_document(7).unwrap();
    let request = PickerRequest {
        window: 99,
        source: PICKER_URL,
        frame_id: 7,
        parent_frame_id: None,
        document_generation: 1,
    };
    let hello = r#"{"v":1,"kind":"picker-hello"}"#;
    assert!(matches!(
        session.receive(&cfg, &main, &request, hello).unwrap(),
        WireAction::Ready { generation: 1 }
    ));
    let call = |id, method: &str, args| {
        json!({"v":1,"kind":"picker-invoke","id":id,"gen":1,"method":method,"args":args})
            .to_string()
    };
    assert!(matches!(
        session
            .receive(&cfg, &main, &request, &call(1, "list", json!([])))
            .unwrap(),
        WireAction::Call { id: 1, .. }
    ));
    assert!(
        session
            .receive(&cfg, &main, &request, &call(1, "list", json!([])))
            .is_err()
    );
    let raw = call(2, "choose", json!([{"id":"window:21:0","audio":true}]));
    assert!(matches!(
        session.receive(&cfg, &main, &request, &raw).unwrap(),
        WireAction::Call { id: 2, .. }
    ));
    for raw in [
        call(3, "bogus", json!([])),
        call(3, "list", json!([true])),
        call(3, "choose", json!([{"id":"window:21:0","audio":"yes"}])),
        call(3, "choose", json!([{"id":"x".repeat(129),"audio":true}])),
    ] {
        assert!(session.receive(&cfg, &main, &request, &raw).is_err());
    }
    let wrong = PickerRequest {
        window: 99,
        source: PICKER_URL,
        frame_id: 7,
        parent_frame_id: Some(7),
        document_generation: 1,
    };
    assert_eq!(
        session.receive(&cfg, &main, &wrong, "not json").err(),
        Some("picker-unauthorized")
    );
    assert!(
        session
            .receive(&cfg, &main, &request, &" ".repeat(4097))
            .is_err()
    );
    let stale = json!({"v":1,"kind":"picker-invoke","id":3,"gen":2,"method":"cancel","args":[]})
        .to_string();
    assert!(session.receive(&cfg, &main, &request, &stale).is_err());
    assert!(matches!(
        session
            .receive(&cfg, &main, &request, &call(3, "cancel", json!([])))
            .unwrap(),
        WireAction::Call { id: 3, .. }
    ));
}
