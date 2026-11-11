//! TEST-ONLY transport double (CP2 S1): drives the REAL bridge dispatcher over stdio instead of
//! WebView2. Debug-only: `required-features = ["test-host"]`, which build.rs and policy.rs reject
//! in any release build. Synthetic loopback data only; nothing is persisted. This proves the
//! JS <-> Rust protocol, NOT native WebView2 delivery, attribution or script injection.
//!
//! Usage: bridge-stdio --test-host-url http://127.0.0.1:<port>/
//! stdin lines:
//!   {"attr":{"source":<uri>,"top":<bool>},"msg":<json>}   page -> host (msg re-serialized)
//!   {"attr":{...},"raw":<string>}                          page -> host, exact JSON text
//!   content-loading [frame-id]   new document (generation + 1; frame id default 7)
//!   disable <code>               host disables the current document
//!   tick <ms>                    advance the virtual clock and run housekeeping
//!   dump                         print dispatcher state
//!   script                       print the injected script with the substituted origin
//! stdout: {"to":"page","msg":<json>} | {"dump":{..}} | {"script":".."}
//! stderr: nimbo-bridge: <fixed code> [count]
use nimbo_desktop::bridge::{Dispatcher, DocumentState, Outbound, script};
use nimbo_desktop::policy::{Config, NativeRequest};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::process::ExitCode;
use std::rc::Rc;

const MAIN_FRAME: u32 = 7;

fn page_message(d: &mut Dispatcher, line: &str, now: u64) -> Option<Vec<Outbound>> {
    let input: Value = serde_json::from_str(line).ok()?;
    let source = input["attr"]["source"].as_str()?;
    let top = input["attr"]["top"].as_bool()?;
    let text = match (input.get("msg"), input.get("raw")) {
        (Some(msg), None) => msg.to_string(),
        (None, Some(Value::String(raw))) => raw.clone(),
        _ => return None,
    };
    let main = d.main_frame_request(source);
    // A child frame: different id, parented to the main frame (never sent by the real host,
    // which subscribes only to the top-level document's WebMessageReceived).
    let attr = if top {
        main
    } else {
        NativeRequest {
            frame_id: main.main_frame_id.map(|m| m.wrapping_add(1)),
            parent_frame_id: main.main_frame_id,
            ..main
        }
    };
    Some(d.receive(&attr, &text, now))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = match args.as_slice() {
        [flag, url] if flag == "--test-host-url" => Config::test_host(url),
        _ => Err("usage: bridge-stdio --test-host-url http://127.0.0.1:<port>/"),
    };
    let config = match config {
        Ok(config) => config,
        Err(error) => {
            eprintln!("nimbo-bridge: {error}");
            return ExitCode::from(64);
        }
    };
    let origin = config.app_origin();
    let mut d = Dispatcher::new(Rc::new(config));
    let mut now = 0u64;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let words: Vec<&str> = line.split_whitespace().collect();
        let results = match words.as_slice() {
            _ if line.starts_with('{') => page_message(&mut d, &line, now),
            ["content-loading"] => {
                Some([d.begin_document(), d.set_main_frame(MAIN_FRAME)].concat())
            }
            ["content-loading", id] => id
                .parse()
                .ok()
                .map(|id| [d.begin_document(), d.set_main_frame(id)].concat()),
            ["disable", code] => Some(d.disable_document(Box::leak(code.to_string().into()))),
            ["tick", ms] => ms.parse::<u64>().ok().map(|ms| {
                now = now.saturating_add(ms);
                d.tick(now)
            }),
            ["dump"] => {
                let (state, last_id) = match d.document() {
                    DocumentState::Disabled => ("disabled", None),
                    DocumentState::AwaitingHello => ("awaiting-hello", None),
                    DocumentState::Ready { last_id } => ("ready", Some(last_id)),
                };
                let dump = json!({"audio_wanted": d.audio_wanted(), "generation": d.generation(),
                    "document": state, "last_id": last_id});
                let _ = writeln!(out, "{}", json!({ "dump": dump }));
                Some(Vec::new())
            }
            ["script"] => {
                let _ = writeln!(out, "{}", json!({ "script": script(&origin) }));
                Some(Vec::new())
            }
            _ => None,
        };
        for result in results.unwrap_or_else(|| vec![Outbound::Code("stdio-bad-line")]) {
            match result {
                Outbound::Reply(msg) => {
                    let _ = writeln!(out, "{{\"to\":\"page\",\"msg\":{msg}}}");
                }
                Outbound::Code(code) => eprintln!("nimbo-bridge: {code}"),
                Outbound::Suppressed(n) => eprintln!("nimbo-bridge: bridge-drops-suppressed {n}"),
            }
        }
        let _ = out.flush();
    }
    ExitCode::SUCCESS
}
