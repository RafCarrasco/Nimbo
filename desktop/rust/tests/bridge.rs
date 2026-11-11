//! Portable bridge v1 dispatcher (CP2 S1). These drive the same pure dispatcher the Windows host
//! uses; they are NOT evidence that WebView2 delivered, attributed or injected anything.
use nimbo_desktop::bridge::*;
use nimbo_desktop::policy::{
    BRIDGE_METHOD_NAMES, Config, Feature, HOSTED_URL, NativeRequest, feature_methods,
};
use serde_json::{Value, json};
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

fn fresh() -> Dispatcher {
    let mut d = Dispatcher::new(Rc::new(Config::release()));
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
    let mut none = Dispatcher::new(Rc::new(Config::release()));
    let attr = none.main_frame_request(HOSTED_URL);
    dropped(&none.receive(&attr, &hello, now()), "bridge-unauthorized");
    // Frame id 0 / unavailable disables the document: fail closed, no ready.
    let mut zero = Dispatcher::new(Rc::new(Config::release()));
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
    let cases = [
        (
            "nimbo:signIn",
            json!([]),
            json!({"ok":false,"error":SIGN_IN_INTERIM}),
        ),
        ("nimbo:signIn:cancel", json!([]), json!({"ok":true})),
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
    assert_eq!(cases.len(), BRIDGE_METHOD_NAMES.len());
    for (i, (method, args, expected)) in cases.into_iter().enumerate() {
        let id = i as u64 + 1;
        let out = send(&mut d, &invoke(id, g, method, args));
        let mut want = json!({"v":1,"kind":"result","id":id,"gen":g});
        for (k, v) in expected.as_object().unwrap() {
            want[k] = v.clone();
        }
        assert_eq!(replies(&out), vec![want], "{method}");
        assert!(codes(&out).is_empty());
    }
    assert_eq!(
        SIGN_IN_INTERIM,
        "Login pelo app ainda não disponível nesta prévia."
    );
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
    let mut d = Dispatcher::starting_at(Rc::new(Config::release()), limit - 2);
    d.begin_document();
    d.set_main_frame(FRAME);
    assert_eq!(d.generation(), limit - 1);
    assert_eq!(ready(&mut d), limit - 1);

    let mut d = Dispatcher::starting_at(Rc::new(Config::release()), limit - 1);
    assert_eq!(
        d.begin_document(),
        vec![Outbound::Code("bridge-generation-exhausted")]
    );
    d.set_main_frame(FRAME);
    assert_eq!(d.generation(), limit);
    assert_eq!(d.document(), DocumentState::Disabled);
    assert!(replies(&send(&mut d, &json!({"v":1,"kind":"hello"}))).is_empty());

    // At u64::MAX the generation saturates and every document stays disabled (no wrap to 0/1).
    let mut d = Dispatcher::starting_at(Rc::new(Config::release()), u64::MAX);
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
