//! Attributed picker lifecycle. Portable host core, not a WebView controller or capture grant.
use crate::policy::{Config, NativeRequest};
pub const PICKER_URL: &str = "https://nimbo-picker.invalid/";
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
pub struct PickerSession {
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
    candidate: crate::sources::Candidate,
    audio: bool,
}
impl Choice {
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
            .catalog
            .as_ref()
            .and_then(|c| c.resolve(id))
            .ok_or("picker-source-not-found")?;
        let candidate = fresh.resolve(id).ok_or("picker-source-not-found")?;
        if !presented.same_source(&candidate) {
            return Err("picker-source-changed");
        }
        self.closed = true;
        self.catalog = None;
        self.pending = None;
        self.frame = None;
        Ok(Choice { candidate, audio })
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
                self.catalog = Some(c);
                Ok(true)
            }
            Err(e) => {
                self.catalog = None;
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
        Ok(
            serde_json::json!({"sources":self.catalog.as_ref().ok_or("picker-catalog-unavailable")?.list(),"audio":self.audio}),
        )
    }
    pub fn bind_document(&mut self, frame: u32) -> Result<u64, &'static str> {
        if self.closed {
            return Err("picker-closed");
        }
        self.catalog = None;
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
