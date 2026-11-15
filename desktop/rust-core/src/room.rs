//! Sala de um canal: entrar, sair e manter a presença. Segue `LiveRoom` em `src/live/room.ts`.
//!
//! Etapa atual: presença. Quem entra aparece para os outros (site ou app), some sozinho se a
//! conexão cair, volta a se registrar quando ela retorna, e sai se a mesma conta entrar no canal em
//! outro lugar. A sinalização das lives (ofertas, respostas, candidatos) entra aqui na próxima etapa.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use serde_json::json;

use crate::Json;
use crate::rtdb::{Db, DbError, Event, ListenId, Status};

pub const MAX_MEMBERS: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub message: String,
    /// A sessão saiu do canal: perdeu o acesso, ou a conta entrou nele em outro lugar.
    pub fatal: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoomSnapshot {
    pub room: String,
    pub joined: bool,
    pub online: bool,
    pub members: Vec<String>,
    /// Versão do app de cada pessoa no canal, como ela mesma informou ao entrar.
    pub apps: BTreeMap<String, String>,
    pub issue: Option<Issue>,
}

struct State {
    snapshot: RoomSnapshot,
    was_offline: bool,
    /// Outra sessão desta conta entrou no canal: esta sai sem mexer na presença, que agora é da outra.
    replaced: bool,
    listens: Vec<ListenId>,
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
            state: Mutex::new(State {
                snapshot: RoomSnapshot { room: room.to_owned(), online: db.is_connected(), ..RoomSnapshot::default() },
                was_offline: false,
                replaced: false,
                listens: Vec::new(),
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
            let (Some(inner), Status::Connected(online)) = (weak.upgrade(), status) else { return };
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
            // O servidor já tirou a presença durante a queda: registra de novo (fora desta thread,
            // porque a gravação espera a resposta).
            if again {
                let retry = inner.clone();
                std::thread::spawn(move || {
                    if let Err(e) = retry.register_presence() {
                        retry.issue(format!("Não deu para voltar ao canal: {e}"), false);
                    }
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
        inner.state.lock().expect("estado da sala").listens.push(listen);
        Ok(Room { inner })
    }

    pub fn id(&self) -> &str {
        &self.inner.room
    }

    pub fn snapshot(&self) -> RoomSnapshot {
        self.inner.state.lock().expect("estado da sala").snapshot.clone()
    }

    /// Sai do canal. Bloqueia até o servidor confirmar: chame fora da thread que desenha.
    pub fn leave(&self) {
        let (listens, replaced) = {
            let mut state = self.inner.state.lock().expect("estado da sala");
            state.snapshot.joined = false;
            (std::mem::take(&mut state.listens), state.replaced)
        };
        for id in listens {
            self.inner.db.unlisten(id);
        }
        let member = self.inner.member();
        let _ = self.inner.db.on_disconnect_cancel(&member);
        // Trocada por outra sessão da mesma conta: o registro de presença agora é dela.
        if !replaced {
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
        self.db.update(&format!("rooms/{}", self.room), Json::Object(removals))
    }

    /// O pedido de "tire-me se a conexão cair" vai antes da entrada, para uma queda logo depois de
    /// entrar não deixar sessão órfã.
    fn register_presence(&self) -> Result<(), DbError> {
        let member = self.member();
        self.db.on_disconnect_remove(&member)?;
        // A versão do app vai junto: quando uma live não conecta, mostra quem ainda não atualizou.
        self.db.set(&member, json!({ "joinedAt": { ".sv": "timestamp" }, "app": self.app, "sid": self.sid }))
    }

    fn on_members(&self, entries: &Json) {
        let mut state = self.state.lock().expect("estado da sala");
        // A presença desta conta foi escrita por outra sessão (outra aba, o navegador, outro
        // aparelho): a mais nova fica com o canal. Sem registro nenhum é só uma queda de rede.
        let mine = &entries[&self.uid];
        if state.snapshot.joined && !state.replaced && mine.is_object() && mine["sid"] != self.sid.as_str() {
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
            .map(|m| m.iter().map(|(id, entry)| (id.clone(), entry["app"].as_str().unwrap_or("").to_owned())).collect())
            .unwrap_or_default();
    }
}

/// Doze caracteres ao acaso, como o `crypto.randomUUID().slice(0, 12)` do site.
fn session_id() -> String {
    let mut bytes = [0u8; 6];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        bytes.copy_from_slice(&nanos.to_le_bytes()[..6]);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
