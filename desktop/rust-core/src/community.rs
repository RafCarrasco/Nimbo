//! Estado da comunidade para a interface: categorias, canais, níveis, cargos, perfis e quem está
//! em cada canal. Espelha `src/hooks/useCommunity.ts` e `src/community.ts` do site, com as mesmas
//! leituras: o proprietário lê as listas inteiras; os demais leem o que é deles, os grupos de que
//! fazem parte e, uma a uma, as pessoas que encontram nos canais que enxergam.
//!
//! Quem garante o acesso são as regras do banco. As contas daqui (`channel_allowed` etc.) só decidem
//! o que a interface mostra.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, Weak};

use crate::Json;
use crate::rtdb::{Db, Event, ListenId};

pub const DEFAULT_CATEGORY: &str = "lives";
/// Canais que existem sempre, sem registro no banco: (id, nome, descrição).
pub const SYSTEM_CHANNELS: [(&str, &str, &str); 2] = [
    ("geral", "Geral", "Jogos, lives e conversa"),
    (
        "competitivo",
        "Competitivo",
        "Partidas e comunicação de equipe",
    ),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    #[default]
    Member,
    Admin,
    Owner,
}

impl Level {
    fn parse(value: &Json) -> Option<Level> {
        match value.as_str()? {
            "owner" => Some(Level::Owner),
            "admin" => Some(Level::Admin),
            "member" => Some(Level::Member),
            _ => None,
        }
    }

    pub fn is_staff(self) -> bool {
        self != Level::Member
    }

    pub fn label(self) -> &'static str {
        match self {
            Level::Owner => "Proprietário",
            Level::Admin => "Administrador",
            Level::Member => "Membro",
        }
    }

    /// Uma restrição de nível `self` deixa passar quem tem o nível `mine`?
    fn admits(self, mine: Level) -> bool {
        self == Level::Member
            || mine == Level::Owner
            || (self == Level::Admin && mine == Level::Admin)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Category {
    pub name: String,
    pub order: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub name: String,
    pub description: String,
    pub category_id: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoleDef {
    pub name: String,
    pub color: String,
    pub created_at: f64,
}

/// Lives que uma pessoa tem no ar num canal.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Live {
    pub screen: bool,
    pub camera: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoomPresence {
    pub members: Vec<String>,
    pub streams: BTreeMap<String, Live>,
}

/// Retrato do que a conta enxerga agora. É o que a interface desenha.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub loaded: bool,
    pub uid: String,
    pub level: Level,
    pub owner: bool,
    pub categories: BTreeMap<String, Category>,
    pub channels: BTreeMap<String, Channel>,
    pub category_access: BTreeMap<String, Level>,
    pub channel_access: BTreeMap<String, Level>,
    pub category_roles: BTreeMap<String, Vec<String>>,
    pub channel_roles: BTreeMap<String, Vec<String>>,
    /// Nome de exibição por pessoa.
    pub profiles: BTreeMap<String, String>,
    pub avatars: BTreeMap<String, String>,
    pub roles: BTreeMap<String, Level>,
    pub role_defs: BTreeMap<String, RoleDef>,
    pub member_roles: BTreeMap<String, BTreeSet<String>>,
    pub net_admins: BTreeSet<String>,
    /// Pessoas cujo perfil ainda está sendo buscado.
    pub resolving: BTreeSet<String>,
    pub presence: BTreeMap<String, RoomPresence>,
}

impl Snapshot {
    pub fn level_of(&self, uid: &str) -> Level {
        self.roles.get(uid).copied().unwrap_or_default()
    }

    /// Só o proprietário passa por cima dos cargos; administrador segue os grupos como todo mundo.
    fn roles_ok(&self, required: Option<&Vec<String>>, uid: &str) -> bool {
        let Some(required) = required.filter(|list| !list.is_empty()) else {
            return true;
        };
        if self.level_of(uid) == Level::Owner {
            return true;
        }
        let mine = self.member_roles.get(uid);
        required
            .iter()
            .any(|role| mine.is_some_and(|set| set.contains(role)))
    }

    pub fn category_allowed(&self, category_id: &str, uid: &str) -> bool {
        self.category_access
            .get(category_id)
            .copied()
            .unwrap_or_default()
            .admits(self.level_of(uid))
            && self.roles_ok(self.category_roles.get(category_id), uid)
    }

    pub fn channel_allowed(&self, channel_id: &str, uid: &str) -> bool {
        let Some(channel) = self.channels.get(channel_id).filter(|c| c.enabled) else {
            return false;
        };
        self.channel_access
            .get(channel_id)
            .copied()
            .unwrap_or_default()
            .admits(self.level_of(uid))
            && self.roles_ok(self.channel_roles.get(channel_id), uid)
            && self.category_allowed(&channel.category_id, uid)
    }

    /// Texto do cadeado. O proprietário vê todos os cargos; os demais só conhecem o nome dos que têm.
    pub fn restricted_label(&self, level: Option<Level>, role_ids: Option<&Vec<String>>) -> String {
        let ids = role_ids.map(Vec::as_slice).unwrap_or_default();
        let names: Vec<&str> = ids
            .iter()
            .filter_map(|id| match self.role_defs.get(id) {
                Some(def) => Some(def.name.as_str()),
                None => self.owner.then_some("removido"),
            })
            .collect();
        let by_level = match level {
            Some(Level::Owner) => "Somente proprietário",
            Some(Level::Admin) => "Somente administradores",
            _ => "",
        };
        let by_role = if !names.is_empty() {
            format!("Cargos: {}", names.join(", "))
        } else if !ids.is_empty() {
            "Restrito por cargo".to_owned()
        } else {
            String::new()
        };
        [by_level.to_owned(), by_role]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Cor do primeiro cargo conhecido da pessoa, como no site.
    pub fn role_color(&self, uid: &str) -> Option<&str> {
        let mine = self.member_roles.get(uid)?;
        mine.iter()
            .find_map(|role| self.role_defs.get(role))
            .map(|def| def.color.as_str())
    }

    /// Canais na ordem do site: os padrão primeiro, depois os criados (o id cresce com a data de criação).
    pub fn ordered_channels(&self) -> Vec<(&String, &Channel)> {
        let system = |id: &str| SYSTEM_CHANNELS.iter().position(|(known, ..)| *known == id);
        let mut list: Vec<_> = self.channels.iter().collect();
        list.sort_by_key(|(id, _)| (system(id).unwrap_or(usize::MAX), (*id).clone()));
        list
    }

    /// Categorias na ordem da barra lateral.
    pub fn ordered_categories(&self) -> Vec<(&String, &Category)> {
        let mut list: Vec<_> = self.categories.iter().collect();
        list.sort_by(|a, b| a.1.order.total_cmp(&b.1.order));
        list
    }
}

fn text(value: &Json, key: &str) -> String {
    value[key].as_str().unwrap_or("").to_owned()
}

fn keys(value: &Json) -> BTreeSet<String> {
    value
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

fn levels(value: &Json) -> BTreeMap<String, Level> {
    value
        .as_object()
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| Some((k.clone(), Level::parse(v)?)))
                .collect()
        })
        .unwrap_or_default()
}

fn texts(value: &Json) -> BTreeMap<String, String> {
    value
        .as_object()
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

fn display_name(value: &Json) -> Option<String> {
    value["displayName"]
        .as_str()
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

fn role_def(value: &Json) -> Option<RoleDef> {
    Some(RoleDef {
        name: value["name"].as_str()?.to_owned(),
        color: text(value, "color"),
        created_at: value["createdAt"].as_f64().unwrap_or(0.0),
    })
}

/// Listas de cargos por categoria ou canal: as posições "0".."4" viram uma lista na ordem.
fn role_lists(value: &Json) -> BTreeMap<String, Vec<String>> {
    let list = |slots: &Json| -> Vec<String> {
        let mut pairs: Vec<(u32, String)> = slots
            .as_object()
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| {
                        Some((
                            k.parse().ok()?,
                            v.as_str().filter(|s| !s.is_empty())?.to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        pairs.sort();
        pairs.into_iter().map(|(_, id)| id).collect()
    };
    value
        .as_object()
        .map(|map| map.iter().map(|(k, v)| (k.clone(), list(v))).collect())
        .unwrap_or_default()
}

fn sets(value: &Json) -> BTreeMap<String, BTreeSet<String>> {
    value
        .as_object()
        .map(|map| map.iter().map(|(k, v)| (k.clone(), keys(v))).collect())
        .unwrap_or_default()
}

fn streams(value: &Json) -> BTreeMap<String, Live> {
    let on = |live: &Json| live["streamId"].as_str().is_some_and(|id| !id.is_empty());
    value
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(uid, kinds)| {
                    (
                        uid.clone(),
                        Live {
                            screen: on(&kinds["screen"]),
                            camera: on(&kinds["camera"]),
                        },
                    )
                })
                .filter(|(_, live)| live.screen || live.camera)
                .collect()
        })
        .unwrap_or_default()
}

/// O que chegou do banco, ainda cru, e o que é derivado disso.
#[derive(Default)]
struct Raw {
    level: Option<Level>,
    channel_access: BTreeMap<String, Level>,
    category_access: BTreeMap<String, Level>,
    channel_roles: BTreeMap<String, Vec<String>>,
    category_roles: BTreeMap<String, Vec<String>>,
    /// `None` até a primeira resposta (lista inteira para o proprietário, o próprio para os demais).
    profiles: Option<BTreeMap<String, String>>,
    avatars: BTreeMap<String, String>,
    net_admins: BTreeSet<String>,
    all_roles: BTreeMap<String, Level>,
    all_defs: BTreeMap<String, RoleDef>,
    all_member_roles: BTreeMap<String, BTreeSet<String>>,
    own_roles: BTreeSet<String>,
    group_defs: BTreeMap<String, RoleDef>,
    group_members: BTreeMap<String, BTreeSet<String>>,
    category_ids: BTreeSet<String>,
    channel_ids: BTreeSet<String>,
    categories: BTreeMap<String, Category>,
    channels: BTreeMap<String, Channel>,
    /// Perfil de quem aparece nos canais: `Some(None)` é "perguntei e não tem".
    people: BTreeMap<String, Option<String>>,
    people_avatars: BTreeMap<String, String>,
    rooms: BTreeMap<String, RoomPresence>,
}

#[derive(Default)]
struct Subs {
    base: Vec<ListenId>,
    /// `Some(true)` com as leituras de proprietário no ar, `Some(false)` com as de quem não é.
    mode: Option<bool>,
    mode_subs: Vec<ListenId>,
    groups: HashMap<String, Vec<ListenId>>,
    index: Vec<ListenId>,
    categories: HashMap<String, ListenId>,
    channels: HashMap<String, ListenId>,
    people: HashMap<String, Vec<ListenId>>,
    rooms: HashMap<String, Vec<ListenId>>,
    permissions: String,
}

struct State {
    raw: Raw,
    subs: Subs,
    snapshot: Snapshot,
}

struct Inner {
    db: Db,
    uid: String,
    state: Mutex<State>,
    notify: Box<dyn Fn() + Send + Sync>,
}

/// Acompanha a comunidade para uma conta. `notify` é chamado (na thread da conexão) a cada mudança;
/// a interface pega o retrato novo com `snapshot()`.
pub struct Community {
    inner: Arc<Inner>,
}

impl Community {
    pub fn start(db: &Db, uid: &str, notify: impl Fn() + Send + Sync + 'static) -> Community {
        let inner = Arc::new(Inner {
            db: db.clone(),
            uid: uid.to_owned(),
            state: Mutex::new(State {
                raw: Raw::default(),
                subs: Subs::default(),
                snapshot: Snapshot {
                    uid: uid.to_owned(),
                    ..Snapshot::default()
                },
            }),
            notify: Box::new(notify),
        });
        let base = vec![
            inner.hook(format!("community/roles/{uid}"), |raw, v| {
                raw.level = Some(v.and_then(Level::parse).unwrap_or_default());
            }),
            inner.hook("community/channelAccess".into(), |raw, v| {
                raw.channel_access = v.map(levels).unwrap_or_default()
            }),
            inner.hook("community/categoryAccess".into(), |raw, v| {
                raw.category_access = v.map(levels).unwrap_or_default()
            }),
            inner.hook("community/categoryRoles".into(), |raw, v| {
                raw.category_roles = v.map(role_lists).unwrap_or_default()
            }),
            inner.hook("community/channelRoles".into(), |raw, v| {
                raw.channel_roles = v.map(role_lists).unwrap_or_default()
            }),
        ];
        inner.state.lock().expect("estado da comunidade").subs.base = base;
        Community { inner }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.inner
            .state
            .lock()
            .expect("estado da comunidade")
            .snapshot
            .clone()
    }
}

impl Drop for Community {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().expect("estado da comunidade");
        let subs = std::mem::take(&mut state.subs);
        let ids = subs
            .base
            .into_iter()
            .chain(subs.mode_subs)
            .chain(subs.index)
            .chain(subs.groups.into_values().flatten())
            .chain(subs.categories.into_values())
            .chain(subs.channels.into_values())
            .chain(subs.people.into_values().flatten())
            .chain(subs.rooms.into_values().flatten());
        for id in ids {
            self.inner.db.unlisten(id);
        }
    }
}

impl Inner {
    /// Acompanha um caminho; `apply` recebe o valor (ou `None` quando a leitura foi recusada).
    fn hook(
        self: &Arc<Self>,
        path: String,
        apply: impl Fn(&mut Raw, Option<&Json>) + Send + 'static,
    ) -> ListenId {
        let weak: Weak<Inner> = Arc::downgrade(self);
        self.db.listen(&path, move |event| {
            let Some(inner) = weak.upgrade() else { return };
            let value = match event {
                Event::Value(value) => Some(value),
                Event::Denied(_) => None,
            };
            {
                let mut state = inner.state.lock().expect("estado da comunidade");
                apply(&mut state.raw, value);
                inner.reconcile(&mut state);
            }
            (inner.notify)();
        })
    }

    fn drop_all(&self, ids: impl IntoIterator<Item = ListenId>) {
        for id in ids {
            self.db.unlisten(id);
        }
    }

    /// Depois de cada mudança: ajusta o que precisa ser acompanhado e refaz o retrato.
    fn reconcile(self: &Arc<Self>, state: &mut State) {
        let State {
            raw,
            subs,
            snapshot,
        } = state;
        let uid = self.uid.as_str();
        let Some(level) = raw.level else { return };
        let owner = level == Level::Owner;

        // Proprietário lê as listas inteiras; os demais, só o que é deles.
        if subs.mode != Some(owner) {
            self.drop_all(subs.mode_subs.drain(..));
            self.drop_all(subs.groups.drain().flat_map(|(_, ids)| ids));
            self.drop_all(subs.people.drain().flat_map(|(_, ids)| ids));
            raw.profiles = None;
            raw.avatars.clear();
            raw.net_admins.clear();
            raw.people.clear();
            raw.people_avatars.clear();
            subs.mode = Some(owner);
            subs.mode_subs = if owner {
                vec![
                    self.hook("community/profiles".into(), |raw, v| {
                        let all = v.and_then(Json::as_object);
                        raw.profiles = Some(
                            all.map(|map| {
                                map.iter()
                                    .filter_map(|(k, p)| Some((k.clone(), display_name(p)?)))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        );
                    }),
                    self.hook("community/roles".into(), |raw, v| {
                        raw.all_roles = v.map(levels).unwrap_or_default()
                    }),
                    self.hook("community/avatars".into(), |raw, v| {
                        raw.avatars = v.map(texts).unwrap_or_default()
                    }),
                    self.hook("community/netAdmins".into(), |raw, v| {
                        raw.net_admins = v.map(keys).unwrap_or_default()
                    }),
                    self.hook("community/roleDefs".into(), |raw, v| {
                        raw.all_defs = v
                            .and_then(Json::as_object)
                            .map(|map| {
                                map.iter()
                                    .filter_map(|(k, d)| Some((k.clone(), role_def(d)?)))
                                    .collect()
                            })
                            .unwrap_or_default();
                    }),
                    self.hook("community/memberRoles".into(), |raw, v| {
                        raw.all_member_roles = v.map(sets).unwrap_or_default();
                    }),
                ]
            } else {
                let me = uid.to_owned();
                let (me_profile, me_avatar, me_net) = (me.clone(), me.clone(), me.clone());
                vec![
                    self.hook(format!("community/profiles/{uid}"), move |raw, v| {
                        raw.profiles = Some(
                            v.and_then(display_name)
                                .map(|name| (me_profile.clone(), name))
                                .into_iter()
                                .collect(),
                        );
                    }),
                    self.hook(format!("community/avatars/{uid}"), move |raw, v| {
                        raw.avatars = v
                            .and_then(Json::as_str)
                            .map(|photo| (me_avatar.clone(), photo.to_owned()))
                            .into_iter()
                            .collect();
                    }),
                    self.hook(format!("community/netAdmins/{uid}"), move |raw, v| {
                        raw.net_admins = v
                            .filter(|flag| flag.as_bool() == Some(true))
                            .map(|_| me_net.clone())
                            .into_iter()
                            .collect();
                    }),
                    self.hook(format!("community/memberRoles/{uid}"), |raw, v| {
                        raw.own_roles = v.map(keys).unwrap_or_default()
                    }),
                ]
            };
        }

        // Quem não é proprietário: nome, cor e colegas de cada cargo que tem (e só desses).
        if !owner {
            let gone: Vec<String> = subs
                .groups
                .keys()
                .filter(|id| !raw.own_roles.contains(*id))
                .cloned()
                .collect();
            for id in gone {
                self.drop_all(subs.groups.remove(&id).unwrap_or_default());
                raw.group_defs.remove(&id);
                raw.group_members.remove(&id);
            }
            for id in &raw.own_roles {
                if subs.groups.contains_key(id) {
                    continue;
                }
                let (def_id, members_id) = (id.clone(), id.clone());
                subs.groups.insert(
                    id.clone(),
                    vec![
                        self.hook(format!("community/roleDefs/{id}"), move |raw, v| {
                            match v.and_then(role_def) {
                                Some(def) => drop(raw.group_defs.insert(def_id.clone(), def)),
                                None => drop(raw.group_defs.remove(&def_id)),
                            }
                        }),
                        self.hook(format!("community/roleMembers/{id}"), move |raw, v| {
                            raw.group_members
                                .insert(members_id.clone(), v.map(keys).unwrap_or_default());
                        }),
                    ],
                );
            }
        }

        let roles = if owner {
            raw.all_roles.clone()
        } else {
            BTreeMap::from([(uid.to_owned(), level)])
        };
        let role_defs = if owner {
            raw.all_defs.clone()
        } else {
            raw.group_defs.clone()
        };
        let member_roles = if owner {
            raw.all_member_roles.clone()
        } else {
            let mut all: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            if !raw.own_roles.is_empty() {
                all.insert(uid.to_owned(), raw.own_roles.clone());
            }
            for (role, members) in &raw.group_members {
                for member in members {
                    all.entry(member.clone()).or_default().insert(role.clone());
                }
            }
            all
        };

        // O servidor só entrega as categorias e canais que a conta pode ver, e uma leitura recusada
        // não é refeita sozinha: quando nível, cargos ou permissões mudam, pede tudo de novo.
        let permissions = format!(
            "{level:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
            member_roles.get(uid),
            raw.channel_access,
            raw.category_access,
            raw.channel_roles,
            raw.category_roles
        );
        if subs.permissions != permissions {
            subs.permissions = permissions;
            self.drop_all(subs.categories.drain().map(|(_, id)| id));
            self.drop_all(subs.channels.drain().map(|(_, id)| id));
            // O que já estava na tela fica até a resposta nova chegar; o que for recusado sai sozinho.
            if subs.index.is_empty() {
                subs.index = vec![
                    self.hook("community/categoryIds".into(), |raw, v| {
                        raw.category_ids = v.map(keys).unwrap_or_default()
                    }),
                    self.hook("community/channelIds".into(), |raw, v| {
                        raw.channel_ids = v.map(keys).unwrap_or_default()
                    }),
                ];
            }
        }
        let gone: Vec<String> = subs
            .categories
            .keys()
            .filter(|id| !raw.category_ids.contains(*id))
            .cloned()
            .collect();
        for id in gone {
            self.drop_all(subs.categories.remove(&id));
            raw.categories.remove(&id);
        }
        for id in &raw.category_ids {
            if subs.categories.contains_key(id) {
                continue;
            }
            let key = id.clone();
            let listen = self.hook(format!("community/categories/{id}"), move |raw, v| match v
                .filter(|item| item.is_object())
            {
                Some(item) => drop(raw.categories.insert(
                    key.clone(),
                    Category {
                        name: text(item, "name"),
                        order: item["order"].as_f64().unwrap_or(0.0),
                    },
                )),
                None => drop(raw.categories.remove(&key)),
            });
            subs.categories.insert(id.clone(), listen);
        }
        let gone: Vec<String> = subs
            .channels
            .keys()
            .filter(|id| !raw.channel_ids.contains(*id))
            .cloned()
            .collect();
        for id in gone {
            self.drop_all(subs.channels.remove(&id));
            raw.channels.remove(&id);
        }
        for id in &raw.channel_ids {
            if subs.channels.contains_key(id) {
                continue;
            }
            let key = id.clone();
            let listen = self.hook(format!("community/channels/{id}"), move |raw, v| {
                match v.filter(|item| item.is_object()) {
                    Some(item) => drop(
                        raw.channels.insert(
                            key.clone(),
                            Channel {
                                name: text(item, "name"),
                                description: text(item, "description"),
                                category_id: item["categoryId"]
                                    .as_str()
                                    .unwrap_or(DEFAULT_CATEGORY)
                                    .to_owned(),
                                enabled: item["enabled"].as_bool().unwrap_or(false),
                            },
                        ),
                    ),
                    None => drop(raw.channels.remove(&key)),
                }
            });
            subs.channels.insert(id.clone(), listen);
        }

        let mut next = Snapshot {
            loaded: raw.profiles.is_some(),
            uid: uid.to_owned(),
            level,
            owner,
            categories: raw.categories.clone(),
            channels: raw.channels.clone(),
            category_access: raw.category_access.clone(),
            channel_access: raw.channel_access.clone(),
            category_roles: raw.category_roles.clone(),
            channel_roles: raw.channel_roles.clone(),
            profiles: raw.profiles.clone().unwrap_or_default(),
            avatars: raw.avatars.clone(),
            roles,
            role_defs,
            member_roles,
            net_admins: raw.net_admins.clone(),
            resolving: BTreeSet::new(),
            presence: BTreeMap::new(),
        };
        next.categories.insert(
            DEFAULT_CATEGORY.into(),
            Category {
                name: "Lives".into(),
                order: 0.0,
            },
        );
        for (id, name, description) in SYSTEM_CHANNELS {
            next.channels.insert(
                id.into(),
                Channel {
                    name: name.into(),
                    description: description.into(),
                    category_id: DEFAULT_CATEGORY.into(),
                    enabled: true,
                },
            );
        }

        // Presença: só dos canais que a conta pode ver (os outros o servidor recusaria).
        let allowed: BTreeSet<String> = if next.loaded {
            next.channels
                .keys()
                .filter(|id| next.channel_allowed(id, uid))
                .cloned()
                .collect()
        } else {
            BTreeSet::new()
        };
        let gone: Vec<String> = subs
            .rooms
            .keys()
            .filter(|id| !allowed.contains(*id))
            .cloned()
            .collect();
        for id in gone {
            self.drop_all(subs.rooms.remove(&id).unwrap_or_default());
            raw.rooms.remove(&id);
        }
        for id in &allowed {
            if subs.rooms.contains_key(id) {
                continue;
            }
            let (members_room, streams_room) = (id.clone(), id.clone());
            subs.rooms.insert(
                id.clone(),
                vec![
                    self.hook(format!("rooms/{id}/members"), move |raw, v| {
                        raw.rooms.entry(members_room.clone()).or_default().members =
                            v.map(keys).unwrap_or_default().into_iter().collect();
                    }),
                    self.hook(format!("rooms/{id}/streams"), move |raw, v| {
                        raw.rooms.entry(streams_room.clone()).or_default().streams =
                            v.map(streams).unwrap_or_default();
                    }),
                ],
            );
        }
        next.presence = raw.rooms.clone();

        // Fora o proprietário, ninguém lista a comunidade: lê, uma a uma, as pessoas que aparecem
        // nos canais visíveis.
        if !owner {
            let seen: BTreeSet<String> = if next.loaded {
                raw.rooms
                    .values()
                    .flat_map(|room| room.members.iter().chain(room.streams.keys()))
                    .cloned()
                    .collect()
            } else {
                BTreeSet::new()
            };
            let gone: Vec<String> = subs
                .people
                .keys()
                .filter(|id| !seen.contains(*id))
                .cloned()
                .collect();
            for id in gone {
                self.drop_all(subs.people.remove(&id).unwrap_or_default());
                raw.people.remove(&id);
                raw.people_avatars.remove(&id);
            }
            for id in &seen {
                if subs.people.contains_key(id) {
                    continue;
                }
                let (profile_id, avatar_id) = (id.clone(), id.clone());
                subs.people.insert(
                    id.clone(),
                    vec![
                        self.hook(format!("community/profiles/{id}"), move |raw, v| {
                            raw.people
                                .insert(profile_id.clone(), v.and_then(display_name));
                        }),
                        self.hook(format!("community/avatars/{id}"), move |raw, v| {
                            match v.and_then(Json::as_str) {
                                Some(photo) => drop(
                                    raw.people_avatars
                                        .insert(avatar_id.clone(), photo.to_owned()),
                                ),
                                None => drop(raw.people_avatars.remove(&avatar_id)),
                            }
                        }),
                    ],
                );
            }
            for id in &seen {
                match raw.people.get(id) {
                    Some(Some(name)) => drop(
                        next.profiles
                            .entry(id.clone())
                            .or_insert_with(|| name.clone()),
                    ),
                    Some(None) => {}
                    None => drop(next.resolving.insert(id.clone())),
                }
                if let Some(photo) = raw.people_avatars.get(id) {
                    next.avatars
                        .entry(id.clone())
                        .or_insert_with(|| photo.clone());
                }
            }
        }
        *snapshot = next;
    }
}
