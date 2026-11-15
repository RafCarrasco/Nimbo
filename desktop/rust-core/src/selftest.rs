//! Confere o cliente do banco contra os emuladores locais (`npm run emulators`), com as regras de
//! verdade: login, leitura recusada e liberada, gravação, mudança feita por outra conexão,
//! mensagem grande (em pedaços), presença que some quando a conexão cai, e reconexão.
//!
//! Usa contas de teste novas a cada rodada e apaga o que criou.

use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nimbo_core::Json;
use nimbo_core::auth::{self, Session};
use nimbo_core::config::Project;
use nimbo_core::rtdb::{Db, DbError, Event, Status};
use serde_json::json;

type AnyError = Box<dyn std::error::Error>;
const WAIT: Duration = Duration::from_secs(8);

/// Grava direto no emulador como administrador (o emulador aceita `Bearer owner`), para fazer o
/// papel do proprietário aprovando alguém.
fn admin(method: &str, path: &str, body: &Json) -> Result<(), AnyError> {
    let project = Project::emulator();
    let url = format!("http://{}/{path}.json?ns={}", project.db_host, project.db_namespace);
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let request = ureq::http::Request::builder()
        .method(method)
        .uri(&url)
        .header("Authorization", "Bearer owner")
        .header("Content-Type", "application/json")
        .body(body.to_string())?;
    let status = agent.run(request)?.status();
    if !status.is_success() {
        return Err(format!("emulador recusou {method} {path}: {status}").into());
    }
    Ok(())
}

fn watch(db: &Db, path: &str) -> Receiver<Event> {
    let (tx, rx) = channel();
    db.listen(path, move |event| drop(tx.send(event.clone())));
    rx
}

fn expect(rx: &Receiver<Event>, what: &str, pass: impl Fn(&Event) -> bool) -> Result<Event, AnyError> {
    let until = Instant::now() + WAIT;
    let mut seen = Vec::new();
    while let Ok(event) = rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
        if pass(&event) {
            return Ok(event);
        }
        seen.push(format!("{event:?}"));
    }
    Err(format!("não chegou: {what}. Chegou: {}", seen.join(" | ")).into())
}

fn account(project: &Project, name: &str) -> Result<(Session, Db), AnyError> {
    let session = auth::sign_in_google(project, &auth::dev_credential(name), "")?;
    let db = Db::connect(project);
    db.set_token(Some(session.id_token.clone()));
    Ok((session, db))
}

fn step(name: &str) {
    println!("ok  {name}");
}

pub fn run() -> Result<(), AnyError> {
    let project = Project::emulator();
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let (name_a, name_b) = (format!("core-a{stamp}"), format!("core-b{stamp}"));
    let (a, db_a) = account(&project, &name_a)?;
    let (b, db_b) = account(&project, &name_b)?;
    let result = checks(&project, &a, &db_a, &b, &db_b);
    for uid in [&a.uid, &b.uid] {
        for path in ["community/approved", "community/requests", "community/avatars", "rooms/geral/members"] {
            let _ = admin("DELETE", &format!("{path}/{uid}"), &Json::Null);
        }
    }
    result?;
    println!("tudo certo");
    Ok(())
}

fn checks(project: &Project, a: &Session, db_a: &Db, b: &Session, db_b: &Db) -> Result<(), AnyError> {
    if a.uid.is_empty() || a.uid == b.uid || a.email != format!("{}@nimbo.test", a.email.split('@').next().unwrap_or("")) {
        return Err("contas de teste inesperadas".into());
    }
    step("login de duas contas de teste no emulador");

    let renewed = auth::refresh(project, a)?;
    if renewed.uid != a.uid || renewed.id_token.is_empty() {
        return Err("a renovação devolveu outra conta".into());
    }
    step("renovação do token devolve a mesma conta");

    let (tx, status) = channel();
    db_a.on_status(move |s| drop(tx.send(s.clone())));
    let until = Instant::now() + WAIT;
    while status.recv_timeout(until.saturating_duration_since(Instant::now()))? != Status::Connected(true) {}
    step("conexão pronta");
    let drift = (db_a.server_now_ms() - SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64).abs();
    if drift > 5_000 {
        return Err(format!("relógio do servidor fora: {drift} ms").into());
    }

    // Antes de aprovada, a conta lê só a própria aprovação.
    let approval = watch(db_a, &format!("community/approved/{}", a.uid));
    expect(&approval, "aprovação vazia", |e| *e == Event::Value(Json::Null))?;
    let members = watch(db_a, "rooms/geral/members");
    expect(&members, "sala recusada", |e| matches!(e, Event::Denied(_)))?;
    if db_a.get("community/profiles") != Err(DbError::Denied) {
        return Err("a lista de perfis deveria ser recusada".into());
    }
    step("leituras recusadas pelas regras chegam como recusa");

    db_a.set(&format!("community/requests/{}", a.uid), json!({ "email": a.email, "createdAt": db_a.server_now_ms() }))?;
    if db_a.set(&format!("community/approved/{}", a.uid), json!(true)) != Err(DbError::Denied) {
        return Err("a conta não pode aprovar a si mesma".into());
    }
    step("gravação permitida passa e a proibida é recusada");

    // O proprietário aprova (por fora): a mudança chega sozinha e o que foi recusado volta a ser pedido.
    for uid in [&a.uid, &b.uid] {
        admin("PUT", &format!("community/approved/{uid}"), &json!(true))?;
    }
    expect(&approval, "aprovação", |e| *e == Event::Value(json!(true)))?;
    db_a.retry_denied();
    expect(&members, "sala liberada", |e| matches!(e, Event::Value(_)))?;
    step("mudança feita por outra conexão chega; leitura recusada volta depois de liberada");

    // Presença: A entra na sala e pede ao servidor para tirá-la se a conexão cair.
    let me = format!("rooms/geral/members/{}", a.uid);
    db_a.on_disconnect_remove(&me)?;
    db_a.set(&me, json!({ "joinedAt": db_a.server_now_ms(), "app": "nimbo-core", "sid": "teste" }))?;
    expect(&members, "a própria gravação de volta", |e| matches!(e, Event::Value(v) if v[&a.uid]["sid"] == "teste"))?;
    let seen_by_b = watch(db_b, "rooms/geral/members");
    expect(&seen_by_b, "B vê A na sala", |e| matches!(e, Event::Value(v) if v[&a.uid]["app"] == "nimbo-core"))?;
    step("a própria gravação volta do servidor e a outra conta vê");

    // Várias gravações de uma vez, e uma leitura única.
    db_a.update(&me, json!({ "app": "nimbo-core 2", "sid": null }))?;
    let now = db_a.get(&me)?;
    if now["app"] != "nimbo-core 2" || !now["sid"].is_null() || !now["joinedAt"].is_number() {
        return Err(format!("atualização parcial errada: {now}").into());
    }
    step("atualização de vários campos e leitura única");

    // Mensagem maior que um pedaço (16 KB), nos dois sentidos: uma foto de perfil.
    let photo = format!("data:image/webp;base64,{}", "A".repeat(15_900));
    let avatar = format!("community/avatars/{}", a.uid);
    db_a.set(&avatar, json!(photo))?;
    if db_b.get(&avatar)? != json!(photo) {
        return Err("a foto grande não voltou igual".into());
    }
    step("mensagem grande vai e volta em pedaços");

    // Queda da conexão: o servidor tira A da sala, e A volta sozinha a acompanhar o que acompanhava.
    db_a.debug_drop_connection();
    expect(&seen_by_b, "A some da sala", |e| matches!(e, Event::Value(v) if v[&a.uid].is_null()))?;
    let until = Instant::now() + WAIT;
    while status.recv_timeout(until.saturating_duration_since(Instant::now()))? != Status::Connected(true) {}
    db_b.set(&format!("rooms/geral/members/{}", b.uid), json!({ "joinedAt": db_b.server_now_ms() }))?;
    expect(&members, "A, reconectada, vê B", |e| matches!(e, Event::Value(v) if v[&b.uid].is_object()))?;
    step("queda: o servidor remove a presença, a conexão volta e as escutas continuam");

    db_a.close();
    db_b.remove(&format!("rooms/geral/members/{}", b.uid))?;
    db_b.close();
    Ok(())
}
