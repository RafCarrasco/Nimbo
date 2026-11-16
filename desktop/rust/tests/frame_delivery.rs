//! Exercises the exact portable delivery gate used by the native frame pump, not hardware.
use nimbo_desktop::frame_route::{FrameAck, FrameLayout, FrameRoute};
use nimbo_desktop::policy::{Config, NativeRequest};
use std::time::Duration;

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

// Preservation controls for existing authorization/deadline behavior through the new gate.
#[test]
fn submillisecond_clock_rollback_revokes_even_while_throttled() {
    let config = Config::release();
    let mut route = FrameRoute::at_frame_rate(
        &config,
        &request(3),
        3,
        17,
        FrameLayout::new(2, 2).unwrap(),
        30,
    )
    .unwrap();
    let first = route
        .reserve_due(&config, &request(3), Duration::ZERO)
        .unwrap()
        .unwrap();
    route.mark_published(first, Duration::ZERO).unwrap();
    assert!(
        route
            .reserve_due(&config, &request(3), Duration::from_micros(17_500))
            .unwrap()
            .is_none()
    );
    assert!(
        route
            .reserve_due(&config, &request(3), Duration::from_micros(17_400))
            .is_err()
    );
}

#[test]
fn ack_timeout_is_checked_even_when_the_next_frame_is_throttled() {
    let config = Config::release();
    let mut route = FrameRoute::at_frame_rate(
        &config,
        &request(3),
        3,
        17,
        FrameLayout::new(2, 2).unwrap(),
        30,
    )
    .unwrap();
    let first = route
        .reserve_due(&config, &request(3), Duration::ZERO)
        .unwrap()
        .unwrap();
    route.mark_published(first, Duration::ZERO).unwrap();
    let second = route
        .reserve_due(&config, &request(3), Duration::from_millis(1990))
        .unwrap()
        .unwrap();
    route
        .mark_published(second, Duration::from_millis(1990))
        .unwrap();
    assert!(
        route
            .reserve_due(&config, &request(3), Duration::from_millis(1999))
            .unwrap()
            .is_none()
    );
    assert!(
        route
            .reserve_due(&config, &request(3), Duration::from_millis(2000))
            .is_err()
    );
}

#[test]
fn a_changed_document_is_revoked_even_while_throttled() {
    let config = Config::release();
    let mut route = FrameRoute::at_frame_rate(
        &config,
        &request(3),
        3,
        17,
        FrameLayout::new(2, 2).unwrap(),
        30,
    )
    .unwrap();
    let first = route
        .reserve_due(&config, &request(3), Duration::ZERO)
        .unwrap()
        .unwrap();
    route.mark_published(first, Duration::ZERO).unwrap();
    assert!(
        route
            .reserve_due(&config, &request(4), Duration::from_millis(1))
            .is_err()
    );
    assert!(
        route
            .reserve_due(&config, &request(3), Duration::from_millis(34))
            .is_err()
    );
}

#[test]
fn the_sixty_fps_route_preserves_nanosecond_deadlines() {
    let config = Config::release();
    let mut route =
        FrameRoute::new(&config, &request(3), 3, 17, FrameLayout::new(2, 2).unwrap()).unwrap();
    let first = route
        .reserve_due(&config, &request(3), Duration::ZERO)
        .unwrap()
        .unwrap();
    route.mark_published(first, Duration::ZERO).unwrap();
    assert!(route.acknowledge_at(
        &config,
        &request(3),
        FrameAck {
            session: 17,
            slot: first.slot(),
            sequence: first.sequence()
        },
        0
    ));
    assert!(
        route
            .reserve_due(&config, &request(3), Duration::from_nanos(16_666_666))
            .unwrap()
            .is_none()
    );
    let now = Duration::from_nanos(16_666_667);
    let next = route
        .reserve_due(&config, &request(3), now)
        .unwrap()
        .unwrap();
    route.mark_published(next, now).unwrap();
}

#[test]
fn unsupported_frame_rates_cannot_construct_a_route() {
    let config = Config::release();
    for rate in [0, 1, 29, 31, 59, 61, u32::MAX] {
        assert!(
            FrameRoute::at_frame_rate(
                &config,
                &request(3),
                3,
                17,
                FrameLayout::new(2, 2).unwrap(),
                rate
            )
            .is_err()
        );
    }
}

#[test]
fn an_unrepresentable_ack_clock_revokes_the_route_instead_of_wrapping() {
    let config = Config::release();
    let mut route =
        FrameRoute::new(&config, &request(3), 3, 17, FrameLayout::new(2, 2).unwrap()).unwrap();
    assert!(
        route
            .reserve_due(&config, &request(3), Duration::MAX)
            .is_err()
    );
    assert!(
        route
            .reserve_due(&config, &request(3), Duration::ZERO)
            .is_err()
    );
}

#[test]
fn a_publication_must_use_the_clock_of_its_own_reservation() {
    let config = Config::release();
    let mut route =
        FrameRoute::new(&config, &request(3), 3, 17, FrameLayout::new(2, 2).unwrap()).unwrap();
    let ticket = route
        .reserve_due(&config, &request(3), Duration::ZERO)
        .unwrap()
        .unwrap();
    assert!(
        route
            .mark_published(ticket, Duration::from_millis(17))
            .is_err()
    );
    assert!(
        route
            .reserve_due(&config, &request(3), Duration::from_millis(18))
            .is_err()
    );
}

#[test]
fn acknowledged_tickets_cannot_be_committed_again() {
    let config = Config::release();
    let mut route =
        FrameRoute::new(&config, &request(3), 3, 17, FrameLayout::new(2, 2).unwrap()).unwrap();
    let ticket = route
        .reserve_due(&config, &request(3), Duration::ZERO)
        .unwrap()
        .unwrap();
    route.mark_published(ticket, Duration::ZERO).unwrap();
    assert!(route.acknowledge_at(
        &config,
        &request(3),
        FrameAck {
            session: 17,
            slot: ticket.slot(),
            sequence: ticket.sequence(),
        },
        0
    ));
    assert!(
        route
            .mark_published(ticket, Duration::from_millis(34))
            .is_err()
    );
    assert!(
        route
            .reserve_due(&config, &request(3), Duration::from_millis(35))
            .is_err()
    );
}

#[test]
fn publication_cannot_commit_a_ticket_after_the_route_was_stopped() {
    let config = Config::release();
    let mut route =
        FrameRoute::new(&config, &request(3), 3, 17, FrameLayout::new(2, 2).unwrap()).unwrap();
    let ticket = route
        .reserve_due(&config, &request(3), Duration::ZERO)
        .unwrap()
        .unwrap();
    route.stop();
    assert!(route.mark_published(ticket, Duration::ZERO).is_err());
}

#[test]
fn thirty_fps_route_throttles_delivery_without_leaking_unposted_slots() {
    let config = Config::release();
    let mut route = FrameRoute::at_frame_rate(
        &config,
        &request(3),
        3,
        17,
        FrameLayout::new(2, 2).unwrap(),
        30,
    )
    .unwrap();
    let first = route
        .reserve_due(&config, &request(3), Duration::ZERO)
        .unwrap()
        .unwrap();
    route.mark_published(first, Duration::ZERO).unwrap();
    assert!(route.acknowledge_at(
        &config,
        &request(3),
        FrameAck {
            session: 17,
            slot: first.slot(),
            sequence: first.sequence(),
        },
        0
    ));
    for ms in 1..34 {
        assert!(
            route
                .reserve_due(&config, &request(3), Duration::from_millis(ms))
                .unwrap()
                .is_none(),
            "30 fps publication was not throttled at {ms} ms"
        );
    }
    let next = route
        .reserve_due(&config, &request(3), Duration::from_millis(34))
        .unwrap()
        .unwrap();
    route
        .mark_published(next, Duration::from_millis(34))
        .unwrap();
}
