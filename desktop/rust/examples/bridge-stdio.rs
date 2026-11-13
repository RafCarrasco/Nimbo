//! TEST-ONLY transport double (CP2 S1, S2-core): drives the REAL bridge dispatcher and the REAL
//! loopback login receiver over stdio instead of WebView2. Debug-only: `required-features =
//! ["test-host"]`, which build.rs and policy.rs reject in any release build. Synthetic loopback
//! data only; nothing is persisted. This proves the JS <-> Rust protocol and the receiver over
//! real loopback HTTP, NOT native WebView2 delivery, attribution, script injection or a
//! Firebase sign-in (LOGIN NOT DELIVERED until S2-integration).
//!
//! Live event loop (plan §4.4): a stdin reader thread and the login notifier feed one channel;
//! a result produced by a real HTTP callback is written as soon as its `Notify` event is
//! consumed, with no further stdin line, tick or dump.
//!
//! Usage: bridge-stdio --test-host-url http://127.0.0.1:<port>/
//! stdin lines:
//!   {"attr":{"source":<uri>,"top":<bool>},"msg":<json>}   page -> host (msg re-serialized)
//!   {"attr":{...},"raw":<string>}                          page -> host, exact JSON text
//!   content-loading [frame-id]   new document (generation + 1; frame id default 7)
//!   disable <code>               host disables the current document
//!   tick <ms>                    advance the VIRTUAL login clock and run housekeeping
//!   dump                         print dispatcher state
//!   script                       print the injected script with the substituted origin
//!   limits <key>=<n> ...         REAL receiver limits for later logins (test-only)
//!   stats                        print receiver counters
//!   quit (or stdin EOF)          shutdown + bounded (2 s) wait, print the report, exit
//! stdout: {"to":"page","msg":<json>} | {"login-url":<url>} | {"dump":{..}} | {"script":".."}
//!         | {"stats":{..}} | {"shutdown":{"workers":N,"acceptors":M,"waited_ms":T}}
//! stderr: nimbo-bridge: <fixed code> [count]
use nimbo_desktop::bridge::{Dispatcher, DocumentState, Outbound, script};
use nimbo_desktop::login::{Hooks, LoginFlow};
use nimbo_desktop::loopback::Limits;
use nimbo_desktop::policy::{Config, NativeRequest};
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const MAIN_FRAME: u32 = 7;

enum Event {
    Line(String),
    Eof,
    Notify,
}

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

/// `limits connection_deadline_ms=200 write_timeout_ms=200 ...` (test-only).
fn parse_limits(words: &[&str]) -> Option<Limits> {
    let mut limits = Limits::RELEASE;
    for word in words {
        let (key, value) = word.split_once('=')?;
        let n: u64 = value.parse().ok()?;
        match key {
            "connection_deadline_ms" => limits.connection_deadline_ms = n,
            "write_timeout_ms" => limits.write_timeout_ms = n,
            "max_workers_global" => limits.max_workers_global = usize::try_from(n).ok()?,
            "max_acceptors_global" => limits.max_acceptors_global = usize::try_from(n).ok()?,
            _ => return None,
        }
    }
    Some(limits)
}

/// Unix CSPRNG through std only (plan T3 example source); a failure is reported, never faked.
fn urandom(buf: &mut [u8; 24]) -> bool {
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(buf))
        .is_ok()
}

fn emit(out: &mut impl Write, results: Vec<Outbound>) {
    for result in results {
        match result {
            Outbound::Reply(msg) => {
                let _ = writeln!(out, "{{\"to\":\"page\",\"msg\":{msg}}}");
            }
            // The transport double's stdout is consumed by the harness pipe only.
            Outbound::Secret(msg) => {
                let _ = writeln!(out, "{{\"to\":\"page\",\"msg\":{}}}", msg.as_str());
            }
            Outbound::OpenLogin(url) => {
                let _ = writeln!(out, "{}", json!({ "login-url": url.as_str() }));
            }
            Outbound::Code(code) => eprintln!("nimbo-bridge: {code}"),
            Outbound::Suppressed(n) => eprintln!("nimbo-bridge: bridge-drops-suppressed {n}"),
            Outbound::CallbackSuppressed(n) => {
                eprintln!("nimbo-bridge: callback-codes-suppressed {n}")
            }
        }
    }
    let _ = out.flush();
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
    let (tx, rx) = mpsc::channel::<Event>();
    let clock = Arc::new(AtomicU64::new(0));
    let (notify_tx, flow_clock) = (tx.clone(), clock.clone());
    let flow = LoginFlow::new(Hooks::new(
        Arc::new(urandom),
        Arc::new(move || notify_tx.send(Event::Notify).is_ok()),
        Arc::new(move || flow_clock.load(Ordering::SeqCst)),
    ));
    let mut d = Dispatcher::new(Rc::new(config), flow.clone());
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(Event::Line(line)).is_err() {
                return;
            }
        }
        let _ = tx.send(Event::Eof);
    });
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for event in rx {
        let now = clock.load(Ordering::SeqCst);
        let line = match event {
            Event::Notify => {
                emit(&mut out, d.drain_settlements(now));
                continue;
            }
            Event::Eof => "quit".to_owned(),
            Event::Line(line) => line,
        };
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
                let now = clock.fetch_add(ms, Ordering::SeqCst).saturating_add(ms);
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
            ["limits", rest @ ..] => parse_limits(rest).map(|limits| {
                flow.set_limits(limits);
                Vec::new()
            }),
            ["stats"] => {
                let s = flow.stats();
                let stats = json!({"workers": s.workers, "acceptors": s.acceptors,
                    "max_workers": s.max_workers, "max_acceptors": s.max_acceptors,
                    "registered": s.registered,
                    "active": flow.is_active(), "activation": flow.activation_reserved()});
                let _ = writeln!(out, "{}", json!({ "stats": stats }));
                Some(Vec::new())
            }
            ["quit"] => {
                emit(&mut out, d.shutdown(now));
                // Bounded supervision off any UI path: shutdown() only signalled and closed.
                let started = Instant::now();
                let s = flow.wait_idle(Duration::from_secs(2));
                let waited = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                let report = json!({"workers": s.workers, "acceptors": s.acceptors,
                    "waited_ms": waited});
                let _ = writeln!(out, "{}", json!({ "shutdown": report }));
                let _ = out.flush();
                return ExitCode::SUCCESS;
            }
            _ => None,
        };
        emit(
            &mut out,
            results.unwrap_or_else(|| vec![Outbound::Code("stdio-bad-line")]),
        );
    }
    ExitCode::SUCCESS
}
