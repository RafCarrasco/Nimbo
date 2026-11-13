//! Portable bridge v1 dispatcher (CP2 S1). These drive the same pure dispatcher the Windows host
//! uses; they are NOT evidence that WebView2 delivered, attributed or injected anything.
use nimbo_desktop::bridge::*;
use nimbo_desktop::login::{CallbackDecision, Caller, Credential, Hooks, LoginFlow};
use nimbo_desktop::loopback::SecretBuf;
use nimbo_desktop::policy::{
    BRIDGE_METHOD_NAMES, Config, Feature, HOSTED_URL, NativeRequest, feature_methods,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{cell::Cell, rc::Rc};

const FRAME: u32 = 7;

thread_local! {
    static CLOCK: Cell<u64> = const { Cell::new(0) };
}
/// Each helper message lands in its own log window, so per-window drop budgets never mask a
/// code in the functional tests (the budget itself is tested separately).
fn now() -> u64 {
    CLOCK.with(|c| {
        c.set(c.get() + LOG_WINDOW_MS);
        c.get()
    })
}

/// A real `LoginFlow` (real loopback bind) with a deterministic test rng. Its spawner never
/// runs the acceptor (job dropped: listener and acceptor slot released at once), so these
/// dispatcher tests never depend on receiver-thread timing or the global acceptor budget; the
/// receiver itself is covered by tests/loopback.rs and the L4 Node tier.
fn test_flow() -> LoginFlow {
    flow_with_rng(Arc::new(AtomicBool::new(true)))
}
fn flow_with_rng(ok: Arc<AtomicBool>) -> LoginFlow {
    let mut hooks = Hooks::new(
        Arc::new(move |buf: &mut [u8; 24]| {
            buf.fill(0xab);
            ok.load(Ordering::SeqCst)
        }),
        Arc::new(|| true),
        Arc::new(|| 0),
    );
    hooks.spawn = Arc::new(|job| {
        drop(job);
        Ok(())
    });
    LoginFlow::new(hooks)
}
fn release(flow: LoginFlow) -> Dispatcher {
    Dispatcher::new(Rc::new(Config::release()), flow)
}
fn fresh() -> Dispatcher {
    fresh_with(test_flow())
}
fn fresh_with(flow: LoginFlow) -> Dispatcher {
    let mut d = release(flow);
    d.begin_document();
    d.set_main_frame(FRAME);
    d
}
fn top(d: &Dispatcher) -> NativeRequest<'static> {
    d.main_frame_request(HOSTED_URL)
}
fn send(d: &mut Dispatcher, msg: &Value) -> Vec<Outbound> {
    let attr = top(d);
    d.receive(&attr, &msg.to_string(), now())
}
fn ready(d: &mut Dispatcher) -> u64 {
    let out = send(d, &json!({"v":1,"kind":"hello"}));
    let gn = d.generation();
    assert_eq!(replies(&out), vec![json!({"v":1,"kind":"ready","gen":gn})]);
    gn
}
fn replies(out: &[Outbound]) -> Vec<Value> {
    out.iter()
        .filter_map(|o| match o {
            Outbound::Reply(s) => Some(serde_json::from_str(s).expect("reply is JSON")),
            _ => None,
        })
        .collect()
}
fn codes(out: &[Outbound]) -> Vec<&'static str> {
    out.iter()
        .filter_map(|o| match o {
            Outbound::Code(c) => Some(*c),
            _ => None,
        })
        .collect()
}
fn invoke(id: u64, gn: u64, method: &str, args: Value) -> Value {
    json!({"v":1,"kind":"invoke","id":id,"gen":gn,"method":method,"args":args})
}
/// Asserts a silent drop: exactly this fixed code and no reply at all.
fn dropped(out: &[Outbound], code: &str) {
    assert!(replies(out).is_empty(), "no reply on drop: {out:?}");
    assert_eq!(codes(out), vec![code]);
}

#[test]
fn authorization_table_reuses_native_request_cases_and_never_replies() {
    let base = |d: &Dispatcher| top(d);
    let mut d = fresh();
    let hello = json!({"v":1,"kind":"hello"}).to_string();
    let g = d.generation();
    let cases: Vec<Box<dyn Fn(NativeRequest<'static>) -> NativeRequest<'static>>> = vec![
        Box::new(|r| NativeRequest {
            source: "https://evil.test/",
            ..r
        }),
        Box::new(|r| NativeRequest {
            source: "https://golive-ad118.web.app:444/",
            ..r
        }),
        Box::new(|r| NativeRequest {
            window: "picker",
            ..r
        }),
        Box::new(|r| NativeRequest {
            frame_id: Some(FRAME + 1),
            parent_frame_id: Some(FRAME),
            ..r
        }),
        Box::new(|r| NativeRequest {
            parent_frame_id: Some(9),
            ..r
        }),
        Box::new(|r| NativeRequest {
            frame_id: None,
            ..r
        }),
        Box::new(|r| NativeRequest {
            main_frame_id: None,
            ..r
        }),
        Box::new(|r| NativeRequest {
            document_generation: g + 1,
            ..r
        }),
        Box::new(|r| NativeRequest {
            document_generation: 0,
            ..r
        }),
    ];
    for case in &cases {
        let attr = case(base(&d));
        dropped(&d.receive(&attr, &hello, now()), "bridge-unauthorized");
    }
    assert_eq!(d.document(), DocumentState::AwaitingHello);
    // Attribution precedes parsing: an unauthorized oversized/garbage payload is never parsed.
    let attr = cases[0](base(&d));
    dropped(&d.receive(&attr, "{not json", now()), "bridge-unauthorized");
    // No document yet (generation 0): nothing is authorized.
    let mut none = release(test_flow());
    let attr = none.main_frame_request(HOSTED_URL);
    dropped(&none.receive(&attr, &hello, now()), "bridge-unauthorized");
    // Frame id 0 / unavailable disables the document: fail closed, no ready.
    let mut zero = release(test_flow());
    zero.begin_document();
    zero.set_main_frame(0);
    assert_eq!(zero.document(), DocumentState::Disabled);
    let attr = zero.main_frame_request(HOSTED_URL);
    assert!(replies(&zero.receive(&attr, &hello, now())).is_empty());
}

#[test]
fn state_machine_matrix() {
    // AwaitingHello: hello -> ready; invoke -> not ready.
    let mut d = fresh();
    let g = d.generation();
    dropped(
        &send(&mut d, &invoke(1, g, "nimbo:audio:start", json!([]))),
        "bridge-not-ready",
    );
    assert_eq!(ready(&mut d), g);
    assert_eq!(d.document(), DocumentState::Ready { last_id: 0 });
    // Ready: second hello dropped; invoke accepted.
    dropped(
        &send(&mut d, &json!({"v":1,"kind":"hello"})),
        "bridge-duplicate-hello",
    );
    let out = send(&mut d, &invoke(1, g, "nimbo:audio:start", json!([])));
    assert_eq!(
        replies(&out),
        vec![json!({"v":1,"kind":"result","id":1,"gen":g,"ok":true,"value":null})]
    );
    // Disabled: everything dropped.
    assert_eq!(
        d.disable_document("bridge-test"),
        vec![Outbound::Code("bridge-test")]
    );
    assert_eq!(d.document(), DocumentState::Disabled);
    dropped(
        &send(&mut d, &json!({"v":1,"kind":"hello"})),
        "bridge-disabled",
    );
    dropped(
        &send(&mut d, &invoke(2, g, "nimbo:audio:start", json!([]))),
        "bridge-disabled",
    );
    // A new document starts over at AwaitingHello with a new generation.
    d.begin_document();
    d.set_main_frame(FRAME);
    assert_eq!(d.document(), DocumentState::AwaitingHello);
    assert_eq!(d.generation(), g + 1);
}

#[test]
fn generation_bump_invalidates_outstanding_ids_and_stale_generations() {
    let mut d = fresh();
    let g1 = ready(&mut d);
    send(&mut d, &invoke(5, g1, "nimbo:capture:audio", json!([true])));
    assert!(d.audio_wanted());
    d.begin_document();
    d.set_main_frame(FRAME);
    let g2 = ready(&mut d);
    assert_eq!(g2, g1 + 1);
    assert_eq!(d.document(), DocumentState::Ready { last_id: 0 });
    // Old-generation messages are stale; a future generation is stale too.
    for gn in [g1, g2 + 1, 0] {
        dropped(
            &send(&mut d, &invoke(9, gn, "nimbo:audio:start", json!([]))),
            "bridge-stale",
        );
    }
    // Process-global state survives navigation (Electron main-process parity).
    assert!(d.audio_wanted());
    // ids restart per document.
    assert_eq!(
        replies(&send(
            &mut d,
            &invoke(1, g2, "nimbo:audio:start", json!([]))
        ))
        .len(),
        1
    );
}

#[test]
fn ids_are_strictly_increasing_and_replays_are_dropped_until_exhaustion() {
    let mut d = fresh();
    let g = ready(&mut d);
    assert_eq!(
        replies(&send(&mut d, &invoke(3, g, "nimbo:audio:start", json!([])))).len(),
        1
    );
    for id in [3, 2, 1] {
        dropped(
            &send(&mut d, &invoke(id, g, "nimbo:audio:start", json!([]))),
            "bridge-replayed-id",
        );
    }
    // A rejected (malformed) invoke never advances last_id.
    dropped(
        &send(&mut d, &invoke(10, g, "nimbo:audio:stop", json!([1]))),
        "bridge-malformed",
    );
    assert_eq!(d.document(), DocumentState::Ready { last_id: 3 });
    // The largest id is accepted once; after it nothing can be accepted (no wrap/reuse).
    let max = u64::from(u32::MAX);
    assert_eq!(
        replies(&send(
            &mut d,
            &invoke(max, g, "nimbo:audio:start", json!([]))
        ))
        .len(),
        1
    );
    dropped(
        &send(&mut d, &invoke(max, g, "nimbo:audio:start", json!([]))),
        "bridge-replayed-id",
    );
    for id in [json!(max + 1), json!(0), json!(-1), json!(1.5), json!("4")] {
        let msg =
            json!({"v":1,"kind":"invoke","id":id,"gen":g,"method":"nimbo:audio:start","args":[]});
        dropped(&send(&mut d, &msg), "bridge-malformed");
    }
}

#[test]
fn member_answers_are_electron_nothing_selected_values() {
    let mut d = fresh();
    let g = ready(&mut d);
    // signIn is DEFERRED (no immediate result) and the cancel settles it first: both have
    // dedicated tests below; here they only complete the exhaustive table.
    let out = send(&mut d, &invoke(1, g, "nimbo:signIn", json!([])));
    assert!(replies(&out).is_empty(), "no immediate signIn result");
    assert_eq!(codes(&out), vec!["login-begin"]);
    let out = send(&mut d, &invoke(2, g, "nimbo:signIn:cancel", json!([])));
    assert_eq!(
        replies(&out),
        vec![
            json!({"v":1,"kind":"result","id":1,"gen":g,"ok":false,"error":"Login cancelado."}),
            json!({"v":1,"kind":"result","id":2,"gen":g,"ok":true}),
        ]
    );
    let cases = [
        ("nimbo:capture:audio", json!([true]), json!({"ok":true})),
        (
            "nimbo:audio:start",
            json!([]),
            json!({"ok":true,"value":null}),
        ),
        ("nimbo:audio:stop", json!(["a"]), json!({"ok":true})),
        (
            "nimbo:update:watch",
            json!([]),
            json!({"ok":true,"value":null}),
        ),
        (
            "nimbo:update:install",
            json!([]),
            json!({"ok":true,"value":false}),
        ),
        (
            "nimbo:game:start",
            json!([]),
            json!({"ok":true,"value":null}),
        ),
        ("nimbo:game:stop", json!(["w"]), json!({"ok":true})),
        ("nimbo:capture:stop", json!([]), json!({"ok":true})),
    ];
    assert_eq!(cases.len() + 2, BRIDGE_METHOD_NAMES.len());
    for (i, (method, args, expected)) in cases.into_iter().enumerate() {
        let id = i as u64 + 3;
        let out = send(&mut d, &invoke(id, g, method, args));
        let mut want = json!({"v":1,"kind":"result","id":id,"gen":g});
        for (k, v) in expected.as_object().unwrap() {
            want[k] = v.clone();
        }
        assert_eq!(replies(&out), vec![want], "{method}");
        assert!(codes(&out).is_empty());
    }
    assert!(d.audio_wanted());
    send(
        &mut d,
        &invoke(20, g, "nimbo:capture:audio", json!([false])),
    );
    assert!(!d.audio_wanted());
}

#[test]
fn allowlist_is_exhaustive_and_unknown_methods_are_dropped() {
    let mut d = fresh();
    let g = ready(&mut d);
    for f in [
        Feature::Capture,
        Feature::Audio,
        Feature::GameWatch,
        Feature::Update,
    ] {
        for m in feature_methods(f) {
            assert!(BRIDGE_METHOD_NAMES.contains(&m.name()), "{f:?}");
        }
    }
    for (i, method) in [
        "nimbo:capture:start",
        "nimbo:signin",
        "eval",
        "",
        "nimbo:update:ready",
    ]
    .into_iter()
    .enumerate()
    {
        dropped(
            &send(&mut d, &invoke(i as u64 + 1, g, method, json!([]))),
            "bridge-unknown-method",
        );
    }
}

#[test]
fn size_and_shape_limits_drop_without_reply() {
    let mut d = fresh();
    let g = ready(&mut d);
    let ok = invoke(1, g, "nimbo:audio:stop", json!(["x".repeat(128)]));
    assert_eq!(replies(&send(&mut d, &ok)).len(), 1);
    let big = "x".repeat(MAX_INBOUND_BYTES);
    let malformed = [
        invoke(2, g, "nimbo:audio:stop", json!(["x".repeat(129)])),
        invoke(2, g, "nimbo:audio:stop", json!(["é".repeat(65)])),
        invoke(2, g, "nimbo:audio:stop", json!([123])),
        invoke(2, g, "nimbo:audio:stop", json!([])),
        invoke(2, g, "nimbo:audio:stop", json!(["a", "b"])),
        invoke(2, g, "nimbo:capture:audio", json!(["true"])),
        invoke(2, g, "nimbo:capture:audio", json!([])),
        invoke(2, g, "nimbo:audio:start", json!([null])),
        invoke(2, g, "nimbo:audio:start", json!({})),
        json!({"v":2,"kind":"invoke","id":2,"gen":g,"method":"nimbo:audio:start","args":[]}),
        json!({"v":"1","kind":"hello"}),
        json!({"v":1,"kind":"result","id":2,"gen":g}),
        json!({"v":1,"kind":"invoke","id":2,"gen":g,"method":"nimbo:audio:start"}),
        json!({"v":1,"kind":"invoke","id":2,"gen":g,"method":"nimbo:audio:start","args":[],"extra":1}),
        json!({"v":1,"kind":"invoke","id":2,"gen":1.5,"method":"nimbo:audio:start","args":[]}),
        json!({"v":1,"kind":"invoke","id":2,"gen":g,"method":7,"args":[]}),
        json!({"v":1,"kind":"invoke","id":2,"gen":g,"method":"nimbo:audio:start","args":[],"pad":big}),
        json!([1, 2]),
        json!("hello"),
        json!(null),
    ];
    for msg in malformed {
        dropped(&send(&mut d, &msg), "bridge-malformed");
    }
    for raw in ["", "{", "{\"v\":1,\"kind\":\"hello\"} x", "\u{feff}{}"] {
        let attr = top(&d);
        dropped(&d.receive(&attr, raw, now()), "bridge-malformed");
    }
    assert_eq!(d.document(), DocumentState::Ready { last_id: 1 });
}

/// Pads a valid message with insignificant JSON whitespace to exactly `len` bytes: the key
/// shape stays valid, so only the inbound byte limit can reject it (Tester F1).
fn padded(msg: &Value, len: usize) -> String {
    let text = msg.to_string();
    let body = text.strip_suffix('}').expect("a JSON object");
    let out = format!("{body}{}}}", " ".repeat(len - text.len()));
    assert_eq!(out.len(), len);
    assert_eq!(
        serde_json::from_str::<Value>(&out).expect("valid JSON"),
        *msg
    );
    out
}

#[test]
fn inbound_limit_is_exactly_4096_bytes_for_valid_shaped_messages() {
    assert_eq!(MAX_INBOUND_BYTES, 4096);
    let hello = json!({"v":1,"kind":"hello"});
    assert!(parse(&padded(&hello, MAX_INBOUND_BYTES)).is_ok());
    assert_eq!(
        parse(&padded(&hello, MAX_INBOUND_BYTES + 1)).err(),
        Some("bridge-malformed")
    );
    let mut d = fresh();
    let g = d.generation();
    let attr = top(&d);
    // 4097 bytes: dropped with the fixed code, no reply, the document stays AwaitingHello.
    let out = d.receive(&attr, &padded(&hello, MAX_INBOUND_BYTES + 1), now());
    dropped(&out, "bridge-malformed");
    assert_eq!(d.document(), DocumentState::AwaitingHello);
    // Exactly 4096 bytes of the same message: accepted.
    let out = d.receive(&attr, &padded(&hello, MAX_INBOUND_BYTES), now());
    assert_eq!(replies(&out), vec![json!({"v":1,"kind":"ready","gen":g})]);
    assert!(codes(&out).is_empty());
    // The same boundary for a state-changing invoke.
    let call = invoke(5, g, "nimbo:capture:audio", json!([true]));
    let out = d.receive(&attr, &padded(&call, MAX_INBOUND_BYTES + 1), now());
    dropped(&out, "bridge-malformed");
    assert_eq!(d.document(), DocumentState::Ready { last_id: 0 });
    assert!(!d.audio_wanted());
    let out = d.receive(&attr, &padded(&call, MAX_INBOUND_BYTES), now());
    assert_eq!(
        replies(&out),
        vec![json!({"v":1,"kind":"result","id":5,"gen":g,"ok":true})]
    );
    assert!(codes(&out).is_empty());
    assert_eq!(d.document(), DocumentState::Ready { last_id: 5 });
    assert!(d.audio_wanted());
}

/// Tester F3: `hello` has exactly two keys; any extra key is malformed and activates nothing.
#[test]
fn hello_with_extra_keys_is_malformed_and_never_activates() {
    let mut d = fresh();
    let g = d.generation();
    let extras = [
        json!({"v":1,"kind":"hello","gen":1}),
        json!({"v":1,"kind":"hello","gen":g}),
        json!({"v":1,"kind":"hello","id":1}),
        json!({"v":1,"kind":"hello","nonce":"synthetic"}),
    ];
    for msg in &extras {
        dropped(&send(&mut d, msg), "bridge-malformed");
        assert_eq!(d.document(), DocumentState::AwaitingHello, "{msg}");
    }
    dropped(
        &send(&mut d, &invoke(1, g, "nimbo:audio:start", json!([]))),
        "bridge-not-ready",
    );
    assert_eq!(ready(&mut d), g);
    // In Ready the same messages are still malformed (not a duplicate hello).
    for msg in &extras {
        dropped(&send(&mut d, msg), "bridge-malformed");
    }
    assert_eq!(d.document(), DocumentState::Ready { last_id: 0 });
}

/// Tester F3: a clock that goes backwards must neither re-open the per-window budget nor lose
/// or double-count suppressed drops.
#[test]
fn clock_rollback_never_reopens_the_log_budget_and_accounting_is_exact() {
    let mut d = fresh();
    let attr = d.main_frame_request("https://evil.test/");
    let t0 = 50 * LOG_WINDOW_MS;
    let mut logged = Vec::new();
    for _ in 0..1000 {
        let out = d.receive(&attr, "{}", t0);
        assert!(replies(&out).is_empty());
        logged.extend(out);
    }
    assert_eq!(
        logged,
        vec![Outbound::Code("bridge-unauthorized"); LOG_BUDGET as usize]
    );
    let first = 1000 - u64::from(LOG_BUDGET);
    let earlier = [0, 1, t0 / 2, t0 - LOG_WINDOW_MS, t0 - 1];
    for i in 0..5000 {
        let at = earlier[i % earlier.len()];
        assert_eq!(d.receive(&attr, "{}", at), vec![], "rollback to {at}");
    }
    for at in earlier {
        assert_eq!(d.tick(at), vec![], "tick rollback to {at}");
    }
    assert_eq!(d.tick(t0 + LOG_WINDOW_MS - 1), vec![]);
    // The next forward window reports every suppressed drop exactly once.
    assert_eq!(
        d.tick(t0 + LOG_WINDOW_MS),
        vec![Outbound::Suppressed(first + 5000)]
    );
    assert_eq!(d.tick(t0 + LOG_WINDOW_MS), vec![]);
    assert_eq!(
        d.receive(&attr, "{}", t0 + LOG_WINDOW_MS),
        vec![Outbound::Code("bridge-unauthorized")]
    );
}

#[test]
fn generation_is_bounded_to_exact_javascript_integers_without_wrap() {
    let limit = 1u64 << 53;
    assert_eq!(MAX_WIRE_GENERATION, limit - 1);
    let mut d = Dispatcher::starting_at(Rc::new(Config::release()), test_flow(), limit - 2);
    d.begin_document();
    d.set_main_frame(FRAME);
    assert_eq!(d.generation(), limit - 1);
    assert_eq!(ready(&mut d), limit - 1);

    let mut d = Dispatcher::starting_at(Rc::new(Config::release()), test_flow(), limit - 1);
    assert_eq!(
        d.begin_document(),
        vec![Outbound::Code("bridge-generation-exhausted")]
    );
    d.set_main_frame(FRAME);
    assert_eq!(d.generation(), limit);
    assert_eq!(d.document(), DocumentState::Disabled);
    assert!(replies(&send(&mut d, &json!({"v":1,"kind":"hello"}))).is_empty());

    // At u64::MAX the generation saturates and every document stays disabled (no wrap to 0/1).
    let mut d = Dispatcher::starting_at(Rc::new(Config::release()), test_flow(), u64::MAX);
    d.begin_document();
    d.set_main_frame(FRAME);
    assert_eq!(d.generation(), u64::MAX);
    assert_eq!(d.document(), DocumentState::Disabled);
    assert!(replies(&send(&mut d, &json!({"v":1,"kind":"hello"}))).is_empty());
}

#[test]
fn debug_never_prints_args() {
    let secret = "synthetic-secret-arg";
    let parsed = parse(&invoke(1, 1, "nimbo:audio:stop", json!([secret])).to_string())
        .expect("valid invoke");
    let text = format!("{parsed:?}");
    assert!(!text.contains(secret), "{text}");
    assert!(text.contains("AudioStop"), "{text}");
    let mut d = fresh();
    let g = ready(&mut d);
    let out = send(&mut d, &invoke(1, g, "nimbo:audio:stop", json!([secret])));
    assert!(!format!("{d:?}").contains(secret));
    assert!(!format!("{out:?}").contains(secret));
}

#[test]
fn outbound_is_bounded() {
    assert_eq!(MAX_OUTBOUND_BYTES, 4096);
    let fits = json!({"v":1,"s":"x".repeat(4000)});
    assert!(encode(&fits).is_ok());
    let big = json!({"v":1,"s":"x".repeat(MAX_OUTBOUND_BYTES)});
    assert_eq!(encode(&big), Err("bridge-outbound-too-large"));
}

/// Host-side flood logging: drops are aggregated per window so a page cannot amplify the host
/// log, while authorization and the no-reply rule stay unchanged.
#[test]
fn drop_logging_is_rate_bounded_and_aggregated() {
    let mut d = fresh();
    let attr = d.main_frame_request("https://evil.test/");
    let mut logged = 0;
    for _ in 0..1000 {
        let out = d.receive(&attr, "{}", 10);
        assert!(replies(&out).is_empty());
        logged += out.len();
    }
    assert_eq!(logged, LOG_BUDGET as usize);
    // Next window: the suppressed count is reported once (a number, never a payload).
    let out = d.tick(10 + LOG_WINDOW_MS);
    assert_eq!(
        out,
        vec![Outbound::Suppressed(1000 - u64::from(LOG_BUDGET))]
    );
    assert!(d.tick(10 + 2 * LOG_WINDOW_MS).is_empty());
    // A new window restores the per-window budget; the summary precedes new codes.
    let out = d.receive(&attr, "{}", 10 + 2 * LOG_WINDOW_MS);
    assert_eq!(out, vec![Outbound::Code("bridge-unauthorized")]);
    for _ in 0..LOG_BUDGET + 3 {
        d.receive(&attr, "{}", 10 + 3 * LOG_WINDOW_MS);
    }
    let out = d.receive(&attr, "{}", 10 + 4 * LOG_WINDOW_MS);
    assert_eq!(
        out,
        vec![
            Outbound::Suppressed(3),
            Outbound::Code("bridge-unauthorized")
        ]
    );
    // Clock going backwards never panics or resets the budget early.
    assert!(d.tick(0).is_empty());
}

#[test]
fn injected_script_has_verbatim_guards_and_one_json_origin() {
    let release = script(&Config::release().app_origin());
    assert_eq!(
        Config::release().app_origin(),
        "https://golive-ad118.web.app"
    );
    let test = script(
        &Config::for_test("http://127.0.0.1:4173/")
            .unwrap()
            .app_origin(),
    );
    for (s, origin) in [
        (&release, "\"https://golive-ad118.web.app\""),
        (&test, "\"http://127.0.0.1:4173\""),
    ] {
        assert!(!s.contains("__APP_ORIGIN__"));
        let guards = [
            "if (window.top !== window) return;".to_owned(),
            format!("if (location.origin !== {origin}) return;"),
            "if (!window.chrome || !window.chrome.webview) return;".to_owned(),
        ];
        let at: Vec<usize> = guards
            .iter()
            .map(|g| s.find(g.as_str()).unwrap_or_else(|| panic!("{g}")))
            .collect();
        assert!(at.is_sorted(), "guards in order");
        assert_eq!(s.matches(origin).count(), 1);
        // The guards are the first statements of the bridge function.
        let body = &s[s.find("'use strict';").expect("strict mode")..at[0]];
        assert_eq!(body.trim(), "'use strict';");
    }
    let raw = include_str!("../bridge/nimbo-desktop.js");
    assert_eq!(raw.matches("__APP_ORIGIN__").count(), 1);
    for forbidden in ["eval(", "Function(", "innerHTML", "AddHostObject", "fetch("] {
        assert!(!raw.contains(forbidden), "{forbidden}");
    }
}

// ---------- CP2 S2-core: deferred `nimbo:signIn` (dispatcher side) ----------
// Portable dispatcher evidence only. A posted result is NOT a Firebase sign-in: LOGIN NOT
// DELIVERED until the real producer/consumer proof (S2-integration).

fn secrets(out: &[Outbound]) -> Vec<Value> {
    out.iter()
        .filter_map(|o| match o {
            Outbound::Secret(s) => Some(serde_json::from_str(s.as_str()).expect("JSON")),
            _ => None,
        })
        .collect()
}
fn login_urls(out: &[Outbound]) -> Vec<String> {
    out.iter()
        .filter_map(|o| match o {
            Outbound::OpenLogin(url) => Some(url.as_str().to_owned()),
            _ => None,
        })
        .collect()
}
fn deliver(flow: &LoginFlow, id_token: &str, access: &str) {
    let login = flow.current_login_id().expect("pending");
    let credential = Credential::new(
        SecretBuf::new(id_token.as_bytes().to_vec()),
        SecretBuf::new(access.as_bytes().to_vec()),
    );
    assert_eq!(
        flow.callback(login, credential, 0),
        CallbackDecision::Deliver
    );
}

#[test]
fn sign_in_is_deferred_and_the_delivered_credential_is_posted_exactly_once_by_drain() {
    let flow = test_flow();
    let mut d = fresh_with(flow.clone());
    let g = ready(&mut d);
    let out = send(&mut d, &invoke(1, g, "nimbo:signIn", json!([])));
    assert!(replies(&out).is_empty() && secrets(&out).is_empty());
    let urls = login_urls(&out);
    assert_eq!(urls.len(), 1);
    assert!(urls[0].starts_with("https://golive-ad118.web.app/desktop-login?port="));
    assert!(urls[0].ends_with(&format!("&state={}", "ab".repeat(24))));
    assert!(
        !format!("{out:?}").contains("abab"),
        "Debug redacts the login URL"
    );
    deliver(&flow, "synthetic-id", "synthetic-access");
    let out = d.drain_settlements(now());
    assert_eq!(
        secrets(&out),
        vec![json!({"v":1,"kind":"result","id":1,"gen":g,"ok":true,
            "value":{"idToken":"synthetic-id","accessToken":"synthetic-access"}})]
    );
    assert!(
        replies(&out).is_empty(),
        "the credential travels only as Secret"
    );
    assert!(codes(&out).contains(&"login-result-posted"));
    assert!(
        !format!("{out:?}").contains("synthetic"),
        "Debug redacts the credential"
    );
    assert!(d.drain_settlements(now()).is_empty(), "exactly once");
    assert!(d.tick(now()).is_empty());
    assert_eq!(d.document(), DocumentState::Ready { last_id: 1 });
}

#[test]
fn delivered_after_navigation_or_disable_is_dropped_stale_without_any_result() {
    for disable in [false, true] {
        let flow = test_flow();
        let mut d = fresh_with(flow.clone());
        let g = ready(&mut d);
        send(&mut d, &invoke(1, g, "nimbo:signIn", json!([])));
        if disable {
            d.disable_document("bridge-test-disabled");
        } else {
            d.begin_document();
            d.set_main_frame(FRAME);
        }
        deliver(&flow, "stale-id", "");
        let out = d.drain_settlements(now());
        assert!(
            replies(&out).is_empty() && secrets(&out).is_empty(),
            "{out:?}"
        );
        assert!(codes(&out).contains(&"login-result-dropped-stale"));
        assert!(!flow.is_active());
    }
}

#[test]
fn missing_id_token_cancel_replacement_and_expiry_answer_with_electron_texts() {
    let flow = test_flow();
    let mut d = fresh_with(flow.clone());
    let g = ready(&mut d);
    let err = |id: u64, text: &str| json!({"v":1,"kind":"result","id":id,"gen":g,"ok":false,"error":text});
    send(&mut d, &invoke(1, g, "nimbo:signIn", json!([])));
    deliver(&flow, "", "");
    assert_eq!(
        replies(&d.drain_settlements(now())),
        vec![err(1, "O Google não devolveu a credencial. Tente de novo.")]
    );
    // Replacement: the previous caller is settled in the same batch as the new begin.
    send(&mut d, &invoke(2, g, "nimbo:signIn", json!([])));
    let out = send(&mut d, &invoke(3, g, "nimbo:signIn", json!([])));
    assert_eq!(replies(&out), vec![err(2, "Login cancelado.")]);
    assert_eq!(login_urls(&out).len(), 1);
    // Replacement of a delivered-but-unclaimed credential.
    deliver(&flow, "unclaimed", "");
    let out = send(&mut d, &invoke(4, g, "nimbo:signIn", json!([])));
    assert_eq!(replies(&out), vec![err(3, "Login cancelado.")]);
    assert!(codes(&out).contains(&"login-replaced-unclaimed"));
    // Host lifetime 300 s on the dispatcher clock (tick), not the page's 330 s.
    let t = now();
    let begun = {
        let out = send(&mut d, &invoke(5, g, "nimbo:signIn", json!([])));
        assert_eq!(replies(&out), vec![err(4, "Login cancelado.")]);
        now()
    };
    let _ = t;
    assert!(replies(&d.tick(begun + 299_000)).is_empty());
    assert_eq!(
        replies(&d.tick(begun + 300_000 + LOG_WINDOW_MS)),
        vec![err(5, "O login expirou. Tente de novo.")]
    );
    // Cancel without a pending login: just {ok:true}.
    let out = send(&mut d, &invoke(6, g, "nimbo:signIn:cancel", json!([])));
    assert_eq!(
        replies(&out),
        vec![json!({"v":1,"kind":"result","id":6,"gen":g,"ok":true})]
    );
}

#[test]
fn begin_failure_answers_the_new_caller_immediately() {
    let ok = Arc::new(AtomicBool::new(false));
    let mut d = fresh_with(flow_with_rng(ok));
    let g = ready(&mut d);
    let out = send(&mut d, &invoke(1, g, "nimbo:signIn", json!([])));
    assert_eq!(
        replies(&out),
        vec![json!({"v":1,"kind":"result","id":1,"gen":g,"ok":false,
            "error":"Não foi possível iniciar o login: rng"})]
    );
    assert!(codes(&out).contains(&"login-server-failed"));
    assert!(login_urls(&out).is_empty());
}

#[test]
fn shutdown_settles_a_pending_sign_in_with_the_cancel_text() {
    let flow = test_flow();
    let mut d = fresh_with(flow.clone());
    let g = ready(&mut d);
    send(&mut d, &invoke(1, g, "nimbo:signIn", json!([])));
    let out = d.shutdown(now());
    assert_eq!(
        replies(&out),
        vec![json!({"v":1,"kind":"result","id":1,"gen":g,"ok":false,"error":"Login cancelado."})]
    );
    assert!(codes(&out).contains(&"login-shutdown"));
    let out = send(&mut d, &invoke(2, g, "nimbo:signIn", json!([])));
    assert_eq!(
        replies(&out),
        vec![json!({"v":1,"kind":"result","id":2,"gen":g,"ok":false,"error":"Login cancelado."})]
    );
    let st = flow.wait_idle(std::time::Duration::from_secs(2));
    assert_eq!((st.workers, st.acceptors), (0, 0));
}

#[test]
fn receiver_codes_pass_through_the_log_budget_with_an_overflow_count() {
    let flow = test_flow();
    let mut d = fresh_with(flow.clone());
    let t = now();
    for _ in 0..70 {
        flow.push_code("callback-state-rejected");
    }
    let out = d.drain_settlements(t);
    let logged = codes(&out).len();
    assert_eq!(logged, LOG_BUDGET as usize, "{out:?}");
    assert!(out.contains(&Outbound::CallbackSuppressed(6)), "{out:?}");
    // The 48 logged-budget drops are reported once in the next window.
    assert_eq!(
        d.tick(t + LOG_WINDOW_MS),
        vec![Outbound::Suppressed(64 - u64::from(LOG_BUDGET))]
    );
}

#[test]
fn login_result_encoding_bound_and_quote_escaping_arithmetic() {
    assert_eq!(MAX_LOGIN_RESULT_BYTES, 139_264);
    let quotes = "\"".repeat(65_400);
    let c = Credential::new(
        SecretBuf::new(quotes.clone().into_bytes()),
        SecretBuf::new(Vec::new()),
    );
    let s = encode_login_result(7, 3, &c).expect("fits");
    let envelope = r#"{"v":1,"kind":"result","id":7,"gen":3,"ok":true,"value":{"idToken":"","accessToken":""}}"#;
    assert_eq!(s.as_str().len(), envelope.len() + 2 * 65_400);
    let v: Value = serde_json::from_str(s.as_str()).unwrap();
    assert_eq!(v["value"]["idToken"], json!(quotes));
    // Worst case: 65,536 decoded bytes, all escaped, with the largest id and generation.
    let half = "\\".repeat(32_768);
    let c = Credential::new(
        SecretBuf::new(half.clone().into_bytes()),
        SecretBuf::new(half.into_bytes()),
    );
    let s = encode_login_result(u32::MAX, MAX_WIRE_GENERATION, &c).expect("fits");
    assert!(s.as_str().len() < MAX_LOGIN_RESULT_BYTES);
    assert!(!format!("{s:?}").contains('\\'));
    // Multibyte UTF-8 is emitted verbatim (no expansion beyond the byte count).
    let c = Credential::new(
        SecretBuf::new("€".repeat(10).into_bytes()),
        SecretBuf::new(Vec::new()),
    );
    let s = encode_login_result(1, 1, &c).unwrap();
    assert!(s.as_str().contains(&"€".repeat(10)));
    let _ = Caller {
        generation: 1,
        id: 1,
    };
}
