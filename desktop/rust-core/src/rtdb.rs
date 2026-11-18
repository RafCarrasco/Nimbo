//! Cliente do Realtime Database por WebSocket: o mesmo protocolo (versão 5) que o SDK da web usa,
//! então vale para o banco de verdade e para o emulador, com as mesmas regras de acesso.
//!
//! Uma thread cuida da conexão: reconecta sozinha, refaz o login e as escutas, e avisa quando a
//! conexão cai ou volta. O que foi gravado por esta própria conexão volta do servidor como qualquer
//! outra mudança, então não há cópia local otimista: a tela mostra o que o servidor confirmou.
//!
//! Os avisos (`listen`, `on_status`) rodam na thread da conexão: devem só guardar o valor e
//! acordar quem desenha. Chamar `get`/`set` de dentro deles trava.

use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, json};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::Json;
use crate::config::Project;

/// O servidor junta os pedaços de uma mensagem grande; cada pedaço vai até este tamanho.
const MAX_FRAME: usize = 16_384;
/// Sem nada para mandar por este tempo, vai um "0" para a conexão não ser fechada no caminho.
const KEEPALIVE: Duration = Duration::from_secs(45);
/// De quanto em quanto a thread olha os pedidos de quem usa o cliente enquanto espera o servidor.
const POLL: Duration = Duration::from_millis(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const FIRST_RETRY: Duration = Duration::from_millis(500);
const MAX_RETRY: Duration = Duration::from_secs(30);
/// Um pedido (gravação, leitura única) que não teve resposta neste tempo é dado como perdido.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Valor atual do caminho (`Null` quando não há nada lá).
    Value(Json),
    /// O servidor recusou a leitura (regras de acesso) ou cancelou a escuta.
    Denied(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum DbError {
    /// As regras do banco recusaram.
    Denied,
    /// A conexão caiu antes da resposta: não dá para saber se o servidor aplicou.
    Disconnected,
    Timeout,
    Server(String),
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbError::Denied => write!(f, "sem permissão"),
            DbError::Disconnected => write!(f, "a conexão caiu"),
            DbError::Timeout => write!(f, "o servidor não respondeu"),
            DbError::Server(text) => write!(f, "{text}"),
        }
    }
}
impl std::error::Error for DbError {}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Connected(bool),
    /// O servidor não aceitou o login (ou ele venceu): quem usa o cliente deve renovar e chamar
    /// `set_token` de novo.
    AuthRejected(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ListenId(u64);

type Callback = Box<dyn FnMut(&Event) + Send>;
type StatusCallback = Box<dyn FnMut(&Status) + Send>;
type Done = Box<dyn FnOnce(Result<Json, DbError>) + Send>;
type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

enum Cmd {
    Token(Option<String>),
    Listen {
        id: u64,
        path: String,
        cb: Callback,
    },
    Unlisten(u64),
    RetryDenied,
    Status(StatusCallback),
    Request {
        action: &'static str,
        body: Json,
        done: Option<Done>,
    },
    /// Só para testes: derruba a conexão atual, como se a rede tivesse caído.
    Drop,
    Close,
}

struct Shared {
    connected: AtomicBool,
    /// Relógio do servidor menos o relógio desta máquina, em ms.
    offset_ms: AtomicI64,
    next_id: AtomicU64,
}

/// Conexão com o banco. Barata de clonar; a conexão fecha quando a última cópia some.
#[derive(Clone)]
pub struct Db {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
}

impl Db {
    pub fn connect(project: &Project) -> Db {
        let (tx, rx) = channel();
        let shared = Arc::new(Shared {
            connected: AtomicBool::new(false),
            offset_ms: AtomicI64::new(0),
            next_id: AtomicU64::new(1),
        });
        let (project, thread_shared) = (project.clone(), shared.clone());
        std::thread::Builder::new()
            .name("nimbo-rtdb".into())
            .spawn(move || Worker::new(project, thread_shared).run(&rx))
            .expect("thread do banco");
        Db { tx, shared }
    }

    /// Login desta conexão: o token do Firebase da conta, ou `None` para sair. Depois de aceito, as
    /// escutas que tinham sido recusadas são tentadas de novo.
    pub fn set_token(&self, token: Option<String>) {
        let _ = self.tx.send(Cmd::Token(token));
    }

    /// Acompanha um caminho. O aviso recebe o valor inteiro a cada mudança; se já houver valor
    /// conhecido para o caminho, ele chega na hora.
    pub fn listen(&self, path: &str, cb: impl FnMut(&Event) + Send + 'static) -> ListenId {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Listen {
            id,
            path: path.into(),
            cb: Box::new(cb),
        });
        ListenId(id)
    }

    pub fn unlisten(&self, id: ListenId) {
        let _ = self.tx.send(Cmd::Unlisten(id.0));
    }

    /// Uma leitura recusada não é refeita sozinha. Quando as permissões da conta mudam (ganhou um
    /// cargo, por exemplo), isto pede de novo o que tinha sido recusado.
    pub fn retry_denied(&self) {
        let _ = self.tx.send(Cmd::RetryDenied);
    }

    /// Avisa quando a conexão cai ou volta e quando o login é recusado. Chega já com o estado atual.
    pub fn on_status(&self, cb: impl FnMut(&Status) + Send + 'static) {
        let _ = self.tx.send(Cmd::Status(Box::new(cb)));
    }

    pub fn is_connected(&self) -> bool {
        self.shared.connected.load(Ordering::Relaxed)
    }

    /// Hora do servidor, em ms desde 1970 (o relógio desta máquina corrigido pela diferença medida
    /// ao conectar).
    pub fn server_now_ms(&self) -> i64 {
        now_ms() + self.shared.offset_ms.load(Ordering::Relaxed)
    }

    pub fn get(&self, path: &str) -> Result<Json, DbError> {
        self.wait("g", json!({ "p": clean(path), "q": {} }))
    }

    pub fn set(&self, path: &str, value: Json) -> Result<(), DbError> {
        self.wait("p", json!({ "p": clean(path), "d": value }))
            .map(drop)
    }

    /// Grava vários caminhos de uma vez (tudo ou nada). As chaves de `children` são caminhos
    /// relativos a `path`; `Null` apaga.
    pub fn update(&self, path: &str, children: Json) -> Result<(), DbError> {
        self.wait("m", json!({ "p": clean(path), "d": children }))
            .map(drop)
    }

    pub fn remove(&self, path: &str) -> Result<(), DbError> {
        self.set(path, Json::Null)
    }

    /// O que o servidor deve gravar quando esta conexão cair (presença).
    pub fn on_disconnect_set(&self, path: &str, value: Json) -> Result<(), DbError> {
        self.wait("o", json!({ "p": clean(path), "d": value }))
            .map(drop)
    }

    pub fn on_disconnect_remove(&self, path: &str) -> Result<(), DbError> {
        self.on_disconnect_set(path, Json::Null)
    }

    pub fn on_disconnect_cancel(&self, path: &str) -> Result<(), DbError> {
        self.wait("oc", json!({ "p": clean(path), "d": null }))
            .map(drop)
    }

    /// Como `set`, sem esperar: o resultado chega em `done`, na thread da conexão.
    pub fn set_then(
        &self,
        path: &str,
        value: Json,
        done: impl FnOnce(Result<(), DbError>) + Send + 'static,
    ) {
        self.send(
            "p",
            json!({ "p": clean(path), "d": value }),
            Some(Box::new(move |r| done(r.map(drop)))),
        );
    }

    pub fn close(&self) {
        let _ = self.tx.send(Cmd::Close);
    }

    /// Só para testes: derruba a conexão atual, como se a rede tivesse caído.
    #[doc(hidden)]
    pub fn debug_drop_connection(&self) {
        let _ = self.tx.send(Cmd::Drop);
    }

    fn send(&self, action: &'static str, body: Json, done: Option<Done>) {
        let _ = self.tx.send(Cmd::Request { action, body, done });
    }

    fn wait(&self, action: &'static str, body: Json) -> Result<Json, DbError> {
        let (tx, rx) = channel();
        self.send(
            action,
            body,
            Some(Box::new(move |result| drop(tx.send(result)))),
        );
        // A thread da conexão sempre responde (resultado, queda ou prazo); a folga é só garantia.
        rx.recv_timeout(REQUEST_TIMEOUT + Duration::from_secs(5))
            .unwrap_or(Err(DbError::Timeout))
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn segments(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn clean(path: &str) -> String {
    segments(path).join("/")
}

/// O banco não guarda `null` nem objeto vazio, e devolve como lista o que tem chaves 0, 1, 2...
/// Aqui tudo vira objeto (chaves "0", "1"...), para um valor ter sempre a mesma forma, venha ele
/// inteiro ou aos pedaços.
pub fn normalize(value: Json) -> Json {
    match value {
        Json::Array(items) => {
            let map: Map<String, Json> = items
                .into_iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), normalize(v)))
                .filter(|(_, v)| !v.is_null())
                .collect();
            if map.is_empty() {
                Json::Null
            } else {
                Json::Object(map)
            }
        }
        Json::Object(fields) => {
            let map: Map<String, Json> = fields
                .into_iter()
                .map(|(k, v)| (k, normalize(v)))
                .filter(|(_, v)| !v.is_null())
                .collect();
            if map.is_empty() {
                Json::Null
            } else {
                Json::Object(map)
            }
        }
        other => other,
    }
}

fn get_at(root: &Json, rel: &[String]) -> Json {
    rel.iter()
        .try_fold(root, |node, key| node.get(key))
        .cloned()
        .unwrap_or(Json::Null)
}

fn set_at(root: &mut Json, rel: &[String], value: Json) {
    let Some((key, rest)) = rel.split_first() else {
        *root = value;
        return;
    };
    if !root.is_object() {
        if value.is_null() {
            return;
        }
        *root = Json::Object(Map::new());
    }
    let Some(map) = root.as_object_mut() else {
        return;
    };
    if rest.is_empty() {
        if value.is_null() {
            map.remove(key);
        } else {
            map.insert(key.clone(), value);
        }
    } else {
        let child = map.entry(key.clone()).or_insert(Json::Null);
        set_at(child, rest, value);
        if child.is_null() {
            map.remove(key);
        }
    }
    if map.is_empty() {
        *root = Json::Null;
    }
}

#[derive(PartialEq)]
enum ListenState {
    /// Ainda não pedida ao servidor (sem conexão).
    Idle,
    Sent,
    Active,
    Denied,
}

struct Listen {
    path: Vec<String>,
    state: ListenState,
    /// O valor só vale depois que o servidor mandou o caminho inteiro pelo menos uma vez.
    complete: bool,
    value: Json,
    last: Option<Event>,
    subs: Vec<(u64, Callback)>,
}

impl Listen {
    fn emit(&mut self, event: Event) {
        if self.last.as_ref() == Some(&event) {
            return;
        }
        for (_, cb) in &mut self.subs {
            cb(&event);
        }
        self.last = Some(event);
    }
}

enum Pending {
    Auth,
    Listen(String),
    Request(Option<Done>),
}

struct Queued {
    action: &'static str,
    body: Json,
    done: Option<Done>,
    deadline: Instant,
}

enum End {
    /// Quem usa o cliente fechou (ou todas as cópias sumiram).
    Closed,
    Lost,
    /// O servidor mandou falar com outra máquina dele.
    Redirect(String),
}

struct Worker {
    project: Project,
    shared: Arc<Shared>,
    token: Option<String>,
    listens: HashMap<String, Listen>,
    statuses: Vec<StatusCallback>,
    pending: HashMap<u64, (Pending, Instant)>,
    outbox: VecDeque<Queued>,
    request: u64,
    ready: bool,
    last_write: Instant,
}

impl Worker {
    fn new(project: Project, shared: Arc<Shared>) -> Self {
        Self {
            project,
            shared,
            token: None,
            listens: HashMap::new(),
            statuses: Vec::new(),
            pending: HashMap::new(),
            outbox: VecDeque::new(),
            request: 0,
            ready: false,
            last_write: Instant::now(),
        }
    }

    fn run(mut self, rx: &Receiver<Cmd>) {
        let mut retry = FIRST_RETRY;
        let mut host = self.project.db_host.clone();
        loop {
            match open(&self.project, &host) {
                Ok(mut socket) => {
                    let end = self.session(&mut socket, rx);
                    let was_ready = self.ready;
                    self.lost();
                    match end {
                        End::Closed => return,
                        End::Redirect(to) => {
                            host = to;
                            continue;
                        }
                        End::Lost => {
                            if was_ready {
                                retry = FIRST_RETRY;
                            }
                        }
                    }
                }
                // A máquina indicada pelo servidor pode ter saído do ar: volta para o endereço padrão.
                Err(_) => host = self.project.db_host.clone(),
            }
            let until = Instant::now() + retry;
            loop {
                let left = until.saturating_duration_since(Instant::now());
                match rx.recv_timeout(left) {
                    Ok(Cmd::Close) | Err(RecvTimeoutError::Disconnected) => return,
                    Ok(cmd) => {
                        self.command(cmd, None);
                    }
                    Err(RecvTimeoutError::Timeout) => break,
                }
                self.expire();
            }
            self.expire();
            retry = (retry * 2).min(MAX_RETRY);
        }
    }

    fn session(&mut self, socket: &mut Socket, rx: &Receiver<Cmd>) -> End {
        let mut parts: Option<(usize, String)> = None;
        let started = Instant::now();
        self.last_write = started;
        loop {
            // Conectou mas o servidor não deu as boas-vindas: tenta de novo.
            if !self.ready && started.elapsed() >= HANDSHAKE_TIMEOUT {
                return End::Lost;
            }
            loop {
                match rx.try_recv() {
                    Ok(Cmd::Close) | Err(TryRecvError::Disconnected) => {
                        let _ = socket.close(None);
                        let _ = socket.flush();
                        return End::Closed;
                    }
                    Ok(cmd) => {
                        if !self.command(cmd, Some(socket)) {
                            return End::Lost;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
            self.expire();
            if self.ready
                && self.last_write.elapsed() >= KEEPALIVE
                && !self.write(socket, "0".into())
            {
                return End::Lost;
            }
            let text = match socket.read() {
                Ok(Message::Text(text)) => text.to_string(),
                Ok(Message::Close(_)) => return End::Lost,
                Ok(_) => continue,
                Err(tungstenite::Error::Io(e))
                    if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    continue;
                }
                Err(_) => return End::Lost,
            };
            // Mensagem grande chega como um número (quantos pedaços) seguido dos pedaços.
            let whole = match parts.take() {
                Some((left, mut buffer)) => {
                    buffer.push_str(&text);
                    if left > 1 {
                        parts = Some((left - 1, buffer));
                        continue;
                    }
                    buffer
                }
                None => match text.parse::<usize>() {
                    Ok(count) if text.len() <= 6 => {
                        if count > 0 {
                            parts = Some((count, String::new()));
                        }
                        continue;
                    }
                    _ => text,
                },
            };
            let Ok(message) = serde_json::from_str::<Json>(&whole) else {
                return End::Lost;
            };
            match self.message(socket, &message) {
                Ok(None) => {}
                Ok(Some(end)) => return end,
                Err(()) => return End::Lost,
            }
        }
    }

    /// Trata um pedido de quem usa o cliente. `false` quando a conexão falhou ao mandar.
    fn command(&mut self, cmd: Cmd, mut socket: Option<&mut Socket>) -> bool {
        let live = self.ready && socket.is_some();
        match cmd {
            Cmd::Close => {}
            Cmd::Drop => return socket.is_none(),
            Cmd::Token(token) => {
                self.token = token;
                if live && let Some(socket) = socket.as_deref_mut() {
                    return self.send_auth(socket);
                }
            }
            Cmd::Status(mut cb) => {
                cb(&Status::Connected(self.ready));
                self.statuses.push(cb);
            }
            Cmd::Listen { id, path, mut cb } => {
                let parts = segments(&path);
                let key = parts.join("/");
                if let Some(listen) = self.listens.get_mut(&key) {
                    if let Some(event) = &listen.last {
                        cb(event);
                    }
                    listen.subs.push((id, cb));
                    return true;
                }
                // Um caminho acima já acompanhado e completo traz o valor na hora.
                let seed = self
                    .listens
                    .values()
                    .filter(|l| {
                        l.complete && parts.len() > l.path.len() && parts.starts_with(&l.path)
                    })
                    .max_by_key(|l| l.path.len())
                    .map(|l| get_at(&l.value, &parts[l.path.len()..]));
                let mut listen = Listen {
                    path: parts,
                    state: ListenState::Idle,
                    complete: seed.is_some(),
                    value: seed.clone().unwrap_or(Json::Null),
                    last: None,
                    subs: vec![(id, cb)],
                };
                if let Some(value) = seed {
                    listen.emit(Event::Value(value));
                }
                self.listens.insert(key.clone(), listen);
                if live && let Some(socket) = socket.as_deref_mut() {
                    return self.send_listen(socket, &key);
                }
            }
            Cmd::Unlisten(id) => {
                let found = self
                    .listens
                    .iter_mut()
                    .find(|(_, l)| l.subs.iter().any(|(sub, _)| *sub == id))
                    .map(|(key, listen)| {
                        listen.subs.retain(|(sub, _)| *sub != id);
                        (key.clone(), listen.subs.is_empty())
                    });
                if let Some((key, true)) = found
                    && let Some(listen) = self.listens.remove(&key)
                    && matches!(listen.state, ListenState::Sent | ListenState::Active)
                    && live
                    && let Some(socket) = socket.as_deref_mut()
                {
                    return self.request(socket, "n", json!({ "p": key }), Pending::Request(None));
                }
            }
            Cmd::RetryDenied => {
                if live && let Some(socket) = socket.as_deref_mut() {
                    return self.retry_denied(socket);
                }
            }
            Cmd::Request { action, body, done } => {
                if live && let Some(socket) = socket.as_deref_mut() {
                    return self.request(socket, action, body, Pending::Request(done));
                }
                self.outbox.push_back(Queued {
                    action,
                    body,
                    done,
                    deadline: Instant::now() + REQUEST_TIMEOUT,
                });
            }
        }
        true
    }

    fn retry_denied(&mut self, socket: &mut Socket) -> bool {
        let denied: Vec<String> = self
            .listens
            .iter()
            .filter(|(_, l)| l.state == ListenState::Denied)
            .map(|(key, _)| key.clone())
            .collect();
        denied.iter().all(|key| self.send_listen(socket, key))
    }

    /// Pedidos que esperaram demais (sem conexão ou sem resposta) terminam com `Timeout`.
    fn expire(&mut self) {
        let now = Instant::now();
        while self.outbox.front().is_some_and(|q| q.deadline <= now) {
            if let Some(Queued {
                done: Some(done), ..
            }) = self.outbox.pop_front()
            {
                done(Err(DbError::Timeout));
            }
        }
        let late: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, (kind, since))| {
                matches!(kind, Pending::Request(_)) && now - *since >= REQUEST_TIMEOUT
            })
            .map(|(id, _)| *id)
            .collect();
        for id in late {
            if let Some((Pending::Request(Some(done)), _)) = self.pending.remove(&id) {
                done(Err(DbError::Timeout));
            }
        }
    }

    fn lost(&mut self) {
        self.ready = false;
        self.shared.connected.store(false, Ordering::Relaxed);
        for (_, (pending, _)) in self.pending.drain() {
            if let Pending::Request(Some(done)) = pending {
                done(Err(DbError::Disconnected));
            }
        }
        for listen in self.listens.values_mut() {
            listen.state = ListenState::Idle;
        }
        self.status(&Status::Connected(false));
    }

    fn status(&mut self, status: &Status) {
        for cb in &mut self.statuses {
            cb(status);
        }
    }

    fn write(&mut self, socket: &mut Socket, text: String) -> bool {
        self.last_write = Instant::now();
        if text.len() <= MAX_FRAME {
            return socket.send(Message::text(text)).is_ok();
        }
        let mut chunks = Vec::new();
        let mut rest = text.as_str();
        while !rest.is_empty() {
            let mut cut = rest.len().min(MAX_FRAME);
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            let (chunk, tail) = rest.split_at(cut);
            chunks.push(chunk.to_owned());
            rest = tail;
        }
        socket.send(Message::text(chunks.len().to_string())).is_ok()
            && chunks
                .into_iter()
                .all(|chunk| socket.send(Message::text(chunk)).is_ok())
    }

    fn request(&mut self, socket: &mut Socket, action: &str, body: Json, pending: Pending) -> bool {
        self.request += 1;
        self.pending.insert(self.request, (pending, Instant::now()));
        let message = json!({ "t": "d", "d": { "r": self.request, "a": action, "b": body } });
        self.write(socket, message.to_string())
    }

    fn send_auth(&mut self, socket: &mut Socket) -> bool {
        match self.token.clone() {
            Some(token) => self.request(socket, "auth", json!({ "cred": token }), Pending::Auth),
            None => self.request(socket, "unauth", json!({}), Pending::Request(None)),
        }
    }

    fn send_listen(&mut self, socket: &mut Socket, key: &str) -> bool {
        if let Some(listen) = self.listens.get_mut(key) {
            listen.state = ListenState::Sent;
        }
        self.request(
            socket,
            "q",
            json!({ "p": key, "h": "" }),
            Pending::Listen(key.to_owned()),
        )
    }

    fn message(&mut self, socket: &mut Socket, message: &Json) -> Result<Option<End>, ()> {
        let body = &message["d"];
        if message["t"] == "c" {
            return self.control(socket, body);
        }
        if let Some(id) = body["r"].as_u64() {
            let result = &body["b"];
            let status = result["s"].as_str().unwrap_or("");
            match self.pending.remove(&id) {
                Some((Pending::Auth, _)) => {
                    if status == "ok" {
                        if !self.retry_denied(socket) {
                            return Err(());
                        }
                    } else {
                        let reason = result["d"].as_str().unwrap_or(status).to_owned();
                        self.status(&Status::AuthRejected(reason));
                    }
                }
                Some((Pending::Listen(key), _)) => {
                    if let Some(listen) = self.listens.get_mut(&key) {
                        if status == "ok" {
                            listen.state = ListenState::Active;
                            if !listen.complete {
                                listen.complete = true;
                                listen.value = Json::Null;
                            }
                            let value = listen.value.clone();
                            listen.emit(Event::Value(value));
                        } else {
                            listen.state = ListenState::Denied;
                            listen.complete = false;
                            listen.emit(Event::Denied(status.to_owned()));
                        }
                    }
                }
                Some((Pending::Request(Some(done)), _)) => done(match status {
                    "ok" => Ok(normalize(result["d"].clone())),
                    "permission_denied" => Err(DbError::Denied),
                    other => Err(DbError::Server(match result["d"].as_str() {
                        Some(detail) => format!("{other}: {detail}"),
                        None => other.to_owned(),
                    })),
                }),
                _ => {}
            }
            return Ok(None);
        }
        let data = &body["b"];
        match body["a"].as_str() {
            Some("d") => self.apply(
                &segments(data["p"].as_str().unwrap_or("")),
                &data["d"],
                false,
            ),
            Some("m") => self.apply(
                &segments(data["p"].as_str().unwrap_or("")),
                &data["d"],
                true,
            ),
            Some("c") => {
                let key = clean(data["p"].as_str().unwrap_or(""));
                if let Some(listen) = self.listens.get_mut(&key) {
                    listen.state = ListenState::Denied;
                    listen.complete = false;
                    listen.emit(Event::Denied("permission_denied".into()));
                }
            }
            Some("ac") => {
                let reason = data["d"]
                    .as_str()
                    .or(data["s"].as_str())
                    .unwrap_or("expired")
                    .to_owned();
                self.status(&Status::AuthRejected(reason));
            }
            _ => {}
        }
        Ok(None)
    }

    fn control(&mut self, socket: &mut Socket, body: &Json) -> Result<Option<End>, ()> {
        match body["t"].as_str() {
            // Boas-vindas: a conexão está pronta. Refaz o login e as escutas e solta o que esperava.
            Some("h") => {
                if let Some(server) = body["d"]["ts"].as_i64() {
                    self.shared
                        .offset_ms
                        .store(server - now_ms(), Ordering::Relaxed);
                }
                self.ready = true;
                self.shared.connected.store(true, Ordering::Relaxed);
                let mut ok = self.token.is_none() || self.send_auth(socket);
                let keys: Vec<String> = self.listens.keys().cloned().collect();
                for key in keys {
                    ok = ok && self.send_listen(socket, &key);
                }
                while ok && let Some(queued) = self.outbox.pop_front() {
                    ok = self.request(
                        socket,
                        queued.action,
                        queued.body,
                        Pending::Request(queued.done),
                    );
                }
                if !ok {
                    return Err(());
                }
                self.status(&Status::Connected(true));
            }
            Some("r") => {
                if let Some(host) = body["d"].as_str() {
                    return Ok(Some(End::Redirect(host.to_owned())));
                }
            }
            Some("s") => return Ok(Some(End::Lost)),
            Some("p") => {
                if !self.write(
                    socket,
                    json!({ "t": "c", "d": { "t": "o", "d": {} } }).to_string(),
                ) {
                    return Err(());
                }
            }
            _ => {}
        }
        Ok(None)
    }

    /// Aplica uma mudança vinda do servidor a todas as escutas que ela alcança: as que ficam no
    /// caminho ou acima dele (mudam um pedaço) e as que ficam abaixo (recebem a sua parte).
    fn apply(&mut self, path: &[String], data: &Json, merge: bool) {
        // Um "merge" é uma lista de gravações em caminhos relativos; uma gravação simples é a lista de uma só.
        let writes: Vec<(Vec<String>, Json)> = if merge {
            data.as_object()
                .map(|children| {
                    children
                        .iter()
                        .map(|(key, value)| {
                            let mut full = path.to_vec();
                            full.extend(segments(key));
                            (full, normalize(value.clone()))
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            vec![(path.to_vec(), normalize(data.clone()))]
        };
        for listen in self.listens.values_mut() {
            let mut touched = false;
            for (at, value) in &writes {
                if at.starts_with(&listen.path) {
                    let rel = &at[listen.path.len()..];
                    if rel.is_empty() {
                        listen.complete = true;
                    }
                    if listen.complete {
                        set_at(&mut listen.value, rel, value.clone());
                        touched = true;
                    }
                } else if listen.path.starts_with(at) {
                    listen.value = get_at(value, &listen.path[at.len()..]);
                    listen.complete = true;
                    touched = true;
                }
            }
            if touched {
                let value = listen.value.clone();
                listen.emit(Event::Value(value));
            }
        }
    }
}

fn open(project: &Project, host: &str) -> Result<Socket, String> {
    let scheme = if project.db_secure { "wss" } else { "ws" };
    let url = format!("{scheme}://{host}/.ws?v=5&ns={}", project.db_namespace);
    let address = if host.contains(':') {
        host.to_owned()
    } else {
        format!("{host}:{}", if project.db_secure { 443 } else { 80 })
    };
    let mut last = String::from("endereço do banco não encontrado");
    for addr in address.to_socket_addrs().map_err(|e| e.to_string())? {
        let stream = match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => stream,
            Err(e) => {
                last = e.to_string();
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
        let _ = stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT));
        match tungstenite::client_tls(url.as_str(), stream) {
            Ok((mut socket, _)) => {
                let tcp = match socket.get_mut() {
                    MaybeTlsStream::Plain(tcp) => Some(&*tcp),
                    MaybeTlsStream::NativeTls(tls) => Some(tls.get_ref()),
                    _ => None,
                };
                if let Some(tcp) = tcp {
                    let _ = tcp.set_read_timeout(Some(POLL));
                }
                return Ok(socket);
            }
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> Vec<String> {
        segments(text)
    }

    #[test]
    fn normalize_turns_lists_into_maps_and_drops_empties() {
        assert_eq!(
            normalize(json!(["a", null, "c"])),
            json!({ "0": "a", "2": "c" })
        );
        assert_eq!(
            normalize(json!({ "a": {}, "b": [null], "c": 1 })),
            json!({ "c": 1 })
        );
        assert_eq!(normalize(json!({ "a": { "b": null } })), Json::Null);
    }

    #[test]
    fn set_at_creates_removes_and_prunes() {
        let mut root = Json::Null;
        set_at(&mut root, &path("a/b/c"), json!(1));
        set_at(&mut root, &path("a/d"), json!(true));
        assert_eq!(root, json!({ "a": { "b": { "c": 1 }, "d": true } }));
        set_at(&mut root, &path("a/b/c"), Json::Null);
        assert_eq!(root, json!({ "a": { "d": true } }));
        set_at(&mut root, &path("a/d"), Json::Null);
        assert_eq!(root, Json::Null);
        set_at(&mut root, &path("x/y"), Json::Null);
        assert_eq!(root, Json::Null);
    }

    #[test]
    fn get_at_reads_nested_values() {
        let root = json!({ "a": { "b": 2 } });
        assert_eq!(get_at(&root, &path("a/b")), json!(2));
        assert_eq!(get_at(&root, &path("a/x/y")), Json::Null);
        assert_eq!(get_at(&root, &[]), root);
    }
}
