//! Portable browser-login state machine (CP2 S2-core, plan §4.2-4.5). Electron parity:
//! desktop/main.cjs:58-125. One login at a time; every `nimbo:signIn` caller is settled exactly
//! once through exactly ONE path: an immediate `begin` failure, a `Settle` RETURNED by
//! `begin`/`cancel`/`tick`/`shutdown`, or a `Settle` queued by a receiver thread and drained by
//! `take_settlements`/`tick`/`shutdown` on the UI thread. Never both.
//!
//! Lock discipline: `Mutex<Inner>` is held only for short state moves. RNG, bind, spawn, socket
//! shutdown and notification happen OUTSIDE it. A "delivered" credential is NOT a successful
//! Firebase sign-in; that needs the real SDK consumer (S2-integration).
use crate::loopback::{self, Acceptor, Limits, SecretBuf};
use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

pub const TEXT_CANCELLED: &str = "Login cancelado.";
pub const TEXT_EXPIRED: &str = "O login expirou. Tente de novo.";
pub const TEXT_MISSING_CREDENTIAL: &str = "O Google não devolveu a credencial. Tente de novo.";
pub const TEXT_START_FAILED: &str = "Não foi possível iniciar o login: ";
pub const PENDING_MS: u64 = 300_000;
pub const DELIVERED_MS: u64 = 30_000;
pub const ACTIVATION_MS: u64 = 120_000;
pub const CODE_QUEUE_CAPACITY: usize = 64;
/// Lowest receiver port the web producer accepts (src/components/AuthScreens.tsx; T13).
pub const MIN_RECEIVER_PORT: u16 = 1024;

/// Fills 24 bytes from a CSPRNG; `false` on failure.
pub type Rng = Arc<dyn Fn(&mut [u8; 24]) -> bool + Send + Sync>;
/// Wakes the UI thread (never blocks); `false` when the wake-up could not be posted.
pub type Notify = Arc<dyn Fn() -> bool + Send + Sync>;
/// Login clock in ms (host: `Instant`; stdio example: virtual).
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;
pub type Spawn = Arc<dyn Fn(Box<dyn FnOnce() + Send>) -> io::Result<()> + Send + Sync>;
pub type Bind = Arc<dyn Fn() -> io::Result<(TcpListener, u16)> + Send + Sync>;

pub struct Hooks {
    pub rng: Rng,
    pub notify: Notify,
    pub clock: Clock,
    /// Thread spawner (injectable for failure/order tests).
    pub spawn: Spawn,
    /// Listener factory: always `127.0.0.1:0` in production (injectable for tests).
    pub bind: Bind,
}
impl Hooks {
    pub fn new(rng: Rng, notify: Notify, clock: Clock) -> Self {
        Self {
            rng,
            notify,
            clock,
            spawn: Arc::new(|job| {
                std::thread::Builder::new()
                    .name("nimbo-login".into())
                    .spawn(job)
                    .map(drop)
            }),
            bind: Arc::new(|| {
                let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
                let port = listener.local_addr()?.port();
                Ok((listener, port))
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caller {
    pub generation: u64,
    pub id: u32,
}

/// Validated UTF-8 tokens without C0/0x7F (loopback.rs checks before construction).
pub struct Credential {
    id_token: SecretBuf,
    access_token: SecretBuf,
}
impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credential(..)")
    }
}
impl Credential {
    pub fn new(id_token: SecretBuf, access_token: SecretBuf) -> Self {
        Self {
            id_token,
            access_token,
        }
    }
    pub fn id_token(&self) -> &str {
        std::str::from_utf8(self.id_token.as_bytes()).unwrap_or("")
    }
    pub fn access_token(&self) -> &str {
        std::str::from_utf8(self.access_token.as_bytes()).unwrap_or("")
    }
}

#[derive(Debug)]
pub enum Outcome {
    Credential(Credential),
    /// Fixed Electron text (or the server-failure prefix plus an io::Error Display).
    Error(String),
}
#[derive(Debug)]
pub struct Settle {
    pub caller: Caller,
    pub outcome: Outcome,
    /// Fixed log code of the transition that produced this settlement.
    pub code: &'static str,
}
#[derive(Debug)]
pub struct Failure {
    pub code: &'static str,
    pub text: String,
}

/// `<app origin>/desktop-login?port=<p>&state=<48 hex>`; Debug never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct LoginUrl {
    text: String,
    port: u16,
}
impl std::fmt::Debug for LoginUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LoginUrl(..)")
    }
}
impl LoginUrl {
    pub fn as_str(&self) -> &str {
        &self.text
    }
    pub fn port(&self) -> u16 {
        self.port
    }
}
/// Recorder form: everything after `state=` replaced by `<redacted>`.
pub fn redact_login_url(url: &str) -> String {
    match url.find("state=") {
        Some(i) => format!("{}<redacted>", &url[..i + 6]),
        None => url.to_owned(),
    }
}

pub struct Begin {
    pub started: Result<LoginUrl, Failure>,
    /// Settlements of a replaced caller, returned for immediate delivery (never also queued).
    pub settled: Vec<Settle>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackDecision {
    Deliver,
    IgnoreNotPending,
    /// The login was past its deadline: expired here, under the lock; credential discarded.
    ExpiredAtCallback,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub workers: usize,
    pub acceptors: usize,
    pub max_workers: usize,
    pub max_acceptors: usize,
}

enum State {
    Idle,
    Pending {
        login_id: u64,
        caller: Caller,
        state_hex: [u8; 48],
        deadline: u64,
        stop: Arc<AtomicBool>,
    },
    Delivered {
        caller: Caller,
        credential: Credential,
        deadline: u64,
    },
}
/// S3 activation slot: S2 only reserves (before any response byte), expires and clears it.
struct Reserved {
    #[expect(
        dead_code,
        reason = "consumed by the S3 activation link, not part of S2"
    )]
    state_hex: [u8; 48],
    deadline: u64,
}
struct Inner {
    next_login_id: u64,
    state: State,
    activation: Option<Reserved>,
    pending: Vec<Settle>,
    shut_down: bool,
}
#[derive(Default)]
struct Codes {
    list: Vec<&'static str>,
    overflow: u64,
}

struct Shared {
    inner: Mutex<Inner>,
    registry: Mutex<HashMap<u64, (u64, TcpStream)>>,
    codes: Mutex<Codes>,
    workers: AtomicUsize,
    acceptors: AtomicUsize,
    max_workers: AtomicUsize,
    max_acceptors: AtomicUsize,
    next_conn: AtomicU64,
    shutdown: AtomicBool,
    limits: Mutex<Limits>,
    hooks: Hooks,
}

/// RAII budget reservation (acceptor or worker); released on drop, including on spawn failure.
pub(crate) struct Slot {
    shared: Arc<Shared>,
    worker: bool,
}
impl Drop for Slot {
    fn drop(&mut self) {
        let counter = if self.worker {
            &self.shared.workers
        } else {
            &self.shared.acceptors
        };
        counter.fetch_sub(1, SeqCst);
    }
}
/// RAII connection registration; unregisters (dropping the clone) on drop.
pub(crate) struct Registration {
    shared: Arc<Shared>,
    id: u64,
}
impl Drop for Registration {
    fn drop(&mut self) {
        let removed = relock(&self.shared.registry).remove(&self.id);
        drop(removed);
    }
}

fn relock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
fn to_hex(raw: &[u8; 24]) -> [u8; 48] {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0u8; 48];
    for (i, b) in raw.iter().enumerate() {
        out[2 * i] = DIGITS[usize::from(b >> 4)];
        out[2 * i + 1] = DIGITS[usize::from(b & 15)];
    }
    out
}
fn cancelled(caller: Caller, code: &'static str) -> Settle {
    Settle {
        caller,
        outcome: Outcome::Error(TEXT_CANCELLED.to_owned()),
        code,
    }
}
fn failed(code: &'static str, detail: &str) -> Begin {
    Begin {
        started: Err(Failure {
            code,
            text: format!("{TEXT_START_FAILED}{detail}"),
        }),
        settled: Vec::new(),
    }
}

/// Cheap clonable handle shared by the UI thread and receiver threads.
#[derive(Clone)]
pub struct LoginFlow {
    shared: Arc<Shared>,
}
impl std::fmt::Debug for LoginFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LoginFlow(..)")
    }
}

impl LoginFlow {
    pub fn new(hooks: Hooks) -> Self {
        Self {
            shared: Arc::new(Shared {
                inner: Mutex::new(Inner {
                    next_login_id: 1,
                    state: State::Idle,
                    activation: None,
                    pending: Vec::new(),
                    shut_down: false,
                }),
                registry: Mutex::new(HashMap::new()),
                codes: Mutex::new(Codes::default()),
                workers: AtomicUsize::new(0),
                acceptors: AtomicUsize::new(0),
                max_workers: AtomicUsize::new(0),
                max_acceptors: AtomicUsize::new(0),
                next_conn: AtomicU64::new(1),
                shutdown: AtomicBool::new(false),
                limits: Mutex::new(Limits::RELEASE),
                hooks,
            }),
        }
    }
    /// Tests and the debug-only stdio example only; the host never calls it (source probe).
    #[doc(hidden)]
    pub fn set_limits(&self, limits: Limits) {
        *relock(&self.shared.limits) = limits;
    }
    fn limits(&self) -> Limits {
        *relock(&self.shared.limits)
    }

    /// The state lock. A poisoned lock is fatal for the login in progress: it is settled as a
    /// server failure (queued), the code is recorded, and the flow continues from Idle.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        match self.shared.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                self.shared.inner.clear_poison();
                let mut g = poisoned.into_inner();
                let caller = match std::mem::replace(&mut g.state, State::Idle) {
                    State::Pending { caller, stop, .. } => {
                        stop.store(true, SeqCst);
                        Some(caller)
                    }
                    State::Delivered { caller, .. } => Some(caller),
                    State::Idle => None,
                };
                if let Some(caller) = caller {
                    g.pending.push(Settle {
                        caller,
                        outcome: Outcome::Error(format!("{TEXT_START_FAILED}estado")),
                        code: "login-state-poisoned",
                    });
                }
                self.push_code("login-state-poisoned");
                g
            }
        }
    }

    fn reserve(&self, worker: bool) -> Option<Slot> {
        let s = &self.shared;
        let limits = self.limits();
        let (counter, max, limit) = if worker {
            (&s.workers, &s.max_workers, limits.max_workers_global)
        } else {
            (&s.acceptors, &s.max_acceptors, limits.max_acceptors_global)
        };
        let mut current = counter.load(SeqCst);
        loop {
            if current >= limit {
                return None;
            }
            match counter.compare_exchange(current, current + 1, SeqCst, SeqCst) {
                Ok(_) => {
                    max.fetch_max(current + 1, SeqCst);
                    return Some(Slot {
                        shared: s.clone(),
                        worker,
                    });
                }
                Err(seen) => current = seen,
            }
        }
    }
    pub(crate) fn reserve_worker(&self) -> Option<Slot> {
        self.reserve(true)
    }
    pub(crate) fn spawn(&self, job: Box<dyn FnOnce() + Send>) -> io::Result<()> {
        (self.shared.hooks.spawn)(job)
    }
    pub(crate) fn is_shut_down(&self) -> bool {
        self.shared.shutdown.load(SeqCst)
    }
    pub fn now(&self) -> u64 {
        (self.shared.hooks.clock)()
    }
    pub(crate) fn notify(&self) {
        if !(self.shared.hooks.notify)() {
            self.push_code("login-notify-failed");
        }
    }

    /// Registers a worker's socket clone unless its login was stopped (checked under the
    /// registry lock, after the closer set the stop flag) — no register-after-drain race.
    pub(crate) fn register(
        &self,
        login_id: u64,
        clone: TcpStream,
        stop: &AtomicBool,
    ) -> Option<Registration> {
        let mut registry = relock(&self.shared.registry);
        if stop.load(SeqCst) || self.is_shut_down() {
            return None;
        }
        let id = self.shared.next_conn.fetch_add(1, SeqCst);
        registry.insert(id, (login_id, clone));
        Some(Registration {
            shared: self.shared.clone(),
            id,
        })
    }
    /// Takes matching registry entries under the registry lock, shuts them down after unlock.
    fn close_connections(&self, login: Option<u64>) {
        let taken: Vec<TcpStream> = {
            let mut registry = relock(&self.shared.registry);
            let ids: Vec<u64> = registry
                .iter()
                .filter(|(_, (l, _))| login.is_none_or(|x| x == *l))
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| registry.remove(&id).map(|(_, s)| s))
                .collect()
        };
        for stream in taken {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    /// Phase A (no lock): rng, bind, port rule, acceptor budget. Phase B (one short lock):
    /// settle the replaced caller, publish Pending. Phase C (no lock): spawn the acceptor, then
    /// close the replaced login's connections. The URL is returned only after Phase C.
    pub fn begin(&self, caller: Caller, app_origin: &str, now: u64) -> Begin {
        if self.is_shut_down() {
            return Begin {
                started: Err(Failure {
                    code: "login-shutdown",
                    text: TEXT_CANCELLED.to_owned(),
                }),
                settled: Vec::new(),
            };
        }
        let mut bytes = [0u8; 24];
        let drawn = (self.shared.hooks.rng)(&mut bytes);
        let state_hex = to_hex(&bytes);
        loopback::wipe(&mut bytes); // best effort; the hex copy lives in the state machine
        if !drawn {
            return failed("login-server-failed", "rng");
        }
        let (listener, port) = match (self.shared.hooks.bind)() {
            Ok(bound) => bound,
            Err(e) => return failed("login-server-failed", &e.to_string()),
        };
        if port < MIN_RECEIVER_PORT {
            return failed("login-port-unusable", "porta");
        }
        if let Err(e) = listener.set_nonblocking(true) {
            return failed("login-server-failed", &e.to_string());
        }
        let Some(slot) = self.reserve(false) else {
            return failed("login-busy", "ocupado");
        };

        let stop = Arc::new(AtomicBool::new(false));
        let (login_id, settled, replaced) = {
            let mut g = self.lock();
            if g.shut_down {
                return Begin {
                    started: Err(Failure {
                        code: "login-shutdown",
                        text: TEXT_CANCELLED.to_owned(),
                    }),
                    settled: Vec::new(),
                };
            }
            let mut settled = Vec::new();
            let mut replaced = None;
            match std::mem::replace(&mut g.state, State::Idle) {
                State::Pending {
                    login_id,
                    caller,
                    stop,
                    ..
                } => {
                    stop.store(true, SeqCst);
                    settled.push(cancelled(caller, "login-replaced"));
                    replaced = Some(login_id);
                }
                State::Delivered { caller, .. } => {
                    settled.push(cancelled(caller, "login-replaced-unclaimed"));
                }
                State::Idle => {}
            }
            g.activation = None;
            let login_id = g.next_login_id;
            g.next_login_id += 1;
            g.state = State::Pending {
                login_id,
                caller,
                state_hex,
                deadline: now.saturating_add(PENDING_MS),
                stop: stop.clone(),
            };
            (login_id, settled, replaced)
        };

        let acceptor = Acceptor {
            login_id,
            expected: state_hex,
            app_origin: Arc::from(app_origin),
            listener,
            port,
            stop: stop.clone(),
            flow: self.clone(),
            limits: self.limits(),
            slot,
        };
        let spawned = self.spawn(Box::new(move || loopback::run_acceptor(acceptor)));
        if let Some(old) = replaced {
            self.close_connections(Some(old));
        }
        if spawned.is_err() {
            let mut g = self.lock();
            if matches!(g.state, State::Pending { login_id: id, .. } if id == login_id) {
                g.state = State::Idle;
            }
            stop.store(true, SeqCst);
            drop(g);
            return Begin {
                started: Err(Failure {
                    code: "login-server-failed",
                    text: format!("{TEXT_START_FAILED}thread"),
                }),
                settled,
            };
        }
        let hex = std::str::from_utf8(&state_hex).unwrap_or_default();
        Begin {
            started: Ok(LoginUrl {
                text: format!("{app_origin}/desktop-login?port={port}&state={hex}"),
                port,
            }),
            settled,
        }
    }

    /// Receiver thread, after parsing and the state check (outside the lock). Expiry is
    /// re-checked HERE under the lock; the activation slot is reserved before any response.
    pub fn callback(&self, login_id: u64, credential: Credential, now: u64) -> CallbackDecision {
        let mut g = self.lock();
        let (id, deadline) = match &g.state {
            State::Pending {
                login_id, deadline, ..
            } => (*login_id, *deadline),
            _ => return CallbackDecision::IgnoreNotPending,
        };
        if id != login_id {
            return CallbackDecision::IgnoreNotPending;
        }
        let State::Pending {
            caller,
            state_hex,
            stop,
            ..
        } = std::mem::replace(&mut g.state, State::Idle)
        else {
            unreachable!("matched Pending above under the same lock")
        };
        stop.store(true, SeqCst);
        if now > deadline {
            g.pending.push(Settle {
                caller,
                outcome: Outcome::Error(TEXT_EXPIRED.to_owned()),
                code: "login-expired-at-callback",
            });
            return CallbackDecision::ExpiredAtCallback;
        }
        g.state = State::Delivered {
            caller,
            credential,
            deadline: now.saturating_add(DELIVERED_MS),
        };
        g.activation = Some(Reserved {
            state_hex,
            deadline: now.saturating_add(ACTIVATION_MS),
        });
        CallbackDecision::Deliver
    }

    /// UI thread: drains queued settlements, then claims a delivered credential (once).
    pub fn take_settlements(&self, _now: u64) -> Vec<Settle> {
        let mut g = self.lock();
        let mut out = std::mem::take(&mut g.pending);
        if matches!(g.state, State::Delivered { .. }) {
            let State::Delivered {
                caller, credential, ..
            } = std::mem::replace(&mut g.state, State::Idle)
            else {
                unreachable!()
            };
            out.push(if credential.id_token().is_empty() {
                Settle {
                    caller,
                    outcome: Outcome::Error(TEXT_MISSING_CREDENTIAL.to_owned()),
                    code: "login-callback-missing-credential",
                }
            } else {
                Settle {
                    caller,
                    outcome: Outcome::Credential(credential),
                    code: "login-credential-claimed",
                }
            });
        }
        out
    }

    pub fn cancel(&self, _now: u64) -> Vec<Settle> {
        let (out, close) = {
            let mut g = self.lock();
            g.activation = None;
            match std::mem::replace(&mut g.state, State::Idle) {
                State::Pending {
                    login_id,
                    caller,
                    stop,
                    ..
                } => {
                    stop.store(true, SeqCst);
                    (vec![cancelled(caller, "login-cancelled")], Some(login_id))
                }
                State::Delivered { caller, .. } => {
                    (vec![cancelled(caller, "login-cancelled")], None)
                }
                State::Idle => (Vec::new(), None),
            }
        };
        if let Some(login_id) = close {
            self.close_connections(Some(login_id));
        }
        out
    }

    /// 1 s fallback: expiries, unclaimed drop, activation TTL, then queued settlements.
    pub fn tick(&self, now: u64) -> Vec<Settle> {
        let (out, close) = {
            let mut g = self.lock();
            let mut out = std::mem::take(&mut g.pending);
            let mut close = None;
            let expired = match &g.state {
                State::Pending { deadline, .. } | State::Delivered { deadline, .. } => {
                    now > *deadline
                }
                State::Idle => false,
            };
            if expired {
                match std::mem::replace(&mut g.state, State::Idle) {
                    State::Pending {
                        login_id,
                        caller,
                        stop,
                        ..
                    } => {
                        stop.store(true, SeqCst);
                        close = Some(login_id);
                        out.push(Settle {
                            caller,
                            outcome: Outcome::Error(TEXT_EXPIRED.to_owned()),
                            code: "login-expired",
                        });
                    }
                    State::Delivered { caller, .. } => out.push(Settle {
                        caller,
                        outcome: Outcome::Error(TEXT_EXPIRED.to_owned()),
                        code: "login-result-dropped-unclaimed",
                    }),
                    State::Idle => {}
                }
            }
            if g.activation.as_ref().is_some_and(|a| now > a.deadline) {
                g.activation = None;
                self.push_code("activation-expired");
            }
            (out, close)
        };
        if let Some(login_id) = close {
            self.close_connections(Some(login_id));
        }
        out
    }

    /// Acceptor thread: fatal accept error for `login_id`.
    pub fn server_failed(&self, login_id: u64, display: &str) {
        {
            let mut g = self.lock();
            if !matches!(g.state, State::Pending { login_id: id, .. } if id == login_id) {
                return;
            }
            if let State::Pending { caller, stop, .. } =
                std::mem::replace(&mut g.state, State::Idle)
            {
                stop.store(true, SeqCst);
                g.pending.push(Settle {
                    caller,
                    outcome: Outcome::Error(format!("{TEXT_START_FAILED}{display}")),
                    code: "login-server-failed",
                });
            }
        }
        self.notify();
    }

    /// UI thread at host exit: settles everything, signals every acceptor and closes every
    /// registered connection. Never joins; bounded waiting is `wait_idle`, off the UI path.
    pub fn shutdown(&self) -> Vec<Settle> {
        self.shared.shutdown.store(true, SeqCst);
        let out = {
            let mut g = self.lock();
            g.shut_down = true;
            g.activation = None;
            let mut out = std::mem::take(&mut g.pending);
            match std::mem::replace(&mut g.state, State::Idle) {
                State::Pending { caller, stop, .. } => {
                    stop.store(true, SeqCst);
                    out.push(cancelled(caller, "login-shutdown"));
                }
                State::Delivered { caller, .. } => out.push(cancelled(caller, "login-shutdown")),
                State::Idle => {}
            }
            out
        };
        self.close_connections(None);
        out
    }

    pub fn push_code(&self, code: &'static str) {
        let mut codes = relock(&self.shared.codes);
        if codes.list.len() < CODE_QUEUE_CAPACITY {
            codes.list.push(code);
        } else {
            codes.overflow = codes.overflow.saturating_add(1);
        }
    }
    pub fn take_codes(&self) -> (Vec<&'static str>, u64) {
        let mut codes = relock(&self.shared.codes);
        (
            std::mem::take(&mut codes.list),
            std::mem::take(&mut codes.overflow),
        )
    }
    pub fn stats(&self) -> Stats {
        let s = &self.shared;
        Stats {
            workers: s.workers.load(SeqCst),
            acceptors: s.acceptors.load(SeqCst),
            max_workers: s.max_workers.load(SeqCst),
            max_acceptors: s.max_acceptors.load(SeqCst),
        }
    }
    /// Bounded supervision (never on the UI thread): polls until no worker/acceptor is left or
    /// `bound` elapsed, and returns the last observation.
    pub fn wait_idle(&self, bound: Duration) -> Stats {
        let end = Instant::now() + bound;
        loop {
            let s = self.stats();
            if (s.workers == 0 && s.acceptors == 0) || Instant::now() >= end {
                return s;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[doc(hidden)]
    pub fn current_login_id(&self) -> Option<u64> {
        match self.lock().state {
            State::Pending { login_id, .. } => Some(login_id),
            _ => None,
        }
    }
    /// Pending or delivered-unclaimed.
    pub fn is_active(&self) -> bool {
        !matches!(self.lock().state, State::Idle)
    }
    /// Test-only view of the S3 activation slot (S2 has no consumer).
    #[doc(hidden)]
    pub fn activation_reserved(&self) -> bool {
        self.lock().activation.is_some()
    }
    /// TEST HOOK: a second acceptor of this flow with a never-current login id, to exercise
    /// the GLOBAL worker cap across two live listeners. Returns its port.
    #[doc(hidden)]
    pub fn spawn_test_listener(&self, app_origin: &str) -> io::Result<u16> {
        let (listener, port) = (self.shared.hooks.bind)()?;
        listener.set_nonblocking(true)?;
        let slot = self
            .reserve(false)
            .ok_or_else(|| io::Error::other("busy"))?;
        let acceptor = Acceptor {
            login_id: u64::MAX,
            expected: [0u8; 48],
            app_origin: Arc::from(app_origin),
            listener,
            port,
            stop: Arc::new(AtomicBool::new(false)),
            flow: self.clone(),
            limits: self.limits(),
            slot,
        };
        self.spawn(Box::new(move || loopback::run_acceptor(acceptor)))?;
        Ok(port)
    }
}
