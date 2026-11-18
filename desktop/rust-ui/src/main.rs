//! Nimbo nativo: a interface desenhada em Rust (egui), sem navegador embutido.
//!
//! Em construção, numa branch à parte. O app oficial continua sendo `desktop/rust-app` (o site
//! dentro do WebView2); este aqui só substitui o outro quando fizer tudo o que ele faz.
//! Ver `docs/rust-app.md`, seção "App full Rust".
//!
//! Uso nesta etapa:
//!   nimbo-native --dev-user owner [--join <id do canal>] [--screenshot foto.png]
//!     conta de teste dos emuladores (`npm run emulators`), sem passar pelo navegador
//!   nimbo-native
//!     emuladores, com o login pelo navegador (`npm run dev:local` precisa estar no ar)
//!   nimbo-native --production
//!     Firebase de verdade, com a conta Google de quem usa. Ainda em validação.
//!
//! Para testar lives sem clicar (sempre com `--mute`: nenhum som audível em teste):
//!   nimbo-native --dev-user a --join geral --broadcast <janela> [--sound] [--stats] [--exit-after 60]
//!     transmite a janela (o identificador dela no Windows) assim que entra no canal
//!   nimbo-native --dev-user b --join geral --watch-all --mute [--stats] [--shot-after 30 --screenshot foto.png]
//!     abre no palco toda live do canal; `--cpu-video` troca o desenho pela conversão na CPU

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod admin;
mod app;
mod dialogs;
mod images;
mod live;
// O login pelo navegador é o do app atual, sem cópia: mesma porta local, mesmo código de uso único.
#[allow(dead_code)]
#[path = "../../rust-app/src/login.rs"]
mod login;
mod menu;
mod ops;
mod picker;
mod script;
mod session_store;
mod settings;
mod sidebar;
mod stage;
mod theme;
mod video;
mod widgets;

fn main() -> eframe::Result {
    let mut args = app::Args::default();
    let (mut position, mut size) = (None, [1280.0, 800.0]);
    let pair = |text: &str| -> Option<[f32; 2]> {
        let (a, b) = text.split_once(',')?;
        Some([a.trim().parse().ok()?, b.trim().parse().ok()?])
    };
    let mut list = std::env::args().skip(1);
    while let Some(arg) = list.next() {
        match arg.as_str() {
            "--dev-user" => args.dev_user = list.next(),
            "--screenshot" => args.screenshot = list.next().map(Into::into),
            "--join" => args.join = list.next(),
            "--production" => args.production = true,
            "--login" => args.login = true,
            "--broadcast" => args.broadcast = list.next().and_then(|window| window.parse().ok()),
            "--sound" => args.sound = true,
            "--mute" => args.mute = true,
            "--watch-all" => args.watch_all = true,
            "--stats" => args.stats = true,
            "--exit-after" => {
                args.exit_after = list.next().and_then(|seconds| seconds.parse().ok())
            }
            "--shot-after" => {
                args.shot_after = list.next().and_then(|seconds| seconds.parse().ok())
            }
            "--cpu-video" => args.cpu_video = true,
            "--pick" => args.pick = true,
            // Telas abertas sem clicar, para as fotos de teste (ver `Args` em app.rs).
            "--open-admin" => args.open_admin = list.next(),
            "--open-role" => args.open_role = list.next(),
            "--role-tab" => args.role_tab = list.next(),
            "--open-settings" => args.open_settings = list.next(),
            "--open-menu" => args.open_menu = list.next(),
            "--no-auto-open" => args.no_auto_open = true,
            "--clicks" => args.clicks = list.next(),
            "--fullscreen-in-window" => args.windowed_fullscreen = true,
            "--site" => args.site = list.next(),
            // Onde abrir a janela ("x,y") e de que tamanho ("largura,altura"): os testes abrem fora da
            // tela, para não aparecer para quem está usando o computador.
            "--position" => position = list.next().and_then(|text| pair(&text)),
            "--size" => size = list.next().and_then(|text| pair(&text)).unwrap_or(size),
            _ => {}
        }
    }
    let icon = image::load_from_memory(include_bytes!("../../../public/nimbo-icon-64.png"))
        .map(|img| img.to_rgba8())
        .map(|img| egui::IconData {
            width: img.width(),
            height: img.height(),
            rgba: img.into_raw(),
        })
        .unwrap_or_default();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Nimbo")
        .with_inner_size(size)
        .with_min_inner_size([760.0, 520.0])
        .with_icon(icon);
    if let Some(at) = position {
        viewport = viewport.with_position(at).with_active(false);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Nimbo",
        options,
        Box::new(|cc| Ok(Box::new(app::Nimbo::new(cc, args)))),
    )
}
