//! CP2 S2-core `LoginFlow` state machine (L5b): injected clock, rng, spawner and binder. These
//! prove the portable settlement/expiry/budget rules only; they are NOT evidence that a browser
//! delivered anything or that Firebase sign-in succeeded (LOGIN NOT DELIVERED until
//! S2-integration).
use nimbo_desktop::login::*;
use nimbo_desktop::loopback::{Limits, SecretBuf};
use std::io;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const ORIGIN: &str = "https://golive-ad118.web.app";

struct Rig {
    flow: LoginFlow,
    clock: Arc<AtomicU64>,
    notified: Arc<AtomicUsize>,
    rng_ok: Arc<AtomicBool>,
    spawn_ok: Arc<AtomicBool>,
    /// Order log: "published" when the spawner observed the new login already Pending.
    order: Arc<Mutex<Vec<&'static str>>>,
}

/// Spawner double: never runs the acceptor (the closure, its listener and its acceptor
/// reservation are dropped at once) unless `run_real` is set.
fn rig_with(run_real: bool, bind: Option<Bind>) -> Rig {
    let clock = Arc::new(AtomicU64::new(1_000));
    let notified = Arc::new(AtomicUsize::new(0));
    let rng_ok = Arc::new(AtomicBool::new(true));
    let spawn_ok = Arc::new(AtomicBool::new(true));
    let order = Arc::new(Mutex::new(Vec::new()));
    let flow_slot: Arc<Mutex<Option<LoginFlow>>> = Arc::new(Mutex::new(None));
    let (c, n, r) = (clock.clone(), notified.clone(), rng_ok.clone());
    let mut hooks = Hooks::new(
        Arc::new(move |buf: &mut [u8; 24]| {
            for (i, b) in buf.iter_mut().enumerate() {
                *b = i as u8 * 7 + 1;
            }
            r.load(Ordering::SeqCst)
        }),
        Arc::new(move || {
            n.fetch_add(1, Ordering::SeqCst);
            true
        }),
        Arc::new(move || c.load(Ordering::SeqCst)),
    );
    let (s, o, slot) = (spawn_ok.clone(), order.clone(), flow_slot.clone());
    hooks.spawn = Arc::new(move |job: Box<dyn FnOnce() + Send>| {
        if let Some(flow) = slot.lock().unwrap().as_ref() {
            // Called outside the flow lock: this re-entrant query would deadlock otherwise.
            if flow.is_active() {
                o.lock().unwrap().push("published");
            }
        }
        o.lock().unwrap().push("spawn");
        if !s.load(Ordering::SeqCst) {
            return Err(io::Error::other("injected spawn failure"));
        }
        if run_real {
            std::thread::spawn(job);
        }
        Ok(())
    });
    if let Some(bind) = bind {
        hooks.bind = bind;
    }
    let flow = LoginFlow::new(hooks, Scheme::Release);
    *flow_slot.lock().unwrap() = Some(flow.clone());
    Rig {
        flow,
        clock,
        notified,
        rng_ok,
        spawn_ok,
        order,
    }
}
fn rig() -> Rig {
    rig_with(false, None)
}
impl Rig {
    fn now(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }
    fn advance(&self, ms: u64) -> u64 {
        self.clock.fetch_add(ms, Ordering::SeqCst) + ms
    }
    fn begin(&self, id: u32) -> Begin {
        self.flow
            .begin(Caller { generation: 1, id }, ORIGIN, self.now())
    }
    fn deliver(&self, id_token: &str) -> CallbackDecision {
        let login = self.flow.current_login_id().expect("pending login");
        self.flow
            .callback(login, credential(id_token, "access"), self.now())
    }
}
fn credential(id: &str, access: &str) -> Credential {
    Credential::new(
        SecretBuf::new(id.as_bytes().to_vec()),
        SecretBuf::new(access.as_bytes().to_vec()),
    )
}
fn error_of(s: &Settle) -> Option<&str> {
    match &s.outcome {
        Outcome::Error(text) => Some(text.as_str()),
        Outcome::Credential(_) => None,
    }
}
fn summary(settled: &[Settle]) -> Vec<(u32, &'static str, Option<&str>)> {
    settled
        .iter()
        .map(|s| (s.caller.id, s.code, error_of(s)))
        .collect()
}

#[test]
fn electron_texts_are_byte_exact() {
    assert_eq!(TEXT_CANCELLED, "Login cancelado.");
    assert_eq!(TEXT_EXPIRED, "O login expirou. Tente de novo.");
    assert_eq!(
        TEXT_MISSING_CREDENTIAL,
        "O Google não devolveu a credencial. Tente de novo."
    );
    assert_eq!(TEXT_START_FAILED, "Não foi possível iniciar o login: ");
    assert_eq!(
        (PENDING_MS, DELIVERED_MS, ACTIVATION_MS),
        (300_000, 30_000, 120_000)
    );
}

#[test]
fn begin_publishes_pending_before_spawn_and_returns_a_loopback_login_url() {
    let r = rig();
    let b = r.begin(1);
    assert!(b.settled.is_empty());
    let url = b.started.expect("login url");
    let text = url.as_str().to_owned();
    let prefix = format!("{ORIGIN}/desktop-login?port={}&state=", url.port());
    assert!(text.starts_with(&prefix));
    let state = &text[prefix.len()..];
    assert_eq!(state.len(), 48);
    assert!(
        state
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert!(url.port() >= 1024);
    assert_eq!(
        *r.order.lock().unwrap(),
        vec!["published", "spawn"],
        "Phase B publish happens before Phase C spawn, outside the lock"
    );
    assert!(r.flow.is_active());
    // Redaction: Debug of every public value never shows state, URL or port.
    for debug in [format!("{url:?}"), format!("{:?}", r.flow)] {
        assert!(!debug.contains(state), "{debug}");
        assert!(!debug.contains("desktop-login"), "{debug}");
    }
    assert_eq!(
        redact_login_url(&text),
        format!("{prefix}<redacted>"),
        "recorder form keeps origin and port only"
    );
}

#[test]
fn rng_bind_port_and_spawn_failures_settle_the_new_caller_immediately_with_electron_text() {
    let r = rig();
    r.rng_ok.store(false, Ordering::SeqCst);
    let b = r.begin(1);
    let f = b.started.expect_err("rng failure");
    assert_eq!(
        (f.code, f.text.as_str()),
        (
            "login-server-failed",
            "Não foi possível iniciar o login: rng"
        )
    );
    assert!(!r.flow.is_active());

    let failing: Bind = Arc::new(|| Err(io::Error::new(io::ErrorKind::AddrInUse, "ocupada")));
    let r = rig_with(false, Some(failing));
    let f = r.begin(1).started.expect_err("bind failure");
    assert_eq!(f.code, "login-server-failed");
    assert_eq!(f.text, "Não foi possível iniciar o login: ocupada");

    // T13: a listener whose port is < 1024 is never published.
    let low: Bind = Arc::new(|| Ok((TcpListener::bind("127.0.0.1:0")?, 80)));
    let r = rig_with(false, Some(low));
    let f = r.begin(1).started.expect_err("low port");
    assert_eq!(
        (f.code, f.text.as_str()),
        (
            "login-port-unusable",
            "Não foi possível iniciar o login: porta"
        )
    );
    assert!(!r.flow.is_active());

    let r = rig();
    r.spawn_ok.store(false, Ordering::SeqCst);
    let f = r.begin(1).started.expect_err("spawn failure");
    assert_eq!(
        (f.code, f.text.as_str()),
        (
            "login-server-failed",
            "Não foi possível iniciar o login: thread"
        )
    );
    assert!(!r.flow.is_active(), "published Pending reverted to Idle");
    assert!(
        r.flow.take_settlements(r.now()).is_empty(),
        "no second settle"
    );
    assert_eq!(r.flow.stats().acceptors, 0, "reservation released");
}

#[test]
fn replacement_settles_previous_caller_once_pending_and_delivered_unclaimed() {
    let r = rig();
    r.begin(1).started.unwrap();
    let b = r.begin(2);
    b.started.as_ref().unwrap();
    assert_eq!(
        summary(&b.settled),
        vec![(1, "login-replaced", Some(TEXT_CANCELLED))]
    );
    assert_eq!(r.deliver("id-2"), CallbackDecision::Deliver);
    let b = r.begin(3);
    assert_eq!(
        summary(&b.settled),
        vec![(2, "login-replaced-unclaimed", Some(TEXT_CANCELLED))]
    );
    assert!(
        r.flow.take_settlements(r.now()).is_empty(),
        "returned settlements are never also queued"
    );
    assert_eq!(
        r.notified.load(Ordering::SeqCst),
        0,
        "returned settlements need no wake-up; callback() itself never notifies (the receiver \
         notifies after writing its response)"
    );
}

#[test]
fn callback_delivers_once_and_take_settlements_hands_off_exactly_once() {
    let r = rig();
    r.begin(1).started.unwrap();
    let login = r.flow.current_login_id().unwrap();
    assert!(!r.flow.activation_reserved());
    assert_eq!(r.deliver("synthetic-id"), CallbackDecision::Deliver);
    // Reserve-before-response: the slot exists as soon as the decision is returned.
    assert!(r.flow.activation_reserved());
    assert_eq!(
        r.flow.callback(login, credential("second", ""), r.now()),
        CallbackDecision::IgnoreNotPending,
        "two POSTs: exactly one Deliver"
    );
    let s = r.flow.take_settlements(r.now());
    assert_eq!(s.len(), 1);
    let Outcome::Credential(c) = &s[0].outcome else {
        panic!("credential expected")
    };
    assert_eq!((c.id_token(), c.access_token()), ("synthetic-id", "access"));
    assert!(!format!("{:?}", s[0]).contains("synthetic-id"));
    assert!(!format!("{c:?}").contains("access\""));
    assert!(
        r.flow.take_settlements(r.now()).is_empty(),
        "second take empty"
    );
    assert!(!r.flow.is_active());
    assert!(
        r.flow.activation_reserved(),
        "activation survives the handoff"
    );
    // Wrong login id never delivers.
    r.begin(2).started.unwrap();
    assert!(!r.flow.activation_reserved(), "cleared by the new login");
    assert_eq!(
        r.flow.callback(login, credential("old", ""), r.now()),
        CallbackDecision::IgnoreNotPending
    );
    assert!(r.flow.is_active());
}

#[test]
fn missing_id_token_settles_with_the_google_text() {
    let r = rig();
    r.begin(1).started.unwrap();
    assert_eq!(r.deliver(""), CallbackDecision::Deliver);
    assert_eq!(
        summary(&r.flow.take_settlements(r.now())),
        vec![(
            1,
            "login-callback-missing-credential",
            Some(TEXT_MISSING_CREDENTIAL)
        )]
    );
}

#[test]
fn cancel_pending_delivered_and_idle() {
    let r = rig();
    assert!(r.flow.cancel(r.now()).is_empty());
    r.begin(1).started.unwrap();
    assert_eq!(
        summary(&r.flow.cancel(r.now())),
        vec![(1, "login-cancelled", Some(TEXT_CANCELLED))]
    );
    assert!(r.flow.cancel(r.now()).is_empty());
    r.begin(2).started.unwrap();
    r.deliver("x");
    assert!(r.flow.activation_reserved());
    assert_eq!(
        summary(&r.flow.cancel(r.now())),
        vec![(2, "login-cancelled", Some(TEXT_CANCELLED))]
    );
    assert!(!r.flow.activation_reserved());
    assert!(r.flow.take_settlements(r.now()).is_empty());
}

#[test]
fn tick_expiry_pending_delivered_unclaimed_and_activation_ttl() {
    let r = rig();
    r.begin(1).started.unwrap();
    r.advance(PENDING_MS);
    assert!(
        r.flow.tick(r.now()).is_empty(),
        "deadline itself still valid"
    );
    r.advance(1);
    assert_eq!(
        summary(&r.flow.tick(r.now())),
        vec![(1, "login-expired", Some(TEXT_EXPIRED))]
    );
    assert!(r.flow.tick(r.now()).is_empty());

    r.begin(2).started.unwrap();
    r.deliver("x");
    r.advance(DELIVERED_MS + 1);
    assert_eq!(
        summary(&r.flow.tick(r.now())),
        vec![(2, "login-result-dropped-unclaimed", Some(TEXT_EXPIRED))]
    );
    assert!(r.flow.take_settlements(r.now()).is_empty());
    assert!(r.flow.activation_reserved());
    r.advance(ACTIVATION_MS);
    r.flow.tick(r.now());
    assert!(!r.flow.activation_reserved(), "TTL cleared");
    let (codes, overflow) = r.flow.take_codes();
    assert!(codes.contains(&"activation-expired"), "{codes:?}");
    assert_eq!(overflow, 0);
}

#[test]
fn expiry_is_checked_under_the_lock_at_callback_before_any_tick() {
    let r = rig();
    r.begin(1).started.unwrap();
    r.advance(PENDING_MS + 1);
    assert_eq!(r.deliver("late"), CallbackDecision::ExpiredAtCallback);
    assert!(
        !r.flow.activation_reserved(),
        "expired secret never accepted"
    );
    assert_eq!(r.notified.load(Ordering::SeqCst), 0, "caller notifies");
    assert_eq!(
        summary(&r.flow.take_settlements(r.now())),
        vec![(1, "login-expired-at-callback", Some(TEXT_EXPIRED))]
    );
    assert!(
        r.flow.tick(r.now()).is_empty(),
        "the tick finds nothing left"
    );
}

#[test]
fn server_failure_and_shutdown_settle_once_and_notify() {
    let r = rig();
    r.begin(1).started.unwrap();
    let login = r.flow.current_login_id().unwrap();
    r.flow.server_failed(login + 1, "outro");
    assert!(r.flow.is_active(), "a stale login id is ignored");
    r.flow.server_failed(login, "falha de teste");
    assert_eq!(r.notified.load(Ordering::SeqCst), 1);
    assert_eq!(
        summary(&r.flow.take_settlements(r.now())),
        vec![(
            1,
            "login-server-failed",
            Some("Não foi possível iniciar o login: falha de teste")
        )]
    );
    r.begin(2).started.unwrap();
    assert_eq!(
        summary(&r.flow.shutdown()),
        vec![(2, "login-shutdown", Some(TEXT_CANCELLED))]
    );
    let after = r.begin(3);
    let f = after.started.expect_err("no login after shutdown");
    assert_eq!(
        (f.code, f.text.as_str()),
        ("login-shutdown", TEXT_CANCELLED)
    );
    assert!(r.flow.shutdown().is_empty());
}

#[test]
fn acceptor_budget_is_global_and_atomic_login_busy_when_exhausted() {
    // Real acceptors parked by a spawner that runs them; replaced logins drain on their own.
    let r = rig_with(true, None);
    r.flow.set_limits(Limits {
        max_acceptors_global: 2,
        ..Limits::RELEASE
    });
    // Hold the reservations: a binder that blocks is not needed; begin+replace quickly and
    // observe that the counter never exceeds the limit and busy is reported, not spawned.
    let mut busy = 0;
    for id in 1..=200u32 {
        match r.begin(id).started {
            Ok(_) => {}
            Err(f) => {
                assert_eq!(f.code, "login-busy");
                assert_eq!(f.text, "Não foi possível iniciar o login: ocupado");
                busy += 1;
            }
        }
        let s = r.flow.stats();
        assert!(s.acceptors <= 2 && s.max_acceptors <= 2, "{s:?}");
    }
    r.flow.shutdown();
    let s = r.flow.wait_idle(Duration::from_secs(2));
    assert_eq!((s.workers, s.acceptors), (0, 0), "{s:?}");
    assert!(s.max_acceptors <= 2);
    eprintln!("busy answers under burst: {busy}");
}

#[test]
fn receiver_codes_are_bounded_with_an_overflow_count() {
    let r = rig();
    for _ in 0..(CODE_QUEUE_CAPACITY + 10) {
        r.flow.push_code("callback-state-rejected");
    }
    let (codes, overflow) = r.flow.take_codes();
    assert_eq!((codes.len(), overflow), (CODE_QUEUE_CAPACITY, 10));
    assert_eq!(r.flow.take_codes(), (Vec::new(), 0));
}

/// Property-style: random operation sequences (fixed seed); every signIn caller is settled
/// exactly once, through exactly one path (immediate begin error, returned or drained).
#[test]
fn exactly_one_settlement_per_caller_over_random_sequences() {
    let mut seed: u64 = 0x5eed_2026_1113;
    let mut next = |n: u64| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    for _round in 0..40 {
        let r = rig();
        let mut settled: std::collections::HashMap<u32, usize> = Default::default();
        fn add(map: &mut std::collections::HashMap<u32, usize>, list: Vec<Settle>) {
            for s in list {
                *map.entry(s.caller.id).or_default() += 1;
            }
        }
        let mut callers = 0u32;
        let mut old_ids = Vec::new();
        for _ in 0..60 {
            match next(9) {
                0 | 1 => {
                    callers += 1;
                    if next(10) == 0 {
                        r.spawn_ok.store(false, Ordering::SeqCst);
                    }
                    let b = r.begin(callers);
                    r.spawn_ok.store(true, Ordering::SeqCst);
                    if b.started.is_err() {
                        *settled.entry(callers).or_default() += 1;
                    }
                    add(&mut settled, b.settled);
                    if let Some(id) = r.flow.current_login_id() {
                        old_ids.push(id);
                    }
                }
                2 => add(&mut settled, r.flow.cancel(r.now())),
                3 => {
                    r.advance(next(400_000));
                    add(&mut settled, r.flow.tick(r.now()));
                }
                4 => {
                    if let Some(id) = r.flow.current_login_id() {
                        let token = if next(4) == 0 { "" } else { "t" };
                        r.flow.callback(id, credential(token, ""), r.now());
                    }
                }
                5 => {
                    if let Some(&id) = old_ids.get(next(old_ids.len().max(1) as u64) as usize) {
                        r.flow.callback(id, credential("old", ""), r.now());
                    }
                }
                6 => {
                    if let Some(id) = r.flow.current_login_id() {
                        r.flow.server_failed(id, "x");
                    }
                }
                _ => add(&mut settled, r.flow.take_settlements(r.now())),
            }
        }
        add(&mut settled, r.flow.shutdown());
        add(&mut settled, r.flow.take_settlements(r.now()));
        for id in 1..=callers {
            assert_eq!(settled.get(&id).copied(), Some(1), "caller {id}");
        }
        assert_eq!(settled.len(), callers as usize);
    }
}

/// SOURCE-POLICY PROBE (portable text match on host.rs, NOT native evidence): the Windows host
/// wires the login exactly as plan §6 says. Execution is a Windows NATIVE-GATE (windows_host.rs).
#[test]
fn host_source_wires_login_handoff_timer_shutdown_and_redaction() {
    let src = include_str!("../src/shell/windows/host.rs");
    let body = |name: &str| {
        let start = src
            .find(&format!("fn {name}("))
            .unwrap_or_else(|| panic!("fn {name} missing"));
        let end = src[start..].find("\n}\n").map_or(src.len(), |e| start + e);
        src[start..end]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .flat_map(str::split_whitespace)
            .collect::<String>()
    };
    let flat: String = src.split_whitespace().collect();
    assert!(flat.contains("constWM_APP_LOGIN:u32=WM_APP+2;"));
    assert!(flat.contains("constLOGIN_TIMER:usize=1;"));
    // CSPRNG: system-preferred BCrypt RNG.
    assert!(flat.contains("BCryptGenRandom(None,buf,BCRYPT_USE_SYSTEM_PREFERRED_RNG)"));
    // Release limits only: the host never shrinks receiver limits.
    assert!(!src.contains("set_limits"));
    // S3 (source-policy, not native): the second-instance link consumes the slot through
    // instance_proc -> copydata -> activate_link -> LoginFlow::activate.
    assert!(body("instance_proc").contains("copydata(lparam)"));
    assert!(body("copydata").contains("activate_link(&link)"));
    assert!(body("activate_link").contains(".activate(link,now_ms())"));
    let proc_ = body("main_proc");
    let login = proc_.find("WM_APP_LOGIN=>").expect("WM_APP_LOGIN handler");
    let timer = proc_.find("WM_TIMER").expect("WM_TIMER handler");
    assert!(proc_[login..].contains("drain_settlements("));
    assert!(proc_[timer..].contains(".tick("));
    // WM_DESTROY: notifier target cleared, flow stopped, THEN timer killed and controller closed.
    let destroy = &proc_[proc_.find("WM_DESTROY=>").expect("WM_DESTROY")..];
    let clear = destroy
        .find("clear_notify_target(")
        .expect("notifier cleared");
    let stop = destroy.find(".shutdown()").expect("flow stop");
    let kill = destroy.find("KillTimer(").expect("KillTimer");
    let close = destroy.find(".Close()").expect("controller close");
    assert!(clear < stop && stop < kill && kill < close);
    assert!(
        !destroy[..close].contains("wait_idle"),
        "never waits on the UI path"
    );
    // Handoff releases the dispatcher borrow before COM (`deliver`) and focuses only on a post.
    let handoff = body("login_handoff");
    let borrow = handoff.find("try_borrow_mut()").expect("borrow");
    let deliver = handoff.find("deliver(&core,").expect("deliver");
    assert!(borrow < deliver);
    assert!(handoff.contains("\"login-result-posted\""));
    assert!(handoff[deliver..].contains("focus_main()"));
    // Bounded supervision after the message loop, recorded as `shutdown workers=.. acceptors=..`.
    let run = body("run_window");
    let pump = run.find("message_loop();").expect("loop");
    let wait = run.find("wait_idle(").expect("bounded wait");
    assert!(pump < wait && run.contains("\"workers={}acceptors={}\""));
    // Timer setup failure fails closed at startup.
    assert!(body("start_webview").contains("Fatal(\"login-timer-failed\")"));
    // Recorder never sees the state: redacted external line; full URL only in login-url.txt.
    let open = body("open_external");
    assert!(open.contains("external_allowed(url)"));
    assert!(open.contains("redact_login_url(url)"));
    assert!(flat.contains("\"login-url.txt\""));
}
