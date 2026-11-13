//! Bridge v1 dispatcher (CP2 S1, plan §4.3-4.5). Portable and pure: the Windows host and the
//! debug-only stdio example drive it identically. Attribution comes ONLY from native data
//! (`NativeRequest`), checked before the message is parsed. Unauthorized, stale, replayed or
//! malformed messages get no reply, only a fixed code whose logging is rate bounded. This is a
//! foreign-origin/iframe boundary, not protection against same-origin XSS or local processes.
//!
//! CP2 S2-core: `nimbo:signIn` is DEFERRED (plan §4.1). Its single result is encoded only in
//! `complete`, on the UI thread, from a `Settle` handed over by `LoginFlow`. A posted result
//! is not a Firebase sign-in (LOGIN NOT DELIVERED until S2-integration).
use crate::login::{Begin, Caller, Credential, LoginFlow, LoginUrl, Outcome, Settle};
use crate::loopback::SecretBuf;
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
/// Only the signIn success result may exceed `MAX_OUTBOUND_BYTES` (plan §5.4 arithmetic:
/// 2 x 65,536 escaped bytes + envelope < 139,264).
pub const MAX_LOGIN_RESULT_BYTES: usize = 139_264;
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
    /// signIn success result carrying the credential: posted like `Reply`, never logged,
    /// wiped (best effort) when dropped.
    Secret(SecretReply),
    /// Login URL for the host's external opener (never logged in full).
    OpenLogin(LoginUrl),
    /// Receiver codes lost to the bounded code queue (`callback-codes-suppressed <n>`).
    CallbackSuppressed(u64),
}

/// Serialized signIn success result in an owned, wiped-on-drop buffer; Debug shows its size.
pub struct SecretReply(SecretBuf);
impl SecretReply {
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(self.0.as_bytes()).unwrap_or("")
    }
}
impl std::fmt::Debug for SecretReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretReply({} bytes)", self.0.as_bytes().len())
    }
}
impl Clone for SecretReply {
    fn clone(&self) -> Self {
        Self(SecretBuf::new(self.0.as_bytes().to_vec()))
    }
}
impl PartialEq for SecretReply {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_bytes() == other.0.as_bytes()
    }
}
impl Eq for SecretReply {}

/// The signIn success result, serialized straight into one pre-sized owned buffer (no
/// intermediate `Value`/`String` copies of the tokens; reallocation cannot happen below the
/// bound). serde_json escapes `"` and `\\` as two bytes and emits other UTF-8 verbatim.
pub fn encode_login_result(
    id: u32,
    generation: u64,
    credential: &Credential,
) -> Result<SecretReply, &'static str> {
    let mut out = Vec::with_capacity(MAX_LOGIN_RESULT_BYTES);
    out.extend_from_slice(
        format!("{{\"v\":1,\"kind\":\"result\",\"id\":{id},\"gen\":{generation},\"ok\":true,\"value\":{{\"idToken\":").as_bytes(),
    );
    let mut out = SecretBuf::new(out);
    let written = (|| {
        let buf = out.vec_mut();
        serde_json::to_writer(&mut *buf, credential.id_token())?;
        buf.extend_from_slice(b",\"accessToken\":");
        serde_json::to_writer(&mut *buf, credential.access_token())?;
        buf.extend_from_slice(b"}}");
        Ok::<(), serde_json::Error>(())
    })();
    if written.is_err() || out.as_bytes().len() > MAX_LOGIN_RESULT_BYTES {
        return Err("bridge-outbound-too-large");
    }
    Ok(SecretReply(out))
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
    login: LoginFlow,
    generation: u64,
    main_frame_id: Option<u32>,
    document: DocumentState,
    /// Process-global (survives navigations), like Electron's main-process `audioWanted`.
    audio_wanted: bool,
    log: LogLimiter,
}

impl Dispatcher {
    pub fn new(config: Rc<Config>, login: LoginFlow) -> Self {
        Self::starting_at(config, login, 0)
    }
    /// Starts at an arbitrary generation (bound tests; the host always starts at 0).
    #[doc(hidden)]
    pub fn starting_at(config: Rc<Config>, login: LoginFlow, generation: u64) -> Self {
        Self {
            config,
            login,
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
    pub fn login(&self) -> &LoginFlow {
        &self.login
    }
    pub fn receive(&mut self, attr: &NativeRequest<'_>, json: &str, now_ms: u64) -> Vec<Outbound> {
        let mut out = self.log.roll(now_ms);
        match self.handle(attr, json, now_ms) {
            Ok(items) => out.extend(items),
            Err(code) => self.log.record(code, &mut out),
        }
        out
    }
    /// Periodic housekeeping (1 s host timer): log window, login expiry/TTL, then the same
    /// handoff as `drain_settlements`.
    pub fn tick(&mut self, now_ms: u64) -> Vec<Outbound> {
        let mut out = self.log.roll(now_ms);
        let expired = self.login.tick(now_ms);
        self.complete_all(expired, &mut out);
        out.extend(self.drain_settlements(now_ms));
        out
    }
    /// The ONLY UI-thread handoff of receiver results (host `WM_APP_LOGIN`, example notify,
    /// and `tick`): queued settlements and a delivered credential, then receiver codes through
    /// the shared log budget.
    pub fn drain_settlements(&mut self, now_ms: u64) -> Vec<Outbound> {
        let mut out = self.log.roll(now_ms);
        let settled = self.login.take_settlements(now_ms);
        self.complete_all(settled, &mut out);
        let (codes, overflow) = self.login.take_codes();
        for code in codes {
            self.log.record(code, &mut out);
        }
        if overflow > 0 {
            out.push(Outbound::CallbackSuppressed(overflow));
        }
        out
    }
    /// Host exit / example quit: settles any pending signIn ("Login cancelado.") and signals
    /// every receiver thread; never joins.
    pub fn shutdown(&mut self, now_ms: u64) -> Vec<Outbound> {
        let mut out = Vec::new();
        let settled = self.login.shutdown();
        self.complete_all(settled, &mut out);
        out.extend(self.drain_settlements(now_ms));
        out
    }
    fn complete_all(&mut self, settled: Vec<Settle>, out: &mut Vec<Outbound>) {
        for settle in settled {
            out.extend(self.complete(settle));
        }
    }
    /// Encodes the single result for a settled signIn caller, only for the CURRENT, enabled
    /// document; otherwise the outcome (and any credential) is discarded.
    pub fn complete(&mut self, settle: Settle) -> Vec<Outbound> {
        let mut out = vec![Outbound::Code(settle.code)];
        let Caller { generation, id } = settle.caller;
        if generation != self.generation || self.document == DocumentState::Disabled {
            out.push(Outbound::Code("login-result-dropped-stale"));
            return out; // `settle` dropped here: SecretBufs wiped
        }
        let item = match &settle.outcome {
            Outcome::Credential(credential) => {
                match encode_login_result(id, generation, credential) {
                    Ok(secret) => Outbound::Secret(secret),
                    Err(code) => {
                        out.push(Outbound::Code(code));
                        match self.result(
                            id,
                            json!({"ok": false, "error": crate::login::TEXT_MISSING_CREDENTIAL}),
                        ) {
                            Ok(reply) => Outbound::Reply(reply),
                            Err(code) => Outbound::Code(code),
                        }
                    }
                }
            }
            Outcome::Error(text) => self.reply_item(id, json!({"ok": false, "error": text})),
        };
        out.push(item);
        out.push(Outbound::Code("login-result-posted"));
        out
    }
    /// Never fails after settlements were collected into the same batch (exactly-once): an
    /// encoding error becomes its fixed code instead of discarding the batch.
    fn reply_item(&self, id: u32, reply: Value) -> Outbound {
        match self.result(id, reply) {
            Ok(text) => Outbound::Reply(text),
            Err(code) => Outbound::Code(code),
        }
    }
    fn result(&self, id: u32, mut reply: Value) -> Result<String, &'static str> {
        reply["v"] = json!(1);
        reply["kind"] = json!("result");
        reply["id"] = json!(id);
        reply["gen"] = json!(self.generation);
        encode(&reply)
    }

    fn handle(
        &mut self,
        attr: &NativeRequest<'_>,
        json: &str,
        now_ms: u64,
    ) -> Result<Vec<Outbound>, &'static str> {
        if !self.config.authorize(attr, self.generation) {
            return Err("bridge-unauthorized");
        }
        if self.document == DocumentState::Disabled {
            return Err("bridge-disabled");
        }
        let call = match parse(json)? {
            Inbound::Hello if self.document == DocumentState::AwaitingHello => {
                self.document = DocumentState::Ready { last_id: 0 };
                let ready = encode(&json!({"v": 1, "kind": "ready", "gen": self.generation}))?;
                return Ok(vec![Outbound::Reply(ready)]);
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
        let caller = Caller {
            generation: self.generation,
            id: call.id,
        };
        let mut out = Vec::new();
        match call.method {
            BridgeMethod::SignIn => {
                let origin = self.config.app_origin();
                let Begin { started, settled } = self.login.begin(caller, &origin, now_ms);
                self.complete_all(settled, &mut out);
                match started {
                    Ok(url) => {
                        out.push(Outbound::Code("login-begin"));
                        out.push(Outbound::OpenLogin(url));
                    }
                    Err(failure) => {
                        out.push(Outbound::Code(failure.code));
                        let reply = json!({"ok": false, "error": failure.text});
                        out.push(self.reply_item(call.id, reply));
                    }
                }
            }
            BridgeMethod::SignInCancel => {
                let settled = self.login.cancel(now_ms);
                self.complete_all(settled, &mut out);
                out.push(self.reply_item(call.id, json!({"ok": true})));
            }
            method => {
                let reply = self.answer(method, &call.args);
                out.push(Outbound::Reply(self.result(call.id, reply)?));
            }
        }
        Ok(out)
    }

    /// Electron's "nothing selected / not packaged" answers (plan §4.5). No events, no media.
    /// signIn/cancel never reach here (deferred login, handled in `handle`).
    fn answer(&mut self, method: BridgeMethod, args: &Args) -> Value {
        use BridgeMethod as M;
        match method {
            M::SignIn | M::SignInCancel => json!({"ok": true}),
            M::CaptureAudio => {
                if let Args::Bool(wanted) = args {
                    self.audio_wanted = *wanted;
                }
                json!({"ok": true})
            }
            M::AudioStart | M::UpdateWatch | M::GameStart => json!({"ok": true, "value": null}),
            M::UpdateInstall => json!({"ok": true, "value": false}),
            M::AudioStop | M::GameStop | M::CaptureStop => json!({"ok": true}),
        }
    }
}
