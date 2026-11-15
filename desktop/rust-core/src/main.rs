//! `nimbo-core`: ferramenta de teste do miolo, sem interface. Só fala com os emuladores locais.
//!
//! - `nimbo-core watch <conta> <caminho>...`: entra como a conta de teste (a mesma de `?devUser=`
//!   no site) e imprime cada mudança dos caminhos, uma linha de JSON por vez.
//! - `nimbo-core sidebar <conta> [segundos]`: imprime a barra lateral que essa conta enxerga (categorias,
//!   canais e quem está em cada um), para comparar com o site.
//! - `nimbo-core selftest`: confere o protocolo contra o emulador (ver `selftest.rs`).

mod selftest;

use std::time::Duration;

use nimbo_core::auth;
use nimbo_core::config::Project;
use nimbo_core::rtdb::{Db, Event};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("watch") if args.len() >= 3 => watch(&args[1], &args[2..]),
        Some("sidebar") if args.len() >= 2 => sidebar(&args[1], args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3)),
        Some("selftest") => selftest::run(),
        _ => Err("uso: nimbo-core watch <conta> <caminho>... | nimbo-core sidebar <conta> [segundos] | nimbo-core selftest".into()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn watch(name: &str, paths: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let project = Project::emulator();
    let session = auth::sign_in_google(&project, &auth::dev_credential(name), "")?;
    println!("{}", serde_json::json!({ "conta": session.email, "uid": session.uid }));
    let db = Db::connect(&project);
    db.set_token(Some(session.id_token));
    for path in paths {
        let label = path.clone();
        db.listen(path, move |event| match event {
            Event::Value(value) => println!("{}", serde_json::json!({ "caminho": label, "valor": value })),
            Event::Denied(reason) => println!("{}", serde_json::json!({ "caminho": label, "recusado": reason })),
        });
    }
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// A barra lateral em texto, com as mesmas contas do site (Sidebar.tsx): o que aparece, em que ordem.
fn sidebar(name: &str, seconds: u64) -> Result<(), Box<dyn std::error::Error>> {
    let project = Project::emulator();
    let session = auth::sign_in_google(&project, &auth::dev_credential(name), "")?;
    let db = Db::connect(&project);
    db.set_token(Some(session.id_token));
    let community = nimbo_core::community::Community::start(&db, &session.uid, || {});
    std::thread::sleep(Duration::from_secs(seconds));
    let data = community.snapshot();
    println!("{} · {}{}", session.email, data.level.label(), if data.loaded { "" } else { " · ainda carregando" });
    let uid = session.uid.as_str();
    let staff = data.level.is_staff();
    for (category_id, category) in data.ordered_categories() {
        let channels: Vec<_> = data
            .ordered_channels()
            .into_iter()
            .filter(|(id, c)| c.enabled && c.category_id == *category_id && (data.owner || data.channel_allowed(id, uid)))
            .collect();
        if !data.owner && (!data.category_allowed(category_id, uid) || (!staff && channels.is_empty())) {
            continue;
        }
        let lock = data.restricted_label(data.category_access.get(category_id).copied(), data.category_roles.get(category_id));
        println!("{}{}", category.name.to_uppercase(), if lock.is_empty() { String::new() } else { format!(" [{lock}]") });
        for (id, channel) in channels {
            let allowed = data.channel_allowed(id, uid);
            let lock = data.restricted_label(data.channel_access.get(id).copied(), data.channel_roles.get(id));
            println!("  {}{}{}", channel.name, if allowed { "" } else { " (trancado)" }, if lock.is_empty() { String::new() } else { format!(" [{lock}]") });
            let Some(here) = data.presence.get(id).filter(|_| allowed) else { continue };
            for member in &here.members {
                let known = data.profiles.get(member);
                if member != uid && known.is_none() && !(staff && !data.resolving.contains(member)) {
                    continue;
                }
                let live = here.streams.get(member).copied().unwrap_or_default();
                println!(
                    "    {}{}{}{}",
                    known.map_or("Sessão sem perfil", String::as_str),
                    if member == uid { " (você)" } else { "" },
                    if live.screen || live.camera { " AO VIVO" } else { "" },
                    data.role_color(member).map(|c| format!(" {c}")).unwrap_or_default(),
                );
            }
        }
    }
    Ok(())
}
