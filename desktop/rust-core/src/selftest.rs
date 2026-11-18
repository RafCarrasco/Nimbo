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
use nimbo_core::room::{Room, Signal};
use nimbo_core::rtdb::{Db, DbError, Event, Status};
use serde_json::json;

type AnyError = Box<dyn std::error::Error>;
const WAIT: Duration = Duration::from_secs(8);

/// Grava direto no emulador como administrador (o emulador aceita `Bearer owner`), para fazer o
/// papel do proprietário aprovando alguém.
fn admin(method: &str, path: &str, body: &Json) -> Result<(), AnyError> {
    let project = Project::emulator();
    let url = format!(
        "http://{}/{path}.json?ns={}",
        project.db_host, project.db_namespace
    );
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
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

fn expect(
    rx: &Receiver<Event>,
    what: &str,
    pass: impl Fn(&Event) -> bool,
) -> Result<Event, AnyError> {
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

/// Espera uma condição ficar verdadeira, conferindo de 50 em 50 ms.
fn wait_until(what: &str, mut pass: impl FnMut() -> bool) -> Result<(), AnyError> {
    let deadline = Instant::now() + WAIT;
    while !pass() {
        if Instant::now() > deadline {
            return Err(format!("não aconteceu: {what}").into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
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
        for path in [
            "community/approved",
            "community/requests",
            "community/avatars",
            "rooms/geral/members",
        ] {
            let _ = admin("DELETE", &format!("{path}/{uid}"), &Json::Null);
        }
    }
    result?;
    admin_checks(&project, stamp)?;
    println!("tudo certo");
    Ok(())
}

/// O que o proprietário lê da administração (a raiz `community` inteira ninguém lê).
fn owner_view(db: &Db) -> Result<Json, AnyError> {
    let mut all = serde_json::Map::new();
    for path in [
        "approved",
        "requests",
        "roleDefs",
        "memberRoles",
        "roleMembers",
    ] {
        all.insert(path.to_owned(), db.get(&format!("community/{path}"))?);
    }
    Ok(Json::Object(all))
}

/// Administração com as regras de verdade: o que o proprietário pode, o que o administrador pode e o
/// que o banco recusa, e o que cada um enxerga de cargos (só o proprietário conhece).
fn admin_checks(project: &Project, stamp: u128) -> Result<(), AnyError> {
    use nimbo_core::admin;
    use nimbo_core::community::{Community, Level};

    let (owner, db_o) = account(project, &format!("core-o{stamp}"))?;
    let (moderator, db_m) = account(project, &format!("core-m{stamp}"))?;
    let (member, db_x) = account(project, &format!("core-x{stamp}"))?;
    let (newcomer, db_n) = account(project, &format!("core-n{stamp}"))?;
    let people = [&owner.uid, &moderator.uid, &member.uid, &newcomer.uid];
    for uid in &people[..3] {
        admin("PUT", &format!("community/approved/{uid}"), &json!(true))?;
    }
    admin(
        "PUT",
        &format!("community/roles/{}", owner.uid),
        &json!("owner"),
    )?;
    admin(
        "PUT",
        &format!("community/roles/{}", moderator.uid),
        &json!("admin"),
    )?;
    let result = (|| -> Result<(), AnyError> {
        let denied = |result: Result<(), DbError>, what: &str| -> Result<(), AnyError> {
            match result {
                Err(DbError::Denied) => Ok(()),
                other => Err(format!("{what} devia ser recusado, deu {other:?}").into()),
            }
        };
        // Cargos: só o proprietário cria e dá; o administrador é recusado.
        let role = admin::create_role(&db_o, "Teste", "#4cc3f5")
            .map_err(|e| format!("create_role: {e}"))?;
        admin::update_role(&db_o, &role, "Grupo de teste", "#ff7ac6")
            .map_err(|e| format!("update_role: {e}"))?;
        admin::set_member_role(&db_o, &member.uid, &role, true)
            .map_err(|e| format!("set_member_role: {e}"))?;
        denied(
            admin::set_member_role(&db_m, &moderator.uid, &role, true),
            "administrador se dar cargo",
        )?;
        denied(
            admin::set_level(&db_m, &member.uid, Level::Admin),
            "administrador promover alguém",
        )?;
        let both = owner_view(&db_o)?;
        if both["memberRoles"][&member.uid][&role] != true
            || both["roleMembers"][&role][&member.uid] != true
        {
            return Err("memberRoles e roleMembers não foram gravados juntos".into());
        }
        // O que cada um enxerga: o membro conhece o próprio cargo (o id), nunca nome nem cor.
        let seen_by_member = Community::start(&db_x, &member.uid, || {});
        let seen_by_owner = Community::start(&db_o, &owner.uid, || {});
        wait_until("o membro carrega a comunidade com o próprio cargo", || {
            let data = seen_by_member.snapshot();
            data.loaded
                && data
                    .member_roles
                    .get(&member.uid)
                    .is_some_and(|set| set.contains(&role))
        })?;
        let data = seen_by_member.snapshot();
        if !data.role_defs.is_empty()
            || data.role_color(&member.uid).is_some()
            || !data.restricted_label(Some(Level::Admin), None).is_empty()
        {
            return Err("quem não é proprietário recebeu nome, cor ou cadeado de cargo".into());
        }
        wait_until(
            "o proprietário vê o cargo com nome, cor e uma pessoa",
            || {
                let data = seen_by_owner.snapshot();
                data.role_defs
                    .get(&role)
                    .is_some_and(|def| def.name == "Grupo de teste")
                    && data.role_count(&role) == 1
                    && data.role_color(&member.uid) == Some("#ff7ac6")
            },
        )?;
        step(
            "cargos: o proprietário cria e dá; o administrador é recusado; só o proprietário vê nome e cor",
        );

        // Canais: o administrador cria, edita, muda o nível e exclui; "só o proprietário" e cargos, não.
        let channel = admin::create_channel(&db_m, &moderator.uid, "canal de teste", "", "lives")
            .map_err(|e| format!("create_channel: {e}"))?;
        admin::edit_channel(&db_m, &channel, "canal editado", "descrição nova", "lives")
            .map_err(|e| format!("edit_channel: {e}"))?;
        admin::set_access(&db_m, false, &channel, Level::Admin)
            .map_err(|e| format!("set_access: {e}"))?;
        denied(
            admin::set_access(&db_m, false, &channel, Level::Owner),
            "administrador pôr 'só o proprietário'",
        )?;
        denied(
            admin::set_roles(&db_m, false, &channel, std::slice::from_ref(&role)),
            "administrador marcar cargo",
        )?;
        admin::set_access(&db_m, false, &channel, Level::Member)
            .map_err(|e| format!("set_access: {e}"))?;
        // Canal privado do cargo: quem tem vê; o administrador sem o cargo deixa de ver.
        admin::set_roles(&db_o, false, &channel, std::slice::from_ref(&role))
            .map_err(|e| format!("set_roles: {e}"))?;
        if db_x.get(&format!("community/channels/{channel}"))?["name"] != "canal editado" {
            return Err("quem tem o cargo não leu o canal privado".into());
        }
        if db_m.get(&format!("community/channels/{channel}")) != Err(DbError::Denied) {
            return Err("administrador sem o cargo leu o canal privado".into());
        }
        denied(
            admin::edit_channel(&db_m, &channel, "às cegas", "", "lives"),
            "administrador editar canal que não vê",
        )?;
        if admin::edit_channel(&db_o, "geral", "Outro nome", "", "lives").is_ok() {
            return Err("canal padrão foi editado".into());
        }
        admin::set_roles(&db_o, false, &channel, &[]).map_err(|e| format!("set_roles: {e}"))?;
        admin::delete_channel(&db_m, &channel).map_err(|e| format!("delete_channel: {e}"))?;
        if !db_o
            .get(&format!("community/channels/{channel}"))?
            .is_null()
        {
            return Err("canal excluído continua lá".into());
        }
        let category = admin::create_category(&db_m, &moderator.uid, "categoria de teste")
            .map_err(|e| format!("create_category: {e}"))?;
        admin::rename_category(&db_m, &category, "categoria renomeada")
            .map_err(|e| format!("rename_category: {e}"))?;
        admin::delete_category(&db_m, &category).map_err(|e| format!("delete_category: {e}"))?;
        step(
            "canais e categorias: o administrador cria, edita e exclui o que vê; cargo e 'só o proprietário' só o dono",
        );

        // Aprovação já num cargo, revogação, sessão abandonada e exclusão do cargo.
        admin(
            "PUT",
            &format!("community/requests/{}", newcomer.uid),
            &json!({ "email": newcomer.email, "createdAt": 1 }),
        )?;
        denied(
            admin::approve_member(&db_m, &newcomer.uid, &[]),
            "administrador aprovar",
        )?;
        admin::approve_member(&db_o, &newcomer.uid, std::slice::from_ref(&role))
            .map_err(|e| format!("approve_member: {e}"))?;
        let state = owner_view(&db_o)?;
        if state["approved"][&newcomer.uid] != true
            || state["memberRoles"][&newcomer.uid][&role] != true
            || !state["requests"][&newcomer.uid].is_null()
        {
            return Err(
                format!("aprovação incompleta: {}", state["approved"][&newcomer.uid]).into(),
            );
        }
        admin::revoke_member(
            &db_o,
            &newcomer.uid,
            &std::collections::BTreeSet::from([role.clone()]),
        )
        .map_err(|e| format!("revoke_member: {e}"))?;
        let state = owner_view(&db_o)?;
        if !state["approved"][&newcomer.uid].is_null()
            || !state["roleMembers"][&role][&newcomer.uid].is_null()
        {
            return Err("revogar não tirou aprovação e cargo".into());
        }
        admin(
            "PUT",
            "rooms/geral/members/sessao-abandonada",
            &json!({ "joinedAt": 1, "app": "navegador" }),
        )?;
        admin::remove_room_session(&db_m, "geral", "sessao-abandonada")
            .map_err(|e| format!("remove_room_session: {e}"))?;
        let data = seen_by_owner.snapshot();
        admin::delete_role(
            &db_o,
            &role,
            &data.member_roles,
            &data.category_roles,
            &data.channel_roles,
        )
        .map_err(|e| format!("delete_role: {e}"))?;
        let state = owner_view(&db_o)?;
        if !state["roleDefs"][&role].is_null()
            || !state["memberRoles"][&member.uid][&role].is_null()
        {
            return Err("excluir o cargo deixou sobras".into());
        }
        drop((seen_by_member, seen_by_owner));
        step("aprovar já no cargo, revogar, limpar sessão abandonada e excluir cargo sem sobras");
        Ok(())
    })();
    for uid in people {
        for path in [
            "community/approved",
            "community/requests",
            "community/roles",
            "community/memberRoles",
        ] {
            let _ = admin("DELETE", &format!("{path}/{uid}"), &Json::Null);
        }
    }
    for db in [db_o, db_m, db_x, db_n] {
        db.close();
    }
    result
}

fn checks(
    project: &Project,
    a: &Session,
    db_a: &Db,
    b: &Session,
    db_b: &Db,
) -> Result<(), AnyError> {
    if a.uid.is_empty()
        || a.uid == b.uid
        || a.email != format!("{}@nimbo.test", a.email.split('@').next().unwrap_or(""))
    {
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
    while status.recv_timeout(until.saturating_duration_since(Instant::now()))?
        != Status::Connected(true)
    {}
    step("conexão pronta");
    let drift = (db_a.server_now_ms()
        - SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64)
        .abs();
    if drift > 5_000 {
        return Err(format!("relógio do servidor fora: {drift} ms").into());
    }

    // Antes de aprovada, a conta lê só a própria aprovação.
    let approval = watch(db_a, &format!("community/approved/{}", a.uid));
    expect(&approval, "aprovação vazia", |e| {
        *e == Event::Value(Json::Null)
    })?;
    let members = watch(db_a, "rooms/geral/members");
    expect(&members, "sala recusada", |e| matches!(e, Event::Denied(_)))?;
    if db_a.get("community/profiles") != Err(DbError::Denied) {
        return Err("a lista de perfis deveria ser recusada".into());
    }
    step("leituras recusadas pelas regras chegam como recusa");

    db_a.set(
        &format!("community/requests/{}", a.uid),
        json!({ "email": a.email, "createdAt": db_a.server_now_ms() }),
    )?;
    if db_a.set(&format!("community/approved/{}", a.uid), json!(true)) != Err(DbError::Denied) {
        return Err("a conta não pode aprovar a si mesma".into());
    }
    step("gravação permitida passa e a proibida é recusada");

    // O proprietário aprova (por fora): a mudança chega sozinha e o que foi recusado volta a ser pedido.
    for uid in [&a.uid, &b.uid] {
        admin("PUT", &format!("community/approved/{uid}"), &json!(true))?;
    }
    expect(&approval, "aprovação", |e| {
        *e == Event::Value(json!(true))
    })?;
    db_a.retry_denied();
    expect(&members, "sala liberada", |e| matches!(e, Event::Value(_)))?;
    step("mudança feita por outra conexão chega; leitura recusada volta depois de liberada");

    // Presença: A entra na sala e pede ao servidor para tirá-la se a conexão cair.
    let me = format!("rooms/geral/members/{}", a.uid);
    db_a.on_disconnect_remove(&me)?;
    db_a.set(
        &me,
        json!({ "joinedAt": db_a.server_now_ms(), "app": "nimbo-core", "sid": "teste" }),
    )?;
    expect(
        &members,
        "a própria gravação de volta",
        |e| matches!(e, Event::Value(v) if v[&a.uid]["sid"] == "teste"),
    )?;
    let seen_by_b = watch(db_b, "rooms/geral/members");
    expect(
        &seen_by_b,
        "B vê A na sala",
        |e| matches!(e, Event::Value(v) if v[&a.uid]["app"] == "nimbo-core"),
    )?;
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
    expect(
        &seen_by_b,
        "A some da sala",
        |e| matches!(e, Event::Value(v) if v[&a.uid].is_null()),
    )?;
    let until = Instant::now() + WAIT;
    while status.recv_timeout(until.saturating_duration_since(Instant::now()))?
        != Status::Connected(true)
    {}
    db_b.set(
        &format!("rooms/geral/members/{}", b.uid),
        json!({ "joinedAt": db_b.server_now_ms() }),
    )?;
    expect(
        &members,
        "A, reconectada, vê B",
        |e| matches!(e, Event::Value(v) if v[&b.uid].is_object()),
    )?;
    step("queda: o servidor remove a presença, a conexão volta e as escutas continuam");

    db_b.remove(&format!("rooms/geral/members/{}", b.uid))?;

    // Sala: entrar aparece para os outros com a versão do app; a mesma conta entrando em outro lugar
    // tira a sessão antiga; sair remove a presença.
    let room = Room::join(db_a, &a.uid, "competitivo", "teste (nativo)", || {})?;
    let in_room = watch(db_b, "rooms/competitivo/members");
    expect(
        &in_room,
        "B vê A no canal, com a versão do app",
        |e| matches!(e, Event::Value(v) if v[&a.uid]["app"] == "teste (nativo)" && v[&a.uid]["joinedAt"].is_number()),
    )?;
    if !room.snapshot().joined || room.snapshot().issue.is_some() {
        return Err("a sala deveria estar aberta e sem problema".into());
    }
    let (_, other) = account(project, a.email.split('@').next().unwrap_or(""))?;
    let second = Room::join(&other, &a.uid, "competitivo", "outra sessão", || {})?;
    let until = Instant::now() + WAIT;
    while !room.snapshot().issue.is_some_and(|issue| issue.fatal) {
        if Instant::now() > until {
            return Err("a sessão antiga não percebeu a nova".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    room.leave();
    expect(
        &in_room,
        "a presença continua sendo da sessão nova",
        |e| matches!(e, Event::Value(v) if v[&a.uid]["app"] == "outra sessão"),
    )?;
    second.leave();
    expect(
        &in_room,
        "A some do canal ao sair",
        |e| matches!(e, Event::Value(v) if v[&a.uid].is_null()),
    )?;
    other.close();
    step("sala: entrar, ser trocada por outra sessão da mesma conta e sair");

    // Sinalização de uma live nativa, dos dois lados: B anuncia a live, A manda a oferta, B responde
    // com o mesmo código, e cada mensagem é entregue uma vez e apagada do banco.
    let room_a = Room::join(db_a, &a.uid, "competitivo", "teste (nativo)", || {})?;
    let room_b = Room::join(db_b, &b.uid, "competitivo", "teste (nativo)", || {})?;
    room_b.publish_stream("screen", "tela-de-b", true, "")?;
    let live_of_b = || {
        room_a
            .snapshot()
            .broadcasts
            .get(&b.uid)
            .and_then(|kinds| kinds.get("screen"))
            .cloned()
    };
    wait_until("A vê a live nativa de B", || {
        live_of_b().is_some_and(|live| live.native && live.sound && live.stream_id == "tela-de-b")
    })?;
    let announced = db_a.get(&format!("rooms/competitivo/streams/{}/screen", b.uid))?;
    if !announced["startedAt"].is_number() || announced["native"] != true {
        return Err(format!("anúncio da live fora do formato do site: {announced}").into());
    }
    let (tx, sent) = channel();
    room_a.send_native_offer(
        &b.uid,
        "screen",
        "v=0 oferta de teste",
        "codigo-1",
        move |result| drop(tx.send(result)),
    );
    sent.recv_timeout(WAIT)??;
    let mut got = Vec::new();
    wait_until("a oferta chega na caixa de B", || {
        got = room_b.take_signals();
        !got.is_empty()
    })?;
    let offer = Signal::Native {
        from: a.uid.clone(),
        kind: "screen".into(),
        answer: false,
        sdp: "v=0 oferta de teste".into(),
        nonce: "codigo-1".into(),
    };
    if got != vec![offer] {
        return Err(format!("sinal inesperado: {got:?}").into());
    }
    wait_until("a mensagem entregue é apagada", || {
        db_b.get(&format!("rooms/competitivo/signals/{}", b.uid))
            .is_ok_and(|inbox| inbox.is_null())
    })?;
    if !room_b.take_signals().is_empty() {
        return Err("a mesma mensagem foi entregue duas vezes".into());
    }
    let (tx, sent) = channel();
    room_b.send_native_answer(
        &a.uid,
        "screen",
        "v=0 resposta de teste",
        "codigo-1",
        move |result| drop(tx.send(result)),
    );
    sent.recv_timeout(WAIT)??;
    wait_until("a resposta chega na caixa de A", || {
        got = room_a.take_signals();
        !got.is_empty()
    })?;
    let answer = Signal::Native {
        from: b.uid.clone(),
        kind: "screen".into(),
        answer: true,
        sdp: "v=0 resposta de teste".into(),
        nonce: "codigo-1".into(),
    };
    if got != vec![answer] {
        return Err(format!("resposta inesperada: {got:?}").into());
    }
    step("sinalização: live anunciada, oferta e resposta entregues uma vez e apagadas");

    // Plateia e prévia, no formato do site: A avisa o que assiste (a própria live não conta), B vê;
    // a prévia de B chega a A por leitura única e pela escuta dos cartões.
    let key_b = format!("{}:screen", b.uid);
    room_a.set_watching(&[key_b.clone(), format!("{}:screen", a.uid)]);
    wait_until("B vê A na plateia", || {
        room_b
            .snapshot()
            .watchers
            .get(&key_b)
            .is_some_and(|who| *who == vec![a.uid.clone()])
    })?;
    let raw = db_b.get(&format!("rooms/competitivo/watching/{}", a.uid))?;
    if raw != json!({ b.uid.clone(): { "screen": true } }) {
        return Err(format!("plateia fora do formato do site: {raw}").into());
    }
    let preview = "data:image/jpeg;base64,/9j/4AAQSkZJRgABAQ==";
    room_b.publish_preview("screen", preview)?;
    if nimbo_core::room::load_previews(db_a, "competitivo", &b.uid)
        .get("screen")
        .map(String::as_str)
        != Some(preview)
    {
        return Err("a prévia de B não chegou a A".into());
    }
    if room_b
        .publish_preview("screen", "data:image/svg+xml;base64,PHN2Zz4=")
        .is_ok()
    {
        return Err("prévia em formato não aceito foi gravada".into());
    }
    room_a.watch_previews(true);
    wait_until("os cartões de A recebem a prévia", || {
        room_a
            .snapshot()
            .previews
            .get(&key_b)
            .is_some_and(|data| data == preview)
    })?;
    room_a.watch_previews(false);
    wait_until("sem cartões, a prévia sai da memória", || {
        room_a.snapshot().previews.is_empty()
    })?;
    room_a.set_watching(&[]);
    wait_until("A sai da plateia", || {
        !room_b.snapshot().watchers.contains_key(&key_b)
    })?;
    step("plateia e prévia: gravadas no formato do site, lidas só quando pedidas");

    // Queda de quem transmite: o servidor tira a live, e ela é reanunciada quando a conexão volta.
    db_b.debug_drop_connection();
    wait_until("a live de B some na queda", || live_of_b().is_none())?;
    wait_until("a live de B volta, reanunciada", || {
        live_of_b().is_some_and(|live| live.stream_id == "tela-de-b")
    })?;
    wait_until("B conta a reconexão", || room_b.snapshot().reconnects == 1)?;
    room_b.unpublish_stream("screen");
    wait_until("a live de B some ao ser retirada", || live_of_b().is_none())?;
    let game = "Jogo de teste com um nome comprido demais para caber no limite do site";
    room_b.publish_stream("screen", "tela-de-b-2", false, game)?;
    wait_until("a live nova de B aparece, sem som", || {
        live_of_b().is_some_and(|live| live.stream_id == "tela-de-b-2" && !live.sound)
    })?;
    let live = live_of_b().ok_or("live de B sumiu")?;
    let late = (db_a.server_now_ms() - live.started_at).abs();
    if live.game.chars().count() != 48 || !game.starts_with(&live.game) || late > 5_000 {
        return Err(format!(
            "jogo ou início fora do esperado: {:?}, {late} ms",
            live.game
        )
        .into());
    }
    room_b.leave();
    wait_until("sair do canal tira a live", || live_of_b().is_none())?;
    room_a.leave();
    step("live própria: reanunciada depois da queda, retirada a pedido e ao sair do canal");

    db_a.close();
    db_b.close();
    Ok(())
}
