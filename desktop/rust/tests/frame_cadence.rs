//! Portable scheduling contract only. This is not Windows capture or throughput evidence.
use nimbo_desktop::frame_route::cadence::FrameCadence;
use std::time::Duration;

// Characterization/regression controls for the scheduler; no native throughput claims.
#[test]
fn sixty_fps_keeps_its_own_rate_and_deadline() {
    let mut cadence = FrameCadence::new(60).unwrap();
    assert_eq!(cadence.frame_rate(), 60);
    cadence.published(Duration::ZERO).unwrap();
    assert!(!cadence.is_due(Duration::from_millis(16)));
    assert!(cadence.is_due(Duration::from_millis(17)));
}

#[test]
fn polling_without_a_successful_publication_does_not_consume_the_budget() {
    let mut cadence = FrameCadence::new(30).unwrap();
    assert!(cadence.is_due(Duration::from_secs(10)));
    assert!(cadence.is_due(Duration::from_secs(10)));
    cadence.published(Duration::from_secs(10)).unwrap();
    assert!(!cadence.is_due(Duration::from_secs(10)));
}

#[test]
fn clock_rollback_cannot_reset_the_last_successful_publication() {
    let mut cadence = FrameCadence::new(30).unwrap();
    cadence.published(Duration::from_millis(1000)).unwrap();
    assert!(!cadence.is_due(Duration::from_millis(900)));
    assert!(cadence.published(Duration::from_millis(900)).is_err());
    assert!(!cadence.is_due(Duration::from_millis(1033)));
    assert!(cadence.is_due(Duration::from_millis(1034)));
}

#[test]
fn a_stall_does_not_accumulate_credits_for_a_catch_up_burst() {
    let mut cadence = FrameCadence::new(60).unwrap();
    cadence.published(Duration::ZERO).unwrap();
    cadence.published(Duration::from_secs(10)).unwrap();
    assert!(!cadence.is_due(Duration::from_secs(10)));
    assert!(cadence.published(Duration::from_secs(10)).is_err());
    assert!(cadence.is_due(Duration::from_secs(10) + Duration::from_nanos(16_666_667)));
}

#[test]
fn max_duration_does_not_overflow_and_a_new_session_has_no_old_credit() {
    for frame_rate in [30, 60] {
        let mut cadence = FrameCadence::new(frame_rate).unwrap();
        cadence.published(Duration::ZERO).unwrap();
        assert!(cadence.is_due(Duration::MAX));
        cadence.published(Duration::MAX).unwrap();
        assert!(!cadence.is_due(Duration::MAX));
        assert!(cadence.published(Duration::ZERO).is_err());
        let fresh = FrameCadence::new(frame_rate).unwrap();
        assert!(fresh.is_due(Duration::ZERO));
    }
}

#[test]
fn deadlines_keep_submillisecond_precision_instead_of_silently_reducing_the_rate() {
    for (frame_rate, deadline_ns) in [(30, 33_333_334), (60, 16_666_667)] {
        let mut cadence = FrameCadence::new(frame_rate).unwrap();
        cadence.published(Duration::ZERO).unwrap();
        assert!(!cadence.is_due(Duration::from_nanos(deadline_ns - 1)));
        assert!(
            cadence.is_due(Duration::from_nanos(deadline_ns)),
            "{frame_rate} fps was rounded to whole milliseconds"
        );
    }
}

#[test]
fn an_early_publication_is_rejected_without_changing_the_last_successful_time() {
    let mut cadence = FrameCadence::new(30).unwrap();
    cadence.published(Duration::ZERO).unwrap();
    assert!(cadence.published(Duration::from_millis(17)).is_err());
    assert!(cadence.is_due(Duration::from_millis(34)));
    cadence.published(Duration::from_millis(34)).unwrap();
    assert!(!cadence.is_due(Duration::from_millis(67)));
    assert!(cadence.is_due(Duration::from_millis(68)));
}

#[test]
fn thirty_fps_does_not_publish_at_the_legacy_sixty_fps_interval() {
    let mut cadence = FrameCadence::new(30).unwrap();
    assert_eq!(cadence.frame_rate(), 30);
    assert!(cadence.is_due(Duration::ZERO));
    cadence.published(Duration::ZERO).unwrap();
    assert!(
        !cadence.is_due(Duration::from_millis(17)),
        "30 fps must not reuse the legacy 17 ms interval"
    );
    assert!(!cadence.is_due(Duration::from_millis(33)));
    assert!(cadence.is_due(Duration::from_millis(34)));
}

#[test]
fn only_the_consumer_supported_thirty_and_sixty_rates_are_accepted() {
    assert!(FrameCadence::new(30).is_ok());
    assert!(FrameCadence::new(60).is_ok());
    for frame_rate in [0, 1, 29, 31, 59, 61, u32::MAX] {
        assert!(
            FrameCadence::new(frame_rate).is_err(),
            "unsupported rate {frame_rate} was accepted"
        );
    }
}
