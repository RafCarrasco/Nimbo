//! Bounded real-frame layout and native-owned delivery state. No pixels travel in JSON.
use crate::policy::{Config, NativeRequest};

const JS_MAX: u64 = (1 << 53) - 1;
const ACK_TIMEOUT_MS: u64 = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameLayout {
    width: u32,
    height: u32,
}
impl FrameLayout {
    pub fn new(width: u32, height: u32) -> Result<Self, &'static str> {
        if width == 0 || height == 0 || width > 3840 || height > 2160 {
            return Err("outside capture experiment budget");
        }
        Ok(Self { width, height })
    }
    pub fn width(self) -> u32 {
        self.width
    }
    pub fn height(self) -> u32 {
        self.height
    }
    pub fn bytes(self) -> usize {
        self.width as usize * self.height as usize * 4
    }
    /// Content only; never copy texture row padding or undefined WGC alpha into the page.
    pub fn rgba(self, bgra: &[u8], pitch: usize) -> Result<Vec<u8>, &'static str> {
        let row = self.width as usize * 4;
        let required = pitch
            .checked_mul(self.height as usize - 1)
            .and_then(|n| n.checked_add(row))
            .ok_or("pitch overflow")?;
        if pitch < row || bgra.len() < required {
            return Err("truncated mapped texture");
        }
        let mut output = vec![0; self.bytes()];
        for y in 0..self.height as usize {
            for (src, dst) in bgra[y * pitch..y * pitch + row]
                .as_chunks::<4>()
                .0
                .iter()
                .zip(
                    output[y * row..(y + 1) * row]
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut(),
                )
            {
                dst.copy_from_slice(&[src[2], src[1], src[0], 255]);
            }
        }
        Ok(output)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DeliveryTicket {
    slot: usize,
    sequence: u64,
}
impl DeliveryTicket {
    pub fn slot(self) -> usize {
        self.slot
    }
    pub fn sequence(self) -> u64 {
        self.sequence
    }
}

#[derive(Clone, Copy)]
pub struct FrameAck {
    pub session: u64,
    pub slot: usize,
    pub sequence: u64,
}

pub struct FrameRoute {
    generation: u64,
    session: u64,
    frame: u32,
    source: String,
    layout: FrameLayout,
    slots: [Option<(u64, u64)>; 2],
    sequence: u64,
    stopped: bool,
    last_time: u64,
}
impl FrameRoute {
    /// Identity MUST come from host ownership, never fields deserialized from page messages.
    pub fn new(
        config: &Config,
        request: &NativeRequest<'_>,
        generation: u64,
        session: u64,
        layout: FrameLayout,
    ) -> Result<Self, &'static str> {
        if !config.authorize(request, generation)
            || session == 0
            || session > JS_MAX
            || generation > JS_MAX
        {
            return Err("unattributed capture route");
        }
        Ok(Self {
            generation,
            session,
            frame: request.frame_id.ok_or("unknown frame")?,
            source: request.source.to_owned(),
            layout,
            slots: [None, None],
            sequence: 0,
            stopped: false,
            last_time: 0,
        })
    }
    fn authorized(&self, config: &Config, request: &NativeRequest<'_>) -> bool {
        !self.stopped
            && config.authorize(request, self.generation)
            && request.frame_id == Some(self.frame)
            && request.source == self.source
    }
    pub fn layout(&self) -> FrameLayout {
        self.layout
    }
    pub fn reserve(
        &mut self,
        config: &Config,
        request: &NativeRequest<'_>,
        now_ms: u64,
    ) -> Result<Option<DeliveryTicket>, &'static str> {
        if !self.authorized(config, request)
            || now_ms < self.last_time
            || self
                .slots
                .iter()
                .flatten()
                .any(|(_, time)| now_ms.saturating_sub(*time) >= ACK_TIMEOUT_MS)
            || self.sequence == JS_MAX
        {
            self.stop();
            return Err("capture route revoked, expired or exhausted");
        }
        self.last_time = now_ms;
        let Some(slot) = self.slots.iter().position(Option::is_none) else {
            return Ok(None);
        };
        self.sequence += 1;
        self.slots[slot] = Some((self.sequence, now_ms));
        Ok(Some(DeliveryTicket {
            slot,
            sequence: self.sequence,
        }))
    }
    pub fn acknowledge_at(
        &mut self,
        config: &Config,
        request: &NativeRequest<'_>,
        ack: FrameAck,
        now_ms: u64,
    ) -> bool {
        if now_ms < self.last_time
            || self
                .slots
                .iter()
                .flatten()
                .any(|(_, time)| now_ms.saturating_sub(*time) >= ACK_TIMEOUT_MS)
        {
            self.stop();
            return false;
        }
        if !self.authorized(config, request)
            || ack.session != self.session
            || self
                .slots
                .get(ack.slot)
                .copied()
                .flatten()
                .map(|(seq, _)| seq)
                != Some(ack.sequence)
        {
            return false;
        }
        self.last_time = now_ms;
        self.slots[ack.slot] = None;
        true
    }
    #[cfg(windows)]
    fn cancel_unposted(&mut self, ticket: DeliveryTicket) {
        if self
            .slots
            .get(ticket.slot)
            .copied()
            .flatten()
            .map(|(seq, _)| seq)
            == Some(ticket.sequence)
        {
            self.slots[ticket.slot] = None;
        }
    }
    pub fn metadata(&self, ticket: DeliveryTicket) -> String {
        format!(
            "{{\"kind\":\"nimbo-frame\",\"format\":\"RGBA8\",\"generation\":{},\"session\":{},\"slot\":{},\"sequence\":{},\"width\":{},\"height\":{}}}",
            self.generation,
            self.session,
            ticket.slot,
            ticket.sequence,
            self.layout.width,
            self.layout.height
        )
    }
    pub fn stop(&mut self) {
        self.stopped = true;
        self.slots = [None, None];
    }
}

/// Function expression, installed only by a future attributed host bootstrap; not a global IPC grant.
pub const CANVAS_ADAPTER: &str = include_str!("../bridge/capture.js");

#[cfg(windows)]
pub mod windows;
