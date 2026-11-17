//! Private picker wire contract. Not installed into any WebView by this portable module.
use super::{PickerRequest, PickerSession};
use crate::policy::{Config, NativeRequest};
use serde_json::Value;
pub enum WireMethod {
    List,
    Choose { id: String, audio: bool },
    Cancel,
    Diagnostics,
}
pub enum WireAction {
    Ready { generation: u64 },
    Call { id: u32, method: WireMethod },
}
const BAD: &str = "picker-malformed";
impl PickerSession {
    /// Native attribution is evaluated BEFORE parsing. Caller must not post an action/result
    /// until its native context is rechecked; this module creates no native source capability.
    pub fn receive(
        &mut self,
        config: &Config,
        main: &NativeRequest<'_>,
        request: &PickerRequest<'_>,
        raw: &str,
    ) -> Result<WireAction, &'static str> {
        if !self.authorized(config, main, request) {
            return Err("picker-unauthorized");
        }
        if self.generation > crate::bridge::MAX_WIRE_GENERATION {
            return Err("picker-generation-exhausted");
        }
        if raw.len() > 4096 {
            return Err(BAD);
        }
        let value: Value = serde_json::from_str(raw).map_err(|_| BAD)?;
        let m = value.as_object().ok_or(BAD)?;
        if m.get("v") != Some(&Value::from(1)) {
            return Err(BAD);
        }
        if m.get("kind").and_then(Value::as_str) == Some("picker-hello") && m.len() == 2 {
            if self.wire_ready {
                return Err("picker-duplicate-hello");
            }
            self.wire_ready = true;
            return Ok(WireAction::Ready {
                generation: self.generation,
            });
        }
        if m.len() != 6 || m.get("kind").and_then(Value::as_str) != Some("picker-invoke") {
            return Err(BAD);
        }
        if !self.wire_ready {
            return Err("picker-not-ready");
        }
        if m.get("gen").and_then(Value::as_u64) != Some(self.generation) {
            return Err("picker-stale");
        }
        let id = m
            .get("id")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n != 0)
            .ok_or(BAD)?;
        if id <= self.last_wire_id {
            return Err("picker-replayed-id");
        }
        let args = m.get("args").and_then(Value::as_array).ok_or(BAD)?;
        let method = match (m.get("method").and_then(Value::as_str), args.as_slice()) {
            (Some("list"), []) => WireMethod::List,
            (Some("cancel"), []) => WireMethod::Cancel,
            (Some("diagnostics"), []) => WireMethod::Diagnostics,
            (Some("choose"), [Value::Object(a)]) if a.len() == 2 => {
                let id = a
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty() && id.len() <= 128)
                    .ok_or(BAD)?;
                let audio = a.get("audio").and_then(Value::as_bool).ok_or(BAD)?;
                WireMethod::Choose {
                    id: id.into(),
                    audio,
                }
            }
            _ => return Err(BAD),
        };
        self.last_wire_id = id;
        Ok(WireAction::Call { id, method })
    }
}
