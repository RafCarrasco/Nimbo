//! Gravações da administração da comunidade: as mesmas de `src/community.ts` do site, no mesmo
//! formato e com as mesmas gravações juntas (tudo ou nada). Quem pode o quê é decidido pelas regras
//! do banco (`scripts/build-rules.mjs`); a interface só esconde o que a conta não pode fazer.
//!
//! Todas bloqueiam até o servidor responder: chame fora da thread que desenha.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, json};

use crate::Json;
use crate::community::{DEFAULT_CATEGORY, Level, SYSTEM_CHANNELS};
use crate::rtdb::{Db, DbError, push_id};

/// Quantos cargos uma categoria ou canal aceita (as regras só olham as posições "0".."4").
pub const ROLE_SLOTS: usize = 5;

fn invalid(message: &str) -> DbError {
    DbError::Server(message.to_owned())
}

/// Nome de canal ou categoria: de 2 a 32 caracteres (as regras conferem o mesmo).
pub fn name_error(name: &str) -> Option<&'static str> {
    let length = name.trim().chars().count();
    (!(2..=32).contains(&length)).then_some("Use de 2 a 32 caracteres.")
}

/// Nome de cargo: de 2 a 24 caracteres.
pub fn role_name_error(name: &str) -> Option<&'static str> {
    let length = name.trim().chars().count();
    (!(2..=24).contains(&length)).then_some("Use de 2 a 24 caracteres.")
}

/// Cor de cargo no formato `#rrggbb`.
pub fn valid_color(color: &str) -> bool {
    color.len() == 7 && color.starts_with('#') && color[1..].chars().all(|c| c.is_ascii_hexdigit())
}

pub fn is_system_channel(id: &str) -> bool {
    SYSTEM_CHANNELS.iter().any(|(known, ..)| *known == id)
}

/// Lista de cargos no formato que as regras leem: posições "0".."4".
fn slots(role_ids: &[String]) -> Json {
    if role_ids.is_empty() {
        return Json::Null;
    }
    Json::Object(
        role_ids
            .iter()
            .take(ROLE_SLOTS)
            .enumerate()
            .map(|(i, id)| (i.to_string(), json!(id)))
            .collect(),
    )
}

/// Dá ou tira um cargo: `memberRoles` (o que as regras consultam) e `roleMembers` (o contrário)
/// gravados juntos, como `setMemberRole` no site.
pub fn set_member_role(db: &Db, uid: &str, role: &str, on: bool) -> Result<(), DbError> {
    let value = if on { json!(true) } else { Json::Null };
    db.update("community", json!({ format!("memberRoles/{uid}/{role}"): value, format!("roleMembers/{role}/{uid}"): value }))
}

/// Nível de administração (só o proprietário grava; ninguém vira proprietário por aqui).
pub fn set_level(db: &Db, uid: &str, level: Level) -> Result<(), DbError> {
    let value = match level {
        Level::Admin => "admin",
        Level::Member => "member",
        Level::Owner => return Err(invalid("Nível inválido")),
    };
    db.set(&format!("community/roles/{uid}"), json!(value))
}

/// Revogar tira tudo o que dava acesso: aprovação, nível, cargos e a permissão de rede. Nome e foto
/// ficam (`revokeMember` no site).
pub fn revoke_member(db: &Db, uid: &str, roles: &BTreeSet<String>) -> Result<(), DbError> {
    let mut changes = Map::new();
    for path in ["approved", "roles", "netAdmins", "memberRoles"] {
        changes.insert(format!("{path}/{uid}"), Json::Null);
    }
    for role in roles {
        changes.insert(format!("roleMembers/{role}/{uid}"), Json::Null);
    }
    db.update("community", Json::Object(changes))
}

/// Aprova já nos cargos escolhidos. Quem entra começa do zero: nível e cargos de uma passagem
/// anterior não voltam (`approveMember` no site).
pub fn approve_member(db: &Db, uid: &str, roles: &[String]) -> Result<(), DbError> {
    let mut changes = Map::new();
    changes.insert(format!("approved/{uid}"), json!(true));
    changes.insert(format!("roles/{uid}"), Json::Null);
    let mine: Map<String, Json> = roles.iter().map(|id| (id.clone(), json!(true))).collect();
    changes.insert(
        format!("memberRoles/{uid}"),
        if mine.is_empty() {
            Json::Null
        } else {
            Json::Object(mine)
        },
    );
    for id in roles {
        changes.insert(format!("roleMembers/{id}/{uid}"), json!(true));
    }
    db.update("community", Json::Object(changes))?;
    let _ = db.remove(&format!("community/requests/{uid}"));
    Ok(())
}

pub fn reject_request(db: &Db, uid: &str) -> Result<(), DbError> {
    db.remove(&format!("community/requests/{uid}"))
}

/// Cria um cargo e devolve o id.
pub fn create_role(db: &Db, name: &str, color: &str) -> Result<String, DbError> {
    if let Some(error) = role_name_error(name) {
        return Err(invalid(error));
    }
    if !valid_color(color) {
        return Err(invalid("Cor inválida"));
    }
    let id = push_id(db.server_now_ms());
    db.set(
        &format!("community/roleDefs/{id}"),
        json!({ "name": name.trim(), "color": color, "createdAt": db.server_now_ms() }),
    )?;
    Ok(id)
}

pub fn update_role(db: &Db, id: &str, name: &str, color: &str) -> Result<(), DbError> {
    if let Some(error) = role_name_error(name) {
        return Err(invalid(error));
    }
    if !valid_color(color) {
        return Err(invalid("Cor inválida"));
    }
    db.update(
        &format!("community/roleDefs/{id}"),
        json!({ "name": name.trim(), "color": color }),
    )
}

/// Excluir um cargo também o tira das pessoas e das categorias e canais, numa gravação só.
pub fn delete_role(
    db: &Db,
    id: &str,
    member_roles: &BTreeMap<String, BTreeSet<String>>,
    category_roles: &BTreeMap<String, Vec<String>>,
    channel_roles: &BTreeMap<String, Vec<String>>,
) -> Result<(), DbError> {
    let mut changes = Map::new();
    changes.insert(format!("roleDefs/{id}"), Json::Null);
    changes.insert(format!("roleMembers/{id}"), Json::Null);
    for (uid, roles) in member_roles {
        if roles.contains(id) {
            changes.insert(format!("memberRoles/{uid}/{id}"), Json::Null);
        }
    }
    for (key, lists) in [
        ("categoryRoles", category_roles),
        ("channelRoles", channel_roles),
    ] {
        for (target, list) in lists {
            if list.iter().any(|role| role == id) {
                let rest: Vec<String> = list.iter().filter(|role| *role != id).cloned().collect();
                changes.insert(format!("{key}/{target}"), slots(&rest));
            }
        }
    }
    db.update("community", Json::Object(changes))
}

/// Nível mínimo de uma categoria ou canal (`categoryAccess`/`channelAccess`).
pub fn set_access(db: &Db, category: bool, id: &str, level: Level) -> Result<(), DbError> {
    let value = match level {
        Level::Owner => "owner",
        Level::Admin => "admin",
        Level::Member => "member",
    };
    let node = if category {
        "categoryAccess"
    } else {
        "channelAccess"
    };
    db.set(&format!("community/{node}/{id}"), json!(value))
}

/// Cargos que veem uma categoria ou canal (no máximo 5; vazio = todos que passam pelo nível).
pub fn set_roles(db: &Db, category: bool, id: &str, role_ids: &[String]) -> Result<(), DbError> {
    let node = if category {
        "categoryRoles"
    } else {
        "channelRoles"
    };
    db.set(&format!("community/{node}/{id}"), slots(role_ids))
}

pub fn create_channel(
    db: &Db,
    uid: &str,
    name: &str,
    description: &str,
    category: &str,
) -> Result<String, DbError> {
    if let Some(error) = name_error(name) {
        return Err(invalid(error));
    }
    let id = push_id(db.server_now_ms());
    let description: String = description.trim().chars().take(160).collect();
    db.update(
        "community",
        json!({
            format!("channels/{id}"): {
                "name": name.trim(),
                "categoryId": category,
                "description": description,
                "createdBy": uid,
                "createdAt": db.server_now_ms(),
                "visibility": "public",
                "enabled": true,
            },
            format!("channelIds/{id}"): true,
        }),
    )?;
    Ok(id)
}

/// Nome, descrição e categoria de um canal (os padrão não mudam).
pub fn edit_channel(
    db: &Db,
    id: &str,
    name: &str,
    description: &str,
    category: &str,
) -> Result<(), DbError> {
    if is_system_channel(id) {
        return Err(invalid("Canais padrão não podem ser editados"));
    }
    if let Some(error) = name_error(name) {
        return Err(invalid(error));
    }
    let description: String = description.trim().chars().take(160).collect();
    db.update(
        &format!("community/channels/{id}"),
        json!({ "name": name.trim(), "description": description, "categoryId": category }),
    )
}

pub fn delete_channel(db: &Db, id: &str) -> Result<(), DbError> {
    if is_system_channel(id) {
        return Err(invalid("Canais padrão não podem ser excluídos"));
    }
    db.update(
        "community",
        json!({ format!("channels/{id}"): null, format!("channelIds/{id}"): null }),
    )?;
    let _ = db.remove(&format!("community/channelAccess/{id}"));
    let _ = db.remove(&format!("community/channelRoles/{id}"));
    // A sala do canal (presença, lives, sinalização) não tem mais dono: sem isso as sessões ficam presas.
    let _ = db.remove(&format!("rooms/{id}"));
    Ok(())
}

pub fn create_category(db: &Db, uid: &str, name: &str) -> Result<String, DbError> {
    if let Some(error) = name_error(name) {
        return Err(invalid(error));
    }
    let id = push_id(db.server_now_ms());
    let order = db.server_now_ms();
    db.update(
        "community",
        json!({
            format!("categories/{id}"): { "name": name.trim(), "createdAt": order, "createdBy": uid, "order": order },
            format!("categoryIds/{id}"): order,
        }),
    )?;
    Ok(id)
}

pub fn rename_category(db: &Db, id: &str, name: &str) -> Result<(), DbError> {
    if id == DEFAULT_CATEGORY {
        return Err(invalid("Categoria padrão não pode ser renomeada"));
    }
    if let Some(error) = name_error(name) {
        return Err(invalid(error));
    }
    db.update(
        &format!("community/categories/{id}"),
        json!({ "name": name.trim() }),
    )
}

pub fn delete_category(db: &Db, id: &str) -> Result<(), DbError> {
    if id == DEFAULT_CATEGORY {
        return Err(invalid("Categoria padrão não pode ser excluída"));
    }
    db.update(
        "community",
        json!({ format!("categories/{id}"): null, format!("categoryIds/{id}"): null }),
    )?;
    let _ = db.remove(&format!("community/categoryAccess/{id}"));
    let _ = db.remove(&format!("community/categoryRoles/{id}"));
    Ok(())
}

/// Limpa uma sessão abandonada num canal (navegador fechado antes do "ao desconectar" ser gravado).
pub fn remove_room_session(db: &Db, room: &str, uid: &str) -> Result<(), DbError> {
    db.remove(&format!("rooms/{room}/streams/{uid}"))?;
    let _ = db.remove(&format!("rooms/{room}/watching/{uid}"));
    let _ = db.remove(&format!("rooms/{room}/previews/{uid}"));
    db.remove(&format!("rooms/{room}/members/{uid}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_lists_use_fixed_slots() {
        assert_eq!(slots(&[]), Json::Null);
        let ids: Vec<String> = (0..7).map(|i| format!("r{i}")).collect();
        let value = slots(&ids);
        assert_eq!(value.as_object().map(Map::len), Some(ROLE_SLOTS));
        assert_eq!(value["0"], json!("r0"));
        assert_eq!(value["4"], json!("r4"));
    }

    #[test]
    fn names_and_colors_follow_the_rules() {
        assert!(name_error("a").is_some());
        assert!(name_error(" ab ").is_none());
        assert!(name_error(&"x".repeat(33)).is_some());
        assert!(role_name_error(&"x".repeat(25)).is_some());
        assert!(valid_color("#7c6cFF"));
        assert!(!valid_color("7c6cff"));
        assert!(!valid_color("#7c6cfg"));
    }
}
