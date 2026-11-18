//! Gravações pedidas pela interface (cargos, níveis, canais, categorias, aprovações). Cada uma roda
//! numa thread própria (a conversa com o servidor não pode travar o desenho) e devolve a mensagem
//! para o aviso, como os `toast.success`/`toast.error` do site. Quem decide o que é permitido é o
//! banco; a mensagem de erro dele chega à pessoa.

use std::collections::{BTreeMap, BTreeSet};

use nimbo_core::admin;
use nimbo_core::community::Level;
use nimbo_core::rtdb::{Db, DbError};

#[derive(Clone, Debug)]
pub enum Op {
    SetMemberRole {
        uid: String,
        role: String,
        on: bool,
        message: String,
    },
    SetLevel {
        uid: String,
        level: Level,
    },
    Revoke {
        uid: String,
        roles: BTreeSet<String>,
    },
    Approve {
        uid: String,
        roles: Vec<String>,
        message: String,
    },
    Reject {
        uid: String,
    },
    CreateRole {
        name: String,
        color: String,
    },
    UpdateRole {
        id: String,
        name: String,
        color: String,
    },
    DeleteRole {
        id: String,
        member_roles: BTreeMap<String, BTreeSet<String>>,
        category_roles: BTreeMap<String, Vec<String>>,
        channel_roles: BTreeMap<String, Vec<String>>,
    },
    SetAccess {
        category: bool,
        id: String,
        level: Level,
    },
    SetRoles {
        category: bool,
        id: String,
        roles: Vec<String>,
        message: String,
    },
    CreateChannel {
        name: String,
        category: String,
    },
    EditChannel {
        id: String,
        name: String,
        description: String,
        category: String,
    },
    DeleteChannel {
        id: String,
    },
    CreateCategory {
        name: String,
    },
    RenameCategory {
        id: String,
        name: String,
    },
    DeleteCategory {
        id: String,
    },
    RemoveSession {
        room: String,
        uid: String,
    },
    /// Várias gravações em sequência, com uma mensagem só (salvar a janela do canal).
    Batch {
        ops: Vec<Op>,
        message: String,
    },
}

/// O que voltou de uma gravação: a mensagem do aviso e, ao criar um cargo, o id dele.
pub struct Done {
    pub result: Result<String, String>,
    pub created_role: Option<String>,
}

fn error(e: DbError) -> String {
    match e {
        DbError::Denied => "Sem permissão para isso.".to_owned(),
        other => format!("Não deu certo: {other}"),
    }
}

impl Op {
    /// Faz a gravação (bloqueia até o servidor responder).
    pub fn run(self, db: &Db, me: &str) -> Done {
        let mut created_role = None;
        let result =
            match self {
                Op::SetMemberRole {
                    uid,
                    role,
                    on,
                    message,
                } => admin::set_member_role(db, &uid, &role, on).map(|()| message),
                Op::SetLevel { uid, level } => {
                    admin::set_level(db, &uid, level).map(|()| "Nível atualizado".to_owned())
                }
                Op::Revoke { uid, roles } => {
                    admin::revoke_member(db, &uid, &roles).map(|()| "Acesso revogado".to_owned())
                }
                Op::Approve {
                    uid,
                    roles,
                    message,
                } => admin::approve_member(db, &uid, &roles).map(|()| message),
                Op::Reject { uid } => {
                    admin::reject_request(db, &uid).map(|()| "Solicitação recusada".to_owned())
                }
                Op::CreateRole { name, color } => admin::create_role(db, &name, &color).map(|id| {
                    created_role = Some(id);
                    "Cargo criado".to_owned()
                }),
                Op::UpdateRole { id, name, color } => admin::update_role(db, &id, &name, &color)
                    .map(|()| "Cargo atualizado".to_owned()),
                Op::DeleteRole {
                    id,
                    member_roles,
                    category_roles,
                    channel_roles,
                } => admin::delete_role(db, &id, &member_roles, &category_roles, &channel_roles)
                    .map(|()| "Cargo excluído".to_owned()),
                Op::SetAccess {
                    category,
                    id,
                    level,
                } => admin::set_access(db, category, &id, level)
                    .map(|()| "Permissão atualizada".to_owned()),
                Op::SetRoles {
                    category,
                    id,
                    roles,
                    message,
                } => admin::set_roles(db, category, &id, &roles).map(|()| message),
                Op::CreateChannel { name, category } => {
                    admin::create_channel(db, me, &name, "", &category)
                        .map(|_| "Canal criado".to_owned())
                }
                Op::EditChannel {
                    id,
                    name,
                    description,
                    category,
                } => admin::edit_channel(db, &id, &name, &description, &category)
                    .map(|()| "Canal atualizado".to_owned()),
                Op::DeleteChannel { id } => {
                    admin::delete_channel(db, &id).map(|()| "Canal excluído".to_owned())
                }
                Op::CreateCategory { name } => {
                    admin::create_category(db, me, &name).map(|_| "Categoria criada".to_owned())
                }
                Op::RenameCategory { id, name } => admin::rename_category(db, &id, &name)
                    .map(|()| "Categoria atualizada".to_owned()),
                Op::DeleteCategory { id } => {
                    admin::delete_category(db, &id).map(|()| "Categoria excluída".to_owned())
                }
                Op::RemoveSession { room, uid } => admin::remove_room_session(db, &room, &uid)
                    .map(|()| "Sessão removida".to_owned()),
                Op::Batch { ops, message } => {
                    let mut failed = None;
                    for op in ops {
                        if let Err(e) = op.run(db, me).result {
                            failed = Some(e);
                            break;
                        }
                    }
                    return Done {
                        result: failed.map_or(Ok(message), Err),
                        created_role: None,
                    };
                }
            };
        Done {
            result: result.map_err(error),
            created_role,
        }
    }
}
