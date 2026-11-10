//! Real byte transformations and delivery ownership; not Windows hardware evidence.
use nimbo_desktop::frame_route::{FrameAck, FrameLayout, FrameRoute};
use nimbo_desktop::policy::{Config, NativeRequest};

fn request(generation: u64) -> NativeRequest<'static> {
    NativeRequest {
        window: "main",
        source: "https://golive-ad118.web.app/",
        frame_id: Some(7),
        main_frame_id: Some(7),
        parent_frame_id: None,
        document_generation: generation,
    }
}

#[test]
fn padded_bgra_becomes_tightly_packed_rgba_without_padding_or_undefined_alpha() {
    let layout = FrameLayout::new(2, 2).unwrap();
    let input = [
        1, 2, 3, 0, 4, 5, 6, 7, 99, 99, 99, 99, 8, 9, 10, 11, 12, 13, 14, 15,
    ];
    assert_eq!(
        layout.rgba(&input, 12).unwrap(),
        [3, 2, 1, 255, 6, 5, 4, 255, 10, 9, 8, 255, 14, 13, 12, 255]
    );
    assert!(layout.rgba(&input[..19], 12).is_err());
    assert!(layout.rgba(&input, 7).is_err());
    assert!(FrameLayout::new(0, 1).is_err());
    assert!(FrameLayout::new(u32::MAX, 2).is_err());
}

#[test]
fn credits_bind_frame_sequence_source_document_and_deadline() {
    let config = Config::release();
    let mut route =
        FrameRoute::new(&config, &request(3), 3, 17, FrameLayout::new(2, 2).unwrap()).unwrap();
    let a = route.reserve(&config, &request(3), 0).unwrap().unwrap();
    let b = route.reserve(&config, &request(3), 1).unwrap().unwrap();
    assert!(route.reserve(&config, &request(3), 2).unwrap().is_none());
    let ack = FrameAck {
        session: 17,
        slot: a.slot(),
        sequence: a.sequence(),
    };
    assert!(!route.acknowledge_at(&config, &request(2), ack, 2));
    assert!(!route.acknowledge_at(&config, &request(3), FrameAck { session: 18, ..ack }, 2));
    assert!(route.acknowledge_at(&config, &request(3), ack, 2));
    let c = route.reserve(&config, &request(3), 3).unwrap().unwrap();
    assert!(!route.acknowledge_at(&config, &request(3), ack, 3));
    assert_eq!(c.slot(), a.slot());
    let json = route.metadata(c);
    assert!(json.contains("\"format\":\"RGBA8\""));
    assert!(json.contains("\"session\":17"));
    assert!(
        route.reserve(&config, &request(3), 2001).is_err(),
        "unacked frame deadline closes route"
    );
    assert!(!route.acknowledge_at(
        &config,
        &request(3),
        FrameAck {
            session: 17,
            slot: b.slot(),
            sequence: b.sequence()
        },
        2001
    ));
    assert!(route.reserve(&config, &request(3), 2002).is_err());
}

#[test]
fn unknown_attribution_reload_and_stop_fail_closed() {
    let config = Config::release();
    let mut unknown = request(1);
    unknown.frame_id = None;
    assert!(FrameRoute::new(&config, &unknown, 1, 1, FrameLayout::new(1, 1).unwrap()).is_err());
    assert!(FrameRoute::new(&config, &request(1), 1, 0, FrameLayout::new(1, 1).unwrap()).is_err());
    let mut route =
        FrameRoute::new(&config, &request(1), 1, 1, FrameLayout::new(1, 1).unwrap()).unwrap();
    assert!(route.reserve(&config, &request(2), 0).is_err());
    assert!(route.reserve(&config, &request(1), 1).is_err());
    route.stop();
    route.stop();
}

#[test]
fn expired_ack_cannot_reanimate_a_stalled_producer() {
    let config = Config::release();
    let mut route =
        FrameRoute::new(&config, &request(1), 1, 1, FrameLayout::new(1, 1).unwrap()).unwrap();
    let ticket = route.reserve(&config, &request(1), 0).unwrap().unwrap();
    assert!(!route.acknowledge_at(
        &config,
        &request(1),
        nimbo_desktop::frame_route::FrameAck {
            session: 1,
            slot: ticket.slot(),
            sequence: ticket.sequence()
        },
        2000
    ));
    assert!(route.reserve(&config, &request(1), 2001).is_err());
}

#[test]
fn same_origin_new_document_cannot_inherit_the_capture_route() {
    let config = Config::release();
    let mut route =
        FrameRoute::new(&config, &request(1), 1, 1, FrameLayout::new(1, 1).unwrap()).unwrap();
    let mut changed = request(1);
    changed.source = "https://golive-ad118.web.app/another-document";
    assert!(route.reserve(&config, &changed, 0).is_err());
    assert!(route.reserve(&config, &request(1), 1).is_err());
}
