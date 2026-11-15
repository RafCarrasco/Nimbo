//! Nimbo nativo: a interface desenhada em Rust (egui), sem navegador embutido.
//!
//! Em construção, numa branch à parte. O app oficial continua sendo `desktop/rust-app` (o site
//! dentro do WebView2); este aqui só substitui o outro quando fizer tudo o que ele faz.
//! Ver `docs/rust-app.md`, seção "App full Rust".
//!
//! Uso nesta etapa (só com os emuladores, `npm run emulators`):
//!   nimbo-native --dev-user owner [--join <id do canal>] [--screenshot foto.png]

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod sidebar;
mod theme;
mod widgets;

fn main() -> eframe::Result {
    let mut args = app::Args { dev_user: None, screenshot: None, join: None };
    let mut list = std::env::args().skip(1);
    while let Some(arg) = list.next() {
        match arg.as_str() {
            "--dev-user" => args.dev_user = list.next(),
            "--screenshot" => args.screenshot = list.next().map(Into::into),
            "--join" => args.join = list.next(),
            _ => {}
        }
    }
    let icon = image::load_from_memory(include_bytes!("../../../public/nimbo-icon-64.png"))
        .map(|img| img.to_rgba8())
        .map(|img| egui::IconData { width: img.width(), height: img.height(), rgba: img.into_raw() })
        .unwrap_or_default();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Nimbo")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([760.0, 520.0])
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native("Nimbo", options, Box::new(|cc| Ok(Box::new(app::Nimbo::new(cc, args)))))
}
