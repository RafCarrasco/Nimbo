//! Portable 30/60-fps publication scheduling used by the native frame route.
//! Windows runtime tests are explicitly deferred to final validation, not waived.
use std::time::Duration;

pub struct FrameCadence {
    frame_rate: u32,
    last_publication: Option<Duration>,
}

impl FrameCadence {
    pub fn new(frame_rate: u32) -> Result<Self, &'static str> {
        if !matches!(frame_rate, 30 | 60) {
            return Err("unsupported capture frame rate");
        }
        Ok(Self {
            frame_rate,
            last_publication: None,
        })
    }

    pub fn frame_rate(&self) -> u32 {
        self.frame_rate
    }

    pub fn is_due(&self, now: Duration) -> bool {
        self.last_publication.is_none_or(|last| {
            now.saturating_sub(last).as_nanos() * u128::from(self.frame_rate) >= 1_000_000_000
        })
    }

    pub fn published(&mut self, now: Duration) -> Result<(), &'static str> {
        if !self.is_due(now) {
            return Err("capture publication before deadline or clock rollback");
        }
        self.last_publication = Some(now);
        Ok(())
    }
}
