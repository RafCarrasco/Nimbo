//! Bridge v1 dispatcher (CP2 S1, plan §4.3-4.5). Portable and pure: the Windows host and the
//! debug-only stdio example drive it identically. Attribution comes ONLY from native data
//! (`NativeRequest`), checked before the message is parsed. Unauthorized, stale, replayed or
//! malformed messages get no reply, only a fixed code whose logging is rate bounded. This is a
//! foreign-origin/iframe boundary, not protection against same-origin XSS or local processes.
use crate::policy::{ArgShape, BridgeMethod, Config, MAX_STRING_ARG_BYTES, NativeRequest};
use serde_json::{Value, json};
use std::rc::Rc;

pub const MAX_INBOUND_BYTES: usize = 4096;
pub const MAX_OUTBOUND_BYTES: usize = 4096;
/// Largest generation put on the wire: the page compares it as an exact JavaScript `Number`.
pub const MAX_WIRE_GENERATION: u64 = (1 << 53) - 1;
/// Drop codes are logged at most `LOG_BUDGET` times per `LOG_WINDOW_MS`; the rest are counted
/// and reported once as `Outbound::Suppressed(count)` when a later window starts.
pub const LOG_WINDOW_MS: u64 = 1000;
pub const LOG_BUDGET: u32 = 16;
/// S1 interim answer (plan §12 D4): internal, undistributed; replaced by the S2 login.
pub const SIGN_IN_INTERIM: &str = "Login pelo app ainda não disponível nesta prévia.";
const MALFORMED: &str = "bridge-malformed";
const SCRIPT: &str = include_str!("../bridge/nimbo-desktop.js");

/// The injected document-created script with the app origin as a JSON string literal.
pub fn script(app_origin: &str) -> String {
    SCRIPT.replacen("__APP_ORIGIN__", &Value::from(app_origin).to_string(), 1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outbound {
    /// JSON for `PostWebMessageAsJson` to the current top-level document.
    Reply(String),
    /// Fixed log code without payload.
    Code(&'static str),
    /// Number of drop codes suppressed in the previous log window.
    Suppressed(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentState {
    Disabled,
    AwaitingHello,
    Ready { last_id: u32 },
}

pub enum Args {
    Empty,
    Bool(bool),
    Text(String),
}
impl std::fmt::Debug for Args {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Args(..)") // never the page-supplied values
    }
}

#[derive(Debug)]
pub struct Invoke {
    pub id: u32,
    pub generation: u64,
    pub method: BridgeMethod,
    pub args: Args,
}
#[derive(Debug)]
pub enum Inbound {
    Hello,
    Invoke(Invoke),
}

/// Strict v1 grammar: exact key sets, integer id 1..=u32::MAX, allowlisted method and args.
pub fn parse(json: &str) -> Result<Inbound, &'static str> {
    if json.len() > MAX_INBOUND_BYTES {
        return Err(MALFORMED);
    }
    let Ok(Value::Object(m)) = serde_json::from_str::<Value>(json) else {
        return Err(MALFORMED);
    };
    if m.get("v") != Some(&Value::from(1)) {
        return Err(MALFORMED);
    }
    match m.get("kind").and_then(Value::as_str) {
        Some("hello") if m.len() == 2 => Ok(Inbound::Hello),
        Some("invoke") if m.len() == 6 => {
            let id = m.get("id").and_then(Value::as_u64).filter(|&i| i >= 1);
            let id = id.and_then(|i| u32::try_from(i).ok()).ok_or(MALFORMED)?;
            let generation = m.get("gen").and_then(Value::as_u64).ok_or(MALFORMED)?;
            let name = m.get("method").and_then(Value::as_str).ok_or(MALFORMED)?;
            let method = BridgeMethod::parse(name).ok_or("bridge-unknown-method")?;
            let args = m.get("args").and_then(Value::as_array).ok_or(MALFORMED)?;
            let args = match (method.args(), args.as_slice()) {
                (ArgShape::Empty, []) => Args::Empty,
                (ArgShape::Bool, [Value::Bool(b)]) => Args::Bool(*b),
                (ArgShape::ShortString, [Value::String(s)]) if s.len() <= MAX_STRING_ARG_BYTES => {
                    Args::Text(s.clone())
                }
                _ => return Err(MALFORMED),
            };
            Ok(Inbound::Invoke(Invoke {
                id,
                generation,
                method,
                args,
            }))
        }
        _ => Err(MALFORMED),
    }
}

/// Serializes an outbound message, refusing anything above `MAX_OUTBOUND_BYTES`.
pub fn encode(message: &Value) -> Result<String, &'static str> {
    let text = message.to_string();
    if text.len() > MAX_OUTBOUND_BYTES {
        return Err("bridge-outbound-too-large");
    }
    Ok(text)
}

#[derive(Debug, Default)]
struct LogLimiter {
    window_start: u64,
    logged: u32,
    suppressed: u64,
}
impl LogLimiter {
    fn roll(&mut self, now: u64) -> Vec<Outbound> {
        if now < self.window_start.saturating_add(LOG_WINDOW_MS) {
            return Vec::new();
        }
        self.window_start = now;
        self.logged = 0;
        match std::mem::take(&mut self.suppressed) {
            0 => Vec::new(),
            n => vec![Outbound::Suppressed(n)],
        }
    }
    fn record(&mut self, code: &'static str, out: &mut Vec<Outbound>) {
        if self.logged < LOG_BUDGET {
            self.logged += 1;
            out.push(Outbound::Code(code));
        } else {
            self.suppressed = self.suppressed.saturating_add(1);
        }
    }
}

#[derive(Debug)]
pub struct Dispatcher {
    config: Rc<Config>,
    generation: u64,
    main_frame_id: Option<u32>,
    document: DocumentState,
    /// Process-global (survives navigations), like Electron's main-process `audioWanted`.
    audio_wanted: bool,
    log: LogLimiter,
}

impl Dispatcher {
    pub fn new(config: Rc<Config>) -> Self {
        Self::starting_at(config, 0)
    }
    /// Starts at an arbitrary generation (bound tests; the host always starts at 0).
    #[doc(hidden)]
    pub fn starting_at(config: Rc<Config>, generation: u64) -> Self {
        Self {
            config,
            generation,
            main_frame_id: None,
            document: DocumentState::Disabled,
            audio_wanted: false,
            log: LogLimiter::default(),
        }
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn document(&self) -> DocumentState {
        self.document
    }
    pub fn audio_wanted(&self) -> bool {
        self.audio_wanted
    }
    /// ContentLoading: a new top-level document. Never reuses or wraps a generation; beyond
    /// `MAX_WIRE_GENERATION` every document stays disabled.
    pub fn begin_document(&mut self) -> Vec<Outbound> {
        self.generation = self.generation.saturating_add(1);
        self.main_frame_id = None;
        if self.generation > MAX_WIRE_GENERATION {
            self.document = DocumentState::Disabled;
            return vec![Outbound::Code("bridge-generation-exhausted")];
        }
        self.document = DocumentState::AwaitingHello;
        Vec::new()
    }
    /// Main frame id read natively at/after ContentLoading; 0 means unavailable (fail closed).
    pub fn set_main_frame(&mut self, id: u32) -> Vec<Outbound> {
        if id == 0 {
            return self.disable_document("bridge-frame-id-unavailable");
        }
        self.main_frame_id = Some(id);
        Vec::new()
    }
    pub fn disable_document(&mut self, code: &'static str) -> Vec<Outbound> {
        self.document = DocumentState::Disabled;
        vec![Outbound::Code(code)]
    }
    /// Attribution for a top-level WebMessageReceived event, from host-owned data only.
    pub fn main_frame_request<'a>(&self, source: &'a str) -> NativeRequest<'a> {
        NativeRequest {
            window: "main",
            source,
            frame_id: self.main_frame_id,
            main_frame_id: self.main_frame_id,
            parent_frame_id: None,
            document_generation: self.generation,
        }
    }
    pub fn receive(&mut self, attr: &NativeRequest<'_>, json: &str, now_ms: u64) -> Vec<Outbound> {
        let mut out = self.log.roll(now_ms);
        match self.handle(attr, json) {
            Ok(reply) => out.push(Outbound::Reply(reply)),
            Err(code) => self.log.record(code, &mut out),
        }
        out
    }
    /// Periodic housekeeping (S1: log-window rollover only).
    pub fn tick(&mut self, now_ms: u64) -> Vec<Outbound> {
        self.log.roll(now_ms)
    }

    fn handle(&mut self, attr: &NativeRequest<'_>, json: &str) -> Result<String, &'static str> {
        if !self.config.authorize(attr, self.generation) {
            return Err("bridge-unauthorized");
        }
        if self.document == DocumentState::Disabled {
            return Err("bridge-disabled");
        }
        let call = match parse(json)? {
            Inbound::Hello if self.document == DocumentState::AwaitingHello => {
                self.document = DocumentState::Ready { last_id: 0 };
                return encode(&json!({"v": 1, "kind": "ready", "gen": self.generation}));
            }
            Inbound::Hello => return Err("bridge-duplicate-hello"),
            Inbound::Invoke(call) => call,
        };
        if call.generation != self.generation {
            return Err("bridge-stale");
        }
        let DocumentState::Ready { last_id } = self.document else {
            return Err("bridge-not-ready");
        };
        if call.id <= last_id {
            return Err("bridge-replayed-id");
        }
        self.document = DocumentState::Ready { last_id: call.id };
        let mut reply = self.answer(call.method, &call.args);
        reply["v"] = json!(1);
        reply["kind"] = json!("result");
        reply["id"] = json!(call.id);
        reply["gen"] = json!(self.generation);
        encode(&reply)
    }

    /// Electron's "nothing selected / not packaged" answers (plan §4.5). No events, no media.
    fn answer(&mut self, method: BridgeMethod, args: &Args) -> Value {
        use BridgeMethod as M;
        match method {
            M::SignIn => json!({"ok": false, "error": SIGN_IN_INTERIM}),
            M::CaptureAudio => {
                if let Args::Bool(wanted) = args {
                    self.audio_wanted = *wanted;
                }
                json!({"ok": true})
            }
            M::AudioStart | M::UpdateWatch | M::GameStart => json!({"ok": true, "value": null}),
            M::UpdateInstall => json!({"ok": true, "value": false}),
            M::SignInCancel | M::AudioStop | M::GameStop | M::CaptureStop => json!({"ok": true}),
        }
    }
}
