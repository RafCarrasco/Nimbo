//! Attributed picker lifecycle. Portable host core, not a WebView controller or capture grant.
use crate::policy::{Config, NativeRequest};
use std::collections::HashMap;
mod protocol;
#[cfg(windows)]
pub mod windows;
pub use protocol::{WireAction, WireMethod};
pub const PICKER_URL: &str = "https://nimbo-picker.invalid/";
pub const PICKER_SCRIPT: &str = include_str!("../bridge/picker.js");
pub fn document_resource(uri: &str, method: &str) -> Option<&'static [u8]> {
    (uri == PICKER_URL && method == "GET")
        .then_some(include_str!("../../capture-picker.html").as_bytes())
}
pub fn minimum_size(work: crate::shell::Rect, dpi: u32) -> Result<(i32, i32), &'static str> {
    let bounds = initial_bounds(work, dpi)?;
    Ok((
        crate::shell::scale(680, dpi).min(bounds.right - bounds.left),
        crate::shell::scale(500, dpi).min(bounds.bottom - bounds.top),
    ))
}
pub fn initial_bounds(
    work: crate::shell::Rect,
    dpi: u32,
) -> Result<crate::shell::Rect, &'static str> {
    let width = i64::from(work.right) - i64::from(work.left);
    let height = i64::from(work.bottom) - i64::from(work.top);
    let margin = i64::from(crate::shell::scale(40, dpi));
    if width <= margin || height <= margin {
        return Err("picker-workarea-too-small");
    }
    let w = i64::from(crate::shell::scale(880, dpi)).min(width - margin);
    let h = i64::from(crate::shell::scale(640, dpi)).min(height - margin);
    let left = i64::from(work.left) + (width - w) / 2;
    let top = i64::from(work.top) + (height - h) / 2;
    Ok(crate::shell::Rect {
        left: left.try_into().map_err(|_| "picker-bounds-overflow")?,
        top: top.try_into().map_err(|_| "picker-bounds-overflow")?,
        right: (left + w)
            .try_into()
            .map_err(|_| "picker-bounds-overflow")?,
        bottom: (top + h).try_into().map_err(|_| "picker-bounds-overflow")?,
    })
}
/// Host-populated event attribution; never deserialize these fields from JavaScript.
/// `window` is a unique, non-reused host controller instance id, not a raw/reusable HWND.
pub struct PickerRequest<'a> {
    pub window: u64,
    pub source: &'a str,
    pub frame_id: u32,
    pub parent_frame_id: Option<u32>,
    pub document_generation: u64,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RefreshTicket {
    window: u64,
    generation: u64,
    sequence: u64,
}
struct PresentedSource {
    wire_id: String,
    candidate: crate::sources::Candidate,
}
pub struct PickerSession {
    next_item: u64,
    presented: HashMap<String, PresentedSource>,
    wire_ready: bool,
    last_wire_id: u32,
    closed: bool,
    window: u64,
    owner_generation: u64,
    owner_frame: Option<u32>,
    generation: u64,
    frame: Option<u32>,
    audio: bool,
    refresh: u64,
    catalog: Option<crate::sources::Catalog>,
    pending: Option<RefreshTicket>,
}
pub struct Choice {
    owner_frame: Option<u32>,
    owner_generation: u64,
    candidate: crate::sources::Candidate,
    audio: bool,
}
impl Choice {
    /// Native owner liveness only; this is not OS source identity validation or a WGC grant.
    pub fn authorized(&self, config: &Config, main: &NativeRequest<'_>) -> bool {
        config.authorize(main, self.owner_generation) && main.frame_id == self.owner_frame
    }
    pub fn candidate(&self) -> &crate::sources::Candidate {
        &self.candidate
    }
    pub fn audio(&self) -> bool {
        self.audio
    }
}
impl PickerSession {
    pub fn cancel(
        &mut self,
        config: &Config,
        main: &NativeRequest<'_>,
        request: &PickerRequest<'_>,
    ) -> Result<(), &'static str> {
        if !self.authorized(config, main, request) {
            return Err("picker-unauthorized");
        }
        self.closed = true;
        self.catalog = None;
        self.presented.clear();
        self.pending = None;
        self.frame = None;
        Ok(())
    }
    pub fn choose(
        &mut self,
        config: &Config,
        main: &NativeRequest<'_>,
        request: &PickerRequest<'_>,
        id: &str,
        audio: bool,
        fresh: &crate::sources::Catalog,
    ) -> Result<Choice, &'static str> {
        if !self.authorized(config, main, request) {
            return Err("picker-unauthorized");
        }
        let presented = self
            .presented
            .values()
            .find(|p| p.wire_id == id)
            .map(|p| &p.candidate)
            .ok_or("picker-source-not-found")?;
        let candidate = fresh
            .resolve(presented.id())
            .ok_or("picker-source-not-found")?;
        if !presented.same_source(&candidate) {
            return Err("picker-source-changed");
        }
        self.closed = true;
        self.catalog = None;
        self.presented.clear();
        self.pending = None;
        self.frame = None;
        Ok(Choice {
            candidate,
            audio,
            owner_frame: self.owner_frame,
            owner_generation: self.owner_generation,
        })
    }
    pub fn new(
        config: &Config,
        main: &NativeRequest<'_>,
        window: u64,
        audio: bool,
    ) -> Result<Self, &'static str> {
        if window == 0 || !config.authorize(main, main.document_generation) {
            return Err("picker-unauthorized");
        }
        Ok(Self {
            next_item: 0,
            presented: HashMap::new(),
            wire_ready: false,
            last_wire_id: 0,
            closed: false,
            window,
            owner_generation: main.document_generation,
            owner_frame: main.frame_id,
            generation: 0,
            frame: None,
            audio,
            refresh: 0,
            catalog: None,
            pending: None,
        })
    }
    pub fn begin_refresh(
        &mut self,
        config: &Config,
        main: &NativeRequest<'_>,
        request: &PickerRequest<'_>,
    ) -> Result<RefreshTicket, &'static str> {
        if !self.authorized(config, main, request) {
            return Err("picker-unauthorized");
        }
        self.refresh = self
            .refresh
            .checked_add(1)
            .ok_or("picker-refresh-exhausted")?;
        let ticket = RefreshTicket {
            window: self.window,
            generation: self.generation,
            sequence: self.refresh,
        };
        self.pending = Some(ticket);
        Ok(ticket)
    }
    pub fn finish_refresh(
        &mut self,
        ticket: RefreshTicket,
        result: Result<crate::sources::Catalog, &'static str>,
    ) -> Result<bool, &'static str> {
        if self.pending != Some(ticket)
            || ticket.window != self.window
            || ticket.generation != self.generation
        {
            return Ok(false);
        }
        self.pending = None;
        match result {
            Ok(c) => {
                self.catalog = None;
                let previous = std::mem::take(&mut self.presented);
                let mut presented = HashMap::new();
                let rows = c.list();
                for row in rows.as_array().ok_or("picker-invalid-catalog")? {
                    let native_id = row["id"].as_str().ok_or("picker-invalid-catalog")?;
                    let candidate = c.resolve(native_id).ok_or("picker-invalid-catalog")?;
                    let wire_id = if let Some(old) = previous
                        .get(native_id)
                        .filter(|p| p.candidate.same_source(&candidate))
                    {
                        old.wire_id.clone()
                    } else {
                        self.next_item = self
                            .next_item
                            .checked_add(1)
                            .ok_or("picker-item-exhausted")?;
                        format!(
                            "source:{}:{}:{}",
                            self.window, self.generation, self.next_item
                        )
                    };
                    presented.insert(native_id.to_owned(), PresentedSource { wire_id, candidate });
                }
                self.presented = presented;
                self.catalog = Some(c);
                Ok(true)
            }
            Err(e) => {
                self.catalog = None;
                self.presented.clear();
                Err(e)
            }
        }
    }
    pub fn list(
        &self,
        config: &Config,
        main: &NativeRequest<'_>,
        request: &PickerRequest<'_>,
    ) -> Result<serde_json::Value, &'static str> {
        if !self.authorized(config, main, request) {
            return Err("picker-unauthorized");
        }
        let mut sources = self
            .catalog
            .as_ref()
            .ok_or("picker-catalog-unavailable")?
            .list();
        for row in sources.as_array_mut().ok_or("picker-invalid-catalog")? {
            let native_id = row["id"].as_str().ok_or("picker-invalid-catalog")?;
            let token = &self
                .presented
                .get(native_id)
                .ok_or("picker-invalid-catalog")?
                .wire_id;
            row["id"] = serde_json::Value::String(token.clone());
        }
        Ok(serde_json::json!({"sources":sources,"audio":self.audio}))
    }
    pub fn bind_document(&mut self, frame: u32) -> Result<u64, &'static str> {
        if self.closed {
            return Err("picker-closed");
        }
        self.wire_ready = false;
        self.last_wire_id = 0;
        self.catalog = None;
        self.presented.clear();
        self.pending = None;
        self.frame = None;
        if frame == 0 {
            return Err("picker-frame-unavailable");
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("picker-generation-exhausted")?;
        self.frame = Some(frame);
        Ok(self.generation)
    }
    pub fn authorized(
        &self,
        config: &Config,
        main: &NativeRequest<'_>,
        request: &PickerRequest<'_>,
    ) -> bool {
        !self.closed
            && config.authorize(main, self.owner_generation)
            && main.frame_id == self.owner_frame
            && request.window == self.window
            && request.source == PICKER_URL
            && self.frame == Some(request.frame_id)
            && request.frame_id != 0
            && request.parent_frame_id.is_none()
            && request.document_generation == self.generation
            && self.generation != 0
    }
}
