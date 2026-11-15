//! `nimbo-core`: ferramenta de teste do miolo, sem interface. Só fala com os emuladores locais.
//!
//! - `nimbo-core watch <conta> <caminho>...`: entra como a conta de teste (a mesma de `?devUser=`
//!   no site) e imprime cada mudança dos caminhos, uma linha de JSON por vez.
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
        Some("selftest") => selftest::run(),
        _ => Err("uso: nimbo-core watch <conta> <caminho>... | nimbo-core selftest".into()),
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
