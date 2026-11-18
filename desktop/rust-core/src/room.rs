//! Sala de um canal: entrar, sair e manter a presença. Segue `LiveRoom` em `src/live/room.ts`.
//!
//! Presença: quem entra aparece para os outros (site ou app), some sozinho se a conexão cair, volta
//! a se registrar quando ela retorna, e sai se a mesma conta entrar no canal em outro lugar.
//!
//! Sinalização: cada pessoa tem uma caixa de entrada no canal (`signals/{quem recebe}/{quem manda}`).
//! A sala lê a caixa, entrega o que chegou a quem cuida das lives (`take_signals`) e apaga cada
//! mensagem depois de entregue. Por enquanto só as lives nativas (as do motor do app) são atendidas;
//! as mensagens das lives de navegador são descartadas (ver N10 em `docs/rust-app.md`).
//!
//! Live própria: `publish_stream` anuncia no canal a live que o motor está enviando (`native: true`),
//! no formato que o site e o app atual leem; ela some sozinha se a conexão cair e é reanunciada quando
//! a conexão volta.
//!
//! Plateia e prévias, como no site (`setWatching`/`onWatching`/`watchPreviews` de `room.ts`): cada
//! sessão avisa o que tem aberto no palco (`watching/{eu}/{quem transmite}/{tipo}: true`, que some
//! sozinho se a conexão cair), e quem transmite grava de tempos em tempos uma foto pequena da live
//! (`previews/{eu}/{tipo}`), lida só enquanto a interface vai mostrá-la.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, Weak};

use serde_json::json;

use crate::Json;
use crate::rtdb::{Db, DbError, Event, ListenId, Status, push_id};

pub const MAX_MEMBERS: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub message: String,
    /// A sessão saiu do canal: perdeu o acesso, ou a conta entrou nele em outro lugar.
    pub fatal: bool,
}

/// Uma live anunciada no canal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Broadcast {
    pub stream_id: String,
    /// Enviada pelo motor do app (uma conexão só de recepção, direto com ele), e não pelo navegador.
    pub native: bool,
    pub sound: bool,
    /// Início da live na hora do servidor, em ms (0 se não veio).
    pub started_at: i64,
    /// Nome do jogo transmitido (vazio quando a fonte não é um jogo).
    pub game: String,
}

/// Tamanho máximo da prévia gravada (as regras do banco conferem o mesmo número).
pub const PREVIEW_LIMIT: usize = 14_000;

/// Só imagens pequenas em data URL viram prévia: o que vem do banco não é confiável por si só.
pub fn valid_preview(value: &str) -> bool {
    value.len() <= PREVIEW_LIMIT
        && (value.starts_with("data:image/webp;base64,")
            || value.starts_with("data:image/jpeg;base64,"))
}

/// Prévias de uma pessoa num canal (uma leitura só, quando o mouse passa em cima dela na barra
/// lateral). Bloqueia até a resposta: chame fora da thread que desenha.
pub fn load_previews(db: &Db, room: &str, uid: &str) -> BTreeMap<String, String> {
    let value = db
        .get(&format!("rooms/{room}/previews/{uid}"))
        .unwrap_or(Json::Null);
    ["screen", "camera"]
        .into_iter()
        .filter_map(|kind| {
            Some((
                kind.to_owned(),
                value[kind]
                    .as_str()
                    .filter(|data| valid_preview(data))?
                    .to_owned(),
            ))
        })
        .collect()
}

/// Lives por pessoa e por tipo ("screen" ou "camera").
pub type Broadcasts = BTreeMap<String, BTreeMap<String, Broadcast>>;

/// Mensagem que chegou na caixa de entrada.
#[derive(Clone, Debug, PartialEq)]
pub enum Signal {
    /// Sinalização de uma live nativa: a resposta do motor de quem transmite à nossa oferta, ou a
    /// oferta de quem quer assistir à nossa.
    Native {
        from: String,
        kind: String,
        answer: bool,
        sdp: String,
        nonce: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoomSnapshot {
    pub room: String,
    pub joined: bool,
    pub online: bool,
    pub members: Vec<String>,
    /// Versão do app de cada pessoa no canal, como ela mesma informou ao entrar.
    pub apps: BTreeMap<String, String>,
    pub broadcasts: Broadcasts,
    /// Quem está assistindo cada live ("uid:tipo" → pessoas), como cada uma informou.
    pub watchers: BTreeMap<String, Vec<String>>,
    /// Prévia de cada live ("uid:tipo" → data URL). Só vem enquanto a interface pede (`watch_previews`).
    pub previews: BTreeMap<String, String>,
    /// Quantas vezes a presença foi refeita depois de uma queda da conexão. Quem cuida das lives
    /// refaz as conexões quando este número muda: as ofertas feitas durante a queda se perderam.
    pub reconnects: u64,
    pub issue: Option<Issue>,
}

/// Live desta conta anunciada no canal (para reanunciar quando a conexão volta).
#[derive(Clone)]
struct Local {
    started_at: i64,
    stream_id: String,
    sound: bool,
    game: String,
}

struct State {
    snapshot: RoomSnapshot,
    /// Lives próprias no ar, por tipo ("screen" ou "camera").
    locals: BTreeMap<String, Local>,
    was_offline: bool,
    /// Outra sessão desta conta entrou no canal: esta sai sem mexer na presença, que agora é da outra.
    replaced: bool,
    listens: Vec<ListenId>,
    members_loaded: bool,
    /// O que chegou antes da lista de membros fica guardado até ela chegar.
    pending_inbox: Option<Json>,
    /// Mensagens já tratadas e ainda não apagadas ("quem mandou/id").
    seen: BTreeSet<String>,
    signals: Vec<Signal>,
    /// O que esta sessão está assistindo ("uid:tipo", em ordem), como foi avisado ao canal.
    watching: Vec<String>,
    /// Escuta das prévias, ligada só enquanto a interface pede.
    previews: Option<ListenId>,
}

struct Inner {
    db: Db,
    uid: String,
    room: String,
    app: String,
    /// Identifica esta sessão na presença do canal (a mesma conta em dois lugares disputaria a
    /// sinalização: a mais nova fica, a antiga percebe e sai).
    sid: String,
    state: Mutex<State>,
    /// Uma gravação da plateia por vez, sempre com o valor mais recente.
    watching_lock: Mutex<()>,
    notify: Box<dyn Fn() + Send + Sync>,
}

pub struct Room {
    inner: Arc<Inner>,
}

impl Room {
    /// Entra no canal. Bloqueia até o servidor confirmar: chame fora da thread que desenha.
    /// `app` é como este cliente se apresenta aos outros ("0.1.0 (nativo)").
    pub fn join(
        db: &Db,
        uid: &str,
        room: &str,
        app: &str,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Result<Room, DbError> {
        let inner = Arc::new(Inner {
            db: db.clone(),
            uid: uid.to_owned(),
            room: room.to_owned(),
            app: app.to_owned(),
            sid: session_id(),
            watching_lock: Mutex::new(()),
            state: Mutex::new(State {
                snapshot: RoomSnapshot {
                    room: room.to_owned(),
                    online: db.is_connected(),
                    ..RoomSnapshot::default()
                },
                locals: BTreeMap::new(),
                was_offline: false,
                replaced: false,
                listens: Vec::new(),
                members_loaded: false,
                pending_inbox: None,
                seen: BTreeSet::new(),
                signals: Vec::new(),
                watching: Vec::new(),
                previews: None,
            }),
            notify: Box::new(notify),
        });
        let members = db.get(&inner.path("members"))?;
        let count = members.as_object().map_or(0, serde_json::Map::len);
        if count >= MAX_MEMBERS && members.get(uid).is_none() {
            return Err(DbError::Server("Canal cheio".into()));
        }
        inner.clear_inbox()?;
        inner.register_presence()?;
        inner.state.lock().expect("estado da sala").snapshot.joined = true;

        let weak = Arc::downgrade(&inner);
        db.on_status(move |status| {
            let (Some(inner), Status::Connected(online)) = (weak.upgrade(), status) else {
                return;
            };
            let again = {
                let mut state = inner.state.lock().expect("estado da sala");
                state.snapshot.online = *online;
                if !online {
                    state.was_offline = true;
                }
                let again = *online && state.was_offline && state.snapshot.joined;
                if again {
                    state.was_offline = false;
                }
                again
            };
            // O servidor já tirou a presença (e as lives) durante a queda: registra de novo (fora
            // desta thread, porque a gravação espera a resposta) e reanuncia o que estava no ar.
            if again {
                let retry = inner.clone();
                std::thread::spawn(move || {
                    let done = retry
                        .register_presence()
                        .and_then(|()| retry.republish())
                        .and_then(|()| retry.publish_watching());
                    if let Err(e) = done {
                        retry.issue(format!("Não deu para voltar ao canal: {e}"), false);
                    }
                    retry
                        .state
                        .lock()
                        .expect("estado da sala")
                        .snapshot
                        .reconnects += 1;
                    (retry.notify)();
                });
            }
            (inner.notify)();
        });
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        let listen = db.listen(&inner.path("members"), move |event| {
            let Some(inner) = weak.upgrade() else { return };
            match event {
                Event::Value(entries) => inner.on_members(entries),
                Event::Denied(_) => inner.issue("Você perdeu o acesso a este canal".into(), true),
            }
            (inner.notify)();
        });
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        let streams = db.listen(&inner.path("streams"), move |event| {
            let Some(inner) = weak.upgrade() else { return };
            let value = match event {
                Event::Value(value) => broadcasts(value),
                Event::Denied(_) => Broadcasts::new(),
            };
            inner
                .state
                .lock()
                .expect("estado da sala")
                .snapshot
                .broadcasts = value;
            (inner.notify)();
        });
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        let watching = db.listen(&inner.path("watching"), move |event| {
            let Some(inner) = weak.upgrade() else { return };
            let value = match event {
                Event::Value(value) => watchers(value),
                Event::Denied(_) => BTreeMap::new(),
            };
            inner
                .state
                .lock()
                .expect("estado da sala")
                .snapshot
                .watchers = value;
            (inner.notify)();
        });
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        let inbox = db.listen(&inner.path(&format!("signals/{uid}")), move |event| {
            let (Some(inner), Event::Value(data)) = (weak.upgrade(), event) else {
                return;
            };
            inner.on_inbox(data);
            (inner.notify)();
        });
        inner
            .state
            .lock()
            .expect("estado da sala")
            .listens
            .extend([listen, streams, watching, inbox]);
        Ok(Room { inner })
    }

    pub fn id(&self) -> &str {
        &self.inner.room
    }

    pub fn snapshot(&self) -> RoomSnapshot {
        self.inner
            .state
            .lock()
            .expect("estado da sala")
            .snapshot
            .clone()
    }

    /// O que chegou na caixa de entrada desde a última chamada.
    pub fn take_signals(&self) -> Vec<Signal> {
        std::mem::take(&mut self.inner.state.lock().expect("estado da sala").signals)
    }

    /// Manda a quem transmite a oferta para assistir à live nativa dele. `nonce` liga a resposta a
    /// esta oferta. Não espera: `done` recebe o resultado, na thread da conexão.
    pub fn send_native_offer(
        &self,
        to: &str,
        kind: &str,
        sdp: &str,
        nonce: &str,
        done: impl FnOnce(Result<(), DbError>) + Send + 'static,
    ) {
        let message = json!({ "type": "offer", "sdp": { "type": "offer", "sdp": sdp }, "native": kind, "nonce": nonce });
        let path = self.inner.path(&format!(
            "signals/{to}/{}/{}",
            self.inner.uid,
            push_id(self.inner.db.server_now_ms())
        ));
        self.inner.db.set_then(&path, message, done);
    }

    /// Devolve a quem quer assistir a resposta do motor à oferta dele, com o mesmo `nonce` (vazio
    /// quando a oferta veio sem). Não espera: `done` recebe o resultado, na thread da conexão.
    pub fn send_native_answer(
        &self,
        to: &str,
        kind: &str,
        sdp: &str,
        nonce: &str,
        done: impl FnOnce(Result<(), DbError>) + Send + 'static,
    ) {
        let mut message =
            json!({ "type": "answer", "sdp": { "type": "answer", "sdp": sdp }, "native": kind });
        if !nonce.is_empty() {
            message["nonce"] = json!(nonce);
        }
        let path = self.inner.path(&format!(
            "signals/{to}/{}/{}",
            self.inner.uid,
            push_id(self.inner.db.server_now_ms())
        ));
        self.inner.db.set_then(&path, message, done);
    }

    /// Anuncia no canal uma live desta conta enviada pelo motor do app (`native: true`): quem
    /// assiste vê o anúncio e manda a oferta. `stream_id` identifica esta live (muda a cada começo);
    /// `sound` diz se há som para pedir; `game` é o nome do jogo, quando a fonte é um jogo (até 48
    /// caracteres, como no site). O início vai na hora do servidor. Bloqueia até o servidor confirmar.
    pub fn publish_stream(
        &self,
        kind: &str,
        stream_id: &str,
        sound: bool,
        game: &str,
    ) -> Result<(), DbError> {
        let game: String = game.trim().chars().take(48).collect();
        let local = Local {
            started_at: self.inner.db.server_now_ms(),
            stream_id: stream_id.to_owned(),
            sound,
            game,
        };
        self.inner
            .state
            .lock()
            .expect("estado da sala")
            .locals
            .insert(kind.to_owned(), local.clone());
        let done = self.inner.publish(kind, &local);
        if done.is_err() {
            self.inner
                .state
                .lock()
                .expect("estado da sala")
                .locals
                .remove(kind);
        }
        done
    }

    /// Tira o anúncio de uma live desta conta (e a prévia dela). Bloqueia até o servidor confirmar.
    pub fn unpublish_stream(&self, kind: &str) {
        self.inner
            .state
            .lock()
            .expect("estado da sala")
            .locals
            .remove(kind);
        let _ = self.inner.db.remove(
            &self
                .inner
                .path(&format!("streams/{}/{kind}", self.inner.uid)),
        );
        let _ = self.inner.db.remove(
            &self
                .inner
                .path(&format!("previews/{}/{kind}", self.inner.uid)),
        );
    }

    /// Grava a prévia (foto pequena, data URL) de uma live própria que está no ar. Ela some sozinha
    /// se a conexão cair. Bloqueia até o servidor confirmar.
    pub fn publish_preview(&self, kind: &str, data: &str) -> Result<(), DbError> {
        if !valid_preview(data) {
            return Err(DbError::Server("prévia inválida".into()));
        }
        if !self
            .inner
            .state
            .lock()
            .expect("estado da sala")
            .locals
            .contains_key(kind)
        {
            return Ok(());
        }
        let previews = self.inner.path(&format!("previews/{}", self.inner.uid));
        self.inner.db.on_disconnect_remove(&previews)?;
        self.inner
            .db
            .set(&format!("{previews}/{kind}"), json!(data))
    }

    /// Avisa ao canal quais lives esta sessão está assistindo ("uid:tipo"). Só grava quando muda; a
    /// gravação sai numa thread (não bloqueia quem desenha) e some sozinha se a conexão cair.
    pub fn set_watching(&self, keys: &[String]) {
        let mut next: Vec<String> = keys
            .iter()
            .filter(|key| {
                key.rsplit_once(':').is_some_and(|(uid, kind)| {
                    uid != self.inner.uid && (kind == "screen" || kind == "camera")
                })
            })
            .cloned()
            .collect();
        next.sort();
        next.dedup();
        {
            let mut state = self.inner.state.lock().expect("estado da sala");
            if state.watching == next {
                return;
            }
            state.watching = next;
            if !state.snapshot.joined || state.replaced {
                return;
            }
        }
        let inner = self.inner.clone();
        std::thread::spawn(move || drop(inner.publish_watching()));
    }

    /// Liga ou desliga a leitura das prévias do canal (cartões "Ao vivo agora"): ninguém baixa foto
    /// que não vai ver.
    pub fn watch_previews(&self, on: bool) {
        let mut state = self.inner.state.lock().expect("estado da sala");
        if on == state.previews.is_some() || (on && !state.snapshot.joined) {
            return;
        }
        if let Some(id) = state.previews.take() {
            self.inner.db.unlisten(id);
            state.snapshot.previews.clear();
            drop(state);
            (self.inner.notify)();
            return;
        }
        let weak: Weak<Inner> = Arc::downgrade(&self.inner);
        state.previews = Some(
            self.inner
                .db
                .listen(&self.inner.path("previews"), move |event| {
                    let Some(inner) = weak.upgrade() else { return };
                    let value = match event {
                        Event::Value(value) => previews(value),
                        Event::Denied(_) => BTreeMap::new(),
                    };
                    let mut state = inner.state.lock().expect("estado da sala");
                    if state.previews.is_some() {
                        state.snapshot.previews = value;
                    }
                    drop(state);
                    (inner.notify)();
                }),
        );
    }

    /// Sai do canal. Bloqueia até o servidor confirmar: chame fora da thread que desenha.
    pub fn leave(&self) {
        let (listens, replaced, locals) = {
            let mut state = self.inner.state.lock().expect("estado da sala");
            state.snapshot.joined = false;
            let mut listens = std::mem::take(&mut state.listens);
            listens.extend(state.previews.take());
            (listens, state.replaced, std::mem::take(&mut state.locals))
        };
        for id in listens {
            self.inner.db.unlisten(id);
        }
        let streams = self.inner.path(&format!("streams/{}", self.inner.uid));
        let previews = self.inner.path(&format!("previews/{}", self.inner.uid));
        for kind in locals.keys() {
            let _ = self.inner.db.remove(&format!("{streams}/{kind}"));
            let _ = self.inner.db.remove(&format!("{previews}/{kind}"));
        }
        let _ = self.inner.db.on_disconnect_cancel(&streams);
        let _ = self.inner.db.on_disconnect_cancel(&previews);
        let member = self.inner.member();
        let watching = self.inner.watching();
        let _ = self.inner.db.on_disconnect_cancel(&watching);
        let _ = self.inner.db.on_disconnect_cancel(&member);
        // Trocada por outra sessão da mesma conta: o registro de presença agora é dela.
        if !replaced {
            let _ = self.inner.db.remove(&watching);
            let _ = self.inner.db.remove(&member);
        }
        (self.inner.notify)();
    }
}

impl Inner {
    fn path(&self, suffix: &str) -> String {
        format!("rooms/{}/{suffix}", self.room)
    }

    fn member(&self) -> String {
        self.path(&format!("members/{}", self.uid))
    }

    fn watching(&self) -> String {
        self.path(&format!("watching/{}", self.uid))
    }

    /// Grava a plateia desta sessão com o valor mais recente (uma gravação por vez).
    fn publish_watching(&self) -> Result<(), DbError> {
        let _one = self.watching_lock.lock().expect("plateia");
        let (keys, joined) = {
            let state = self.state.lock().expect("estado da sala");
            (
                state.watching.clone(),
                state.snapshot.joined && !state.replaced,
            )
        };
        if !joined {
            return Ok(());
        }
        let mut value = serde_json::Map::new();
        for key in &keys {
            if let Some((uid, kind)) = key.rsplit_once(':') {
                let entry = value.entry(uid.to_owned()).or_insert_with(|| json!({}));
                entry[kind] = json!(true);
            }
        }
        let path = self.watching();
        self.db.on_disconnect_remove(&path)?;
        self.db.set(
            &path,
            if value.is_empty() {
                Json::Null
            } else {
                Json::Object(value)
            },
        )
    }

    fn issue(&self, message: String, fatal: bool) {
        let mut state = self.state.lock().expect("estado da sala");
        state.snapshot.issue = Some(Issue { message, fatal });
        drop(state);
        (self.notify)();
    }

    /// Mensagens antigas de outra sessão quebrariam a negociação nova.
    fn clear_inbox(&self) -> Result<(), DbError> {
        let inbox = match self.db.get(&self.path(&format!("signals/{}", self.uid))) {
            Ok(inbox) => inbox,
            Err(DbError::Denied) => return Err(DbError::Denied),
            Err(_) => Json::Null,
        };
        let mut removals = serde_json::Map::new();
        for (sender, messages) in inbox.as_object().into_iter().flatten() {
            for id in messages.as_object().into_iter().flat_map(|map| map.keys()) {
                removals.insert(format!("signals/{}/{sender}/{id}", self.uid), Json::Null);
            }
        }
        if removals.is_empty() {
            return Ok(());
        }
        self.db
            .update(&format!("rooms/{}", self.room), Json::Object(removals))
    }

    /// O pedido de "tire-me se a conexão cair" vai antes da entrada, para uma queda logo depois de
    /// entrar não deixar sessão órfã.
    fn register_presence(&self) -> Result<(), DbError> {
        let member = self.member();
        self.db.on_disconnect_remove(&member)?;
        // A versão do app vai junto: quando uma live não conecta, mostra quem ainda não atualizou.
        self.db.set(
            &member,
            json!({ "joinedAt": { ".sv": "timestamp" }, "app": self.app, "sid": self.sid }),
        )
    }

    /// O pedido de "tire a live se a conexão cair" vai antes do anúncio, como na presença.
    fn publish(&self, kind: &str, local: &Local) -> Result<(), DbError> {
        let streams = self.path(&format!("streams/{}", self.uid));
        self.db.on_disconnect_remove(&streams)?;
        let mut live =
            json!({ "startedAt": local.started_at, "streamId": local.stream_id, "native": true });
        if local.sound {
            live["sound"] = json!(true);
        }
        if !local.game.is_empty() {
            live["game"] = json!(local.game);
        }
        self.db.set(&format!("{streams}/{kind}"), live)
    }

    /// Depois de uma queda: anuncia de novo as lives que estavam no ar.
    fn republish(&self) -> Result<(), DbError> {
        let locals = self.state.lock().expect("estado da sala").locals.clone();
        locals
            .iter()
            .try_for_each(|(kind, local)| self.publish(kind, local))
    }

    fn on_members(self: &Arc<Self>, entries: &Json) {
        let mut state = self.state.lock().expect("estado da sala");
        // A presença desta conta foi escrita por outra sessão (outra aba, o navegador, outro
        // aparelho): a mais nova fica com o canal. Sem registro nenhum é só uma queda de rede.
        let mine = &entries[&self.uid];
        if state.snapshot.joined
            && !state.replaced
            && mine.is_object()
            && mine["sid"] != self.sid.as_str()
        {
            state.replaced = true;
            state.snapshot.issue = Some(Issue {
                message: "Você entrou neste canal em outro lugar (outra aba, navegador ou aparelho). Esta sessão saiu do canal."
                    .into(),
                fatal: true,
            });
            return;
        }
        let map = entries.as_object();
        state.snapshot.members = map.map(|m| m.keys().cloned().collect()).unwrap_or_default();
        state.snapshot.apps = map
            .map(|m| {
                m.iter()
                    .map(|(id, entry)| (id.clone(), entry["app"].as_str().unwrap_or("").to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        // Sinais que chegaram antes da lista de membros ficam guardados até aqui.
        let first = !state.members_loaded;
        state.members_loaded = true;
        let pending = state.pending_inbox.take().filter(|_| first);
        drop(state);
        if let Some(data) = pending {
            self.on_inbox(&data);
        }
    }

    fn on_inbox(self: &Arc<Self>, data: &Json) {
        let mut state = self.state.lock().expect("estado da sala");
        if !state.members_loaded {
            state.pending_inbox = Some(data.clone());
            return;
        }
        for (sender, messages) in data.as_object().into_iter().flatten() {
            for (id, message) in messages.as_object().into_iter().flatten() {
                let key = format!("{sender}/{id}");
                if !state.seen.insert(key.clone()) {
                    continue;
                }
                // Sobras de quem já saiu do canal não devem recriar uma conexão; as mensagens das
                // lives de navegador ainda não são atendidas aqui. Tudo é apagado depois de visto.
                if state.snapshot.members.contains(sender)
                    && let Some(kind) = message["native"].as_str()
                    && let Some(sdp) = message["sdp"]["sdp"].as_str()
                {
                    state.signals.push(Signal::Native {
                        from: sender.clone(),
                        kind: kind.to_owned(),
                        answer: message["type"] == "answer",
                        sdp: sdp.to_owned(),
                        nonce: message["nonce"].as_str().unwrap_or("").to_owned(),
                    });
                }
                let weak = Arc::downgrade(self);
                self.db.set_then(
                    &self.path(&format!("signals/{}/{sender}/{id}", self.uid)),
                    Json::Null,
                    move |result| {
                        if let (Ok(()), Some(inner)) = (result, weak.upgrade()) {
                            inner
                                .state
                                .lock()
                                .expect("estado da sala")
                                .seen
                                .remove(&key);
                        }
                    },
                );
            }
        }
    }
}

fn broadcasts(value: &Json) -> Broadcasts {
    let mut all = Broadcasts::new();
    for (uid, kinds) in value.as_object().into_iter().flatten() {
        for kind in ["screen", "camera"] {
            let live = &kinds[kind];
            let Some(stream_id) = live["streamId"].as_str().filter(|id| !id.is_empty()) else {
                continue;
            };
            all.entry(uid.clone()).or_default().insert(
                kind.to_owned(),
                Broadcast {
                    stream_id: stream_id.to_owned(),
                    native: live["native"].as_bool() == Some(true),
                    sound: live["sound"].as_bool() == Some(true),
                    started_at: live["startedAt"].as_f64().map_or(0, |at| at.round() as i64),
                    game: live["game"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(48)
                        .collect(),
                },
            );
        }
    }
    all
}

/// `watching/{quem assiste}/{quem transmite}/{tipo}` → quem assiste cada live ("uid:tipo").
fn watchers(value: &Json) -> BTreeMap<String, Vec<String>> {
    let mut all: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (viewer, lives) in value.as_object().into_iter().flatten() {
        for (uid, kinds) in lives.as_object().into_iter().flatten() {
            for kind in ["screen", "camera"] {
                if kinds[kind].as_bool() == Some(true) {
                    all.entry(format!("{uid}:{kind}"))
                        .or_default()
                        .push(viewer.clone());
                }
            }
        }
    }
    all
}

/// `previews/{uid}/{tipo}` → prévia válida de cada live ("uid:tipo").
fn previews(value: &Json) -> BTreeMap<String, String> {
    let mut all = BTreeMap::new();
    for (uid, kinds) in value.as_object().into_iter().flatten() {
        for kind in ["screen", "camera"] {
            if let Some(data) = kinds[kind].as_str().filter(|data| valid_preview(data)) {
                all.insert(format!("{uid}:{kind}"), data.to_owned());
            }
        }
    }
    all
}

/// Doze caracteres ao acaso, como o `crypto.randomUUID().slice(0, 12)` do site.
fn session_id() -> String {
    let mut bytes = [0u8; 6];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        bytes.copy_from_slice(&nanos.to_le_bytes()[..6]);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
