//! Estado do app e as telas: entrando, aguardando aprovação e a tela principal.
//!
//! Etapa atual: só leitura. O app entra com uma conta, espera a aprovação e mostra a comunidade
//! ao vivo (canais, quem está em cada um, lives). Entrar em canal, assistir e transmitir vêm nas
//! etapas seguintes (ver `docs/rust-app.md`).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, Rect, Sense, TextureHandle, Ui, pos2, vec2};
use lucide_icons::Icon;
use nimbo_core::auth::{self, Session};
use nimbo_core::community::Community;
use nimbo_core::config::Project;
use nimbo_core::rtdb::{Db, Event, Status};
use serde_json::json;

use crate::sidebar::{Action, Sidebar};
use crate::theme;
use crate::widgets::icon;

pub struct Args {
    /// Conta de teste do emulador (a mesma de `?devUser=` no site).
    pub dev_user: Option<String>,
    /// Salva uma foto da janela neste arquivo depois de carregar, e fecha (para conferir a tela).
    pub screenshot: Option<PathBuf>,
}

enum Msg {
    SignedIn(Result<Session, String>),
    /// Token renovado da mesma conta.
    Renewed(Session),
    Approved(bool),
    Online(bool),
}

/// Conta dentro do app: a conexão, a comunidade (depois de aprovada) e quem renova o token.
struct Account {
    session: Session,
    db: Db,
    approved: Option<bool>,
    requested: bool,
    online: bool,
    community: Option<Community>,
    stop: Arc<AtomicBool>,
}

impl Drop for Account {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.community = None;
        self.db.close();
    }
}

pub struct Nimbo {
    args: Args,
    project: Project,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    account: Option<Account>,
    signing_in: bool,
    error: String,
    logo: TextureHandle,
    collapsed: BTreeSet<String>,
    room: String,
    started: Instant,
    shot_requested: bool,
    shot_done: Arc<AtomicBool>,
}

impl Nimbo {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Args) -> Self {
        theme::install(&cc.egui_ctx);
        let logo = image::load_from_memory(include_bytes!("../../../public/nimbo-icon-256.png"))
            .map(|img| img.to_rgba8())
            .map(|img| egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()))
            .unwrap_or_else(|_| egui::ColorImage::filled([1, 1], Color32::TRANSPARENT));
        let logo = cc.egui_ctx.load_texture("nimbo", logo, egui::TextureOptions::LINEAR);
        let (tx, rx) = channel();
        let mut app = Self {
            args,
            // Enquanto o login de verdade não entra nesta versão, o app nativo só fala com os emuladores.
            project: Project::emulator(),
            tx,
            rx,
            account: None,
            signing_in: false,
            error: String::new(),
            logo,
            collapsed: BTreeSet::new(),
            room: String::new(),
            started: Instant::now(),
            shot_requested: false,
            shot_done: Arc::new(AtomicBool::new(false)),
        };
        if let Some(name) = app.args.dev_user.clone() {
            app.sign_in(&cc.egui_ctx, &name);
        }
        app
    }

    fn sign_in(&mut self, ctx: &egui::Context, dev_user: &str) {
        self.signing_in = true;
        self.error.clear();
        let (project, tx, ctx, credential) = (self.project.clone(), self.tx.clone(), ctx.clone(), auth::dev_credential(dev_user));
        std::thread::spawn(move || {
            let result = auth::sign_in_google(&project, &credential, "").map_err(|e| e.to_string());
            let _ = tx.send(Msg::SignedIn(result));
            ctx.request_repaint();
        });
    }

    fn signed_in(&mut self, ctx: &egui::Context, session: Session) {
        let db = Db::connect(&self.project);
        db.set_token(Some(session.id_token.clone()));
        let (tx, repaint) = (self.tx.clone(), ctx.clone());
        db.on_status(move |status| {
            if let Status::Connected(online) = status {
                let _ = tx.send(Msg::Online(*online));
                repaint.request_repaint();
            }
        });
        let (tx, repaint) = (self.tx.clone(), ctx.clone());
        db.listen(&format!("community/approved/{}", session.uid), move |event| {
            let _ = tx.send(Msg::Approved(matches!(event, Event::Value(value) if value.as_bool() == Some(true))));
            repaint.request_repaint();
        });
        // Renova o token antes de vencer (ele dura uma hora) e entrega o novo à conexão.
        let stop = Arc::new(AtomicBool::new(false));
        let (project, tx, repaint, renew_db, renew_stop, mut current) =
            (self.project.clone(), self.tx.clone(), ctx.clone(), db.clone(), stop.clone(), session.clone());
        std::thread::spawn(move || {
            while !renew_stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_secs(5));
                if !current.needs_refresh() {
                    continue;
                }
                if let Ok(renewed) = auth::refresh(&project, &current) {
                    current = renewed.clone();
                    renew_db.set_token(Some(renewed.id_token.clone()));
                    let _ = tx.send(Msg::Renewed(renewed));
                    repaint.request_repaint();
                }
            }
        });
        self.account =
            Some(Account { session, db, approved: None, requested: false, online: false, community: None, stop });
    }

    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::SignedIn(Ok(session)) => {
                    self.signing_in = false;
                    self.signed_in(ctx, session);
                }
                Msg::SignedIn(Err(error)) => {
                    self.signing_in = false;
                    self.error = error;
                }
                Msg::Renewed(session) => {
                    if let Some(account) = &mut self.account {
                        account.session = session;
                    }
                }
                Msg::Online(online) => {
                    if let Some(account) = &mut self.account {
                        account.online = online;
                    }
                }
                Msg::Approved(approved) => {
                    let Some(account) = &mut self.account else { continue };
                    account.approved = Some(approved);
                    if approved {
                        if account.community.is_none() {
                            let repaint = ctx.clone();
                            account.community =
                                Some(Community::start(&account.db, &account.session.uid, move || repaint.request_repaint()));
                        }
                    } else {
                        account.community = None;
                        // Como no site: registra o pedido de acesso uma vez, para o proprietário aprovar.
                        if !account.requested && !account.session.email.is_empty() {
                            account.requested = true;
                            let request = json!({ "email": account.session.email, "createdAt": account.db.server_now_ms() });
                            account.db.set_then(&format!("community/requests/{}", account.session.uid), request, |_| {});
                        }
                    }
                }
            }
        }
    }

    fn act(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::ToggleCategory(id) => {
                    if !self.collapsed.remove(&id) {
                        self.collapsed.insert(id);
                    }
                }
                Action::SignOut => {
                    self.account = None;
                    self.room.clear();
                }
                // Entrar em canal, assistir, perfil e administração chegam nas próximas etapas.
                Action::Join(_) | Action::Watch(..) | Action::OpenAdmin | Action::EditProfile => {}
            }
        }
    }

    /// Fundo e cartão central das telas de entrada, como `.gate` no site.
    fn gate(&self, ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui)) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG)).show(ui, |ui| {
            let area = ui.max_rect();
            let card = Rect::from_center_size(area.center(), vec2(400.0_f32.min(area.width() - 32.0), height));
            let painter = ui.painter().clone();
            painter.rect_filled(card, 18.0, Color32::from_rgb(0x13, 0x1a, 0x2e));
            painter.rect_stroke(card, 18.0, (1.0, theme::BORDER), egui::StrokeKind::Inside);
            let mascot = Rect::from_center_size(pos2(card.center().x, card.top() + 36.0 + 42.0), vec2(84.0, 84.0));
            painter.image(self.logo.id(), mascot, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            let inner = Rect::from_min_max(pos2(card.left() + 30.0, mascot.bottom() + 14.0), pos2(card.right() - 30.0, card.bottom() - 30.0));
            let mut content = ui.new_child(
                egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Center)),
            );
            add(&mut content);
        });
    }

    fn home(&self, ui: &mut Ui, nickname: &str) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG)).show(ui, |ui| {
            let area = ui.max_rect();
            let painter = ui.painter().clone();
            let title = painter.layout_no_wrap(format!("Olá, {nickname}"), theme::bold(28.0), theme::TEXT);
            let hint = painter.layout_no_wrap(
                "Escolha um canal na barra lateral para assistir às lives ou transmitir.".to_owned(),
                theme::regular(14.0),
                theme::MUTED,
            );
            let height = 110.0 + 10.0 + title.size().y + 10.0 + hint.size().y;
            let mut y = area.center().y - height / 2.0;
            let logo = Rect::from_min_size(pos2(area.center().x - 55.0, y), vec2(110.0, 110.0));
            painter.image(self.logo.id(), logo, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE.gamma_multiply(0.95));
            y += 120.0;
            painter.galley(pos2(area.center().x - title.size().x / 2.0, y), title.clone(), theme::TEXT);
            y += title.size().y + 10.0;
            painter.galley(pos2(area.center().x - hint.size().x / 2.0, y), hint, theme::MUTED);
        });
    }

    /// `--screenshot`: com a tela carregada e parada, salva a foto e fecha.
    fn screenshot(&mut self, ui: &Ui, ready: bool) {
        let Some(path) = self.args.screenshot.clone() else { return };
        let ctx = ui.ctx().clone();
        if self.shot_done.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let waited = self.started.elapsed();
        if !self.shot_requested && ((ready && waited > Duration::from_secs(3)) || waited > Duration::from_secs(12)) {
            self.shot_requested = true;
            let (done, repaint) = (self.shot_done.clone(), ctx.clone());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |shot| {
                let [width, height] = shot.size;
                let pixels: Vec<u8> = shot.pixels.iter().flat_map(|pixel| pixel.to_array()).collect();
                let result = image::save_buffer(&path, &pixels, width as u32, height as u32, image::ColorType::Rgba8);
                println!("{}", json!({ "foto": path, "largura": width, "altura": height, "ok": result.is_ok() }));
                done.store(true, Ordering::Relaxed);
                repaint.request_repaint();
            })));
        }
        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

impl eframe::App for Nimbo {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain(&ctx);
        let mut actions = Vec::new();
        let mut ready = false;
        match &self.account {
            None => {
                let (signing_in, error) = (self.signing_in, self.error.clone());
                self.gate(ui, if signing_in { 230.0 } else { 330.0 }, |ui| {
                    if signing_in {
                        ui.add_space(20.0);
                        ui.add(egui::Spinner::new().size(28.0).color(theme::MUTED));
                        return;
                    }
                    wordmark(ui);
                    ui.add_space(14.0);
                    ui.label(egui::RichText::new("Suas lives. Sua comunidade.").color(theme::MUTED));
                    ui.add_space(20.0);
                    let note = if error.is_empty() {
                        "Versão de teste do app nativo: abra com --dev-user <conta> e os emuladores ligados.".to_owned()
                    } else {
                        error
                    };
                    ui.label(egui::RichText::new(note).font(theme::regular(12.0)).color(theme::MUTED));
                });
            }
            Some(account) if account.approved != Some(true) => {
                let (waiting, email) = (account.approved.is_none(), account.session.email.clone());
                let mut leave = false;
                self.gate(ui, if waiting { 230.0 } else { 384.0 }, |ui| {
                    if waiting {
                        ui.add_space(20.0);
                        ui.add(egui::Spinner::new().size(28.0).color(theme::MUTED));
                        return;
                    }
                    ui.label(egui::RichText::new("Quase lá").font(theme::bold(24.0)));
                    ui.add_space(14.0);
                    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
                    let title = ui.painter().layout_no_wrap("Aguardando aprovação".to_owned(), theme::bold(14.0), theme::TEXT);
                    let x = row.center().x - (20.0 + 8.0 + title.size().x) / 2.0;
                    icon(ui.painter(), pos2(x + 10.0, row.center().y), Icon::Clock, 20.0, theme::PRIMARY);
                    ui.painter().galley(pos2(x + 28.0, row.center().y - title.size().y / 2.0), title, theme::TEXT);
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(
                            "Sua solicitação foi enviada. Esta tela atualiza sozinha quando um administrador liberar o acesso.",
                        )
                        .color(theme::MUTED),
                    );
                    ui.add_space(14.0);
                    ui.label(egui::RichText::new(email).font(theme::regular(12.0)).color(theme::MUTED));
                    ui.add_space(14.0);
                    leave = ghost_button(ui, "Usar outra conta");
                });
                if leave {
                    actions.push(Action::SignOut);
                }
            }
            Some(account) => {
                let data = account.community.as_ref().map(Community::snapshot).unwrap_or_default();
                ready = data.loaded && account.online;
                if data.loaded {
                    Sidebar {
                        data: &data,
                        collapsed: &self.collapsed,
                        room: &self.room,
                        logo: &self.logo,
                        version: env!("CARGO_PKG_VERSION"),
                    }
                    .show(ui, &mut actions);
                    let nickname = data.profiles.get(&data.uid).cloned().unwrap_or_default();
                    self.home(ui, &nickname);
                } else {
                    self.gate(ui, 230.0, |ui| {
                        ui.add_space(20.0);
                        ui.add(egui::Spinner::new().size(28.0).color(theme::MUTED));
                    });
                }
            }
        }
        self.act(actions);
        self.screenshot(ui, ready);
    }
}

/// "Nimbo" com o "bo" na cor principal, como o título da tela de entrada do site.
fn wordmark(ui: &mut Ui) {
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::hover());
    let painter = ui.painter();
    let first = painter.layout_no_wrap("Nim".to_owned(), theme::bold(40.0), theme::TEXT);
    let second = painter.layout_no_wrap("bo".to_owned(), theme::bold(40.0), theme::PRIMARY);
    let x = row.center().x - (first.size().x + second.size().x) / 2.0;
    let y = row.center().y - first.size().y / 2.0;
    let split = x + first.size().x;
    painter.galley(pos2(x, y), first, theme::TEXT);
    painter.galley(pos2(split, y), second, theme::PRIMARY);
}

/// Botão discreto (`.button.ghost` no site).
fn ghost_button(ui: &mut Ui, label: &str) -> bool {
    let text = ui.painter().layout_no_wrap(label.to_owned(), theme::semibold(14.0), theme::TEXT);
    let (rect, response) = ui.allocate_exact_size(vec2(text.size().x + 32.0, 38.0), Sense::click());
    let fill = if response.hovered() { theme::SURFACE_3 } else { theme::SURFACE_2 };
    ui.painter().rect_filled(rect, 8.0, fill);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, theme::semibold(14.0), theme::TEXT);
    response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}
