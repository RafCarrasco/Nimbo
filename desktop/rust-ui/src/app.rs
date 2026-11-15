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
use nimbo_core::community::{Community, Snapshot};
use nimbo_core::config::Project;
use nimbo_core::room::{Room, RoomSnapshot};
use nimbo_core::rtdb::{Db, Event, Status};
use serde_json::json;

use crate::sidebar::{Action, Connection, Sidebar};
use crate::theme;
use crate::widgets::icon;

/// Como este app se apresenta aos outros no canal e no diagnóstico (o site mostra "0.5.5 (Rust)").
const APP_LABEL: &str = concat!(env!("CARGO_PKG_VERSION"), " (nativo)");

pub struct Args {
    /// Conta de teste do emulador (a mesma de `?devUser=` no site).
    pub dev_user: Option<String>,
    /// Salva uma foto da janela neste arquivo depois de carregar, e fecha (para conferir a tela).
    pub screenshot: Option<PathBuf>,
    /// Entra neste canal assim que a comunidade carregar (para testar sem clicar).
    pub join: Option<String>,
}

enum Msg {
    SignedIn(Result<Session, String>),
    /// Token renovado da mesma conta.
    Renewed(Session),
    Approved(bool),
    Online(bool),
    /// Resultado de entrar num canal (o id do canal e a sala, ou o motivo de não ter dado).
    Joined(String, Result<Room, String>),
}

/// Conta dentro do app: a conexão, a comunidade (depois de aprovada) e quem renova o token.
struct Account {
    session: Session,
    db: Db,
    approved: Option<bool>,
    requested: bool,
    online: bool,
    community: Option<Community>,
    room: Option<Arc<Room>>,
    /// Canal em que o app está entrando agora.
    joining: Option<String>,
    stop: Arc<AtomicBool>,
}

impl Drop for Account {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.community = None;
        // Fechar a conexão já tira a presença do canal: o servidor cumpre o pedido feito ao entrar.
        self.room = None;
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
    /// Aviso para a pessoa (não deu para entrar, saiu do canal...).
    notice: String,
    started: Instant,
    shot_requested: bool,
    auto_joined: bool,
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
            notice: String::new(),
            started: Instant::now(),
            shot_requested: false,
            auto_joined: false,
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
            Some(Account {
            session,
            db,
            approved: None,
            requested: false,
            online: false,
            community: None,
            room: None,
            joining: None,
            stop,
        });
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
                Msg::Joined(channel, result) => {
                    let Some(account) = &mut self.account else { continue };
                    if account.joining.as_deref() != Some(channel.as_str()) {
                        // A pessoa já pediu outra coisa: esta entrada não vale mais.
                        if let Ok(room) = result {
                            std::thread::spawn(move || room.leave());
                        }
                        continue;
                    }
                    account.joining = None;
                    match result {
                        Ok(room) => {
                            account.room = Some(Arc::new(room));
                            self.notice.clear();
                        }
                        Err(error) => self.notice = format!("Não deu para entrar no canal: {error}"),
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
                        account.room = None;
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

    /// Entra num canal (saindo do atual, se houver). A conversa com o servidor fica fora da thread
    /// que desenha; o resultado volta como Msg::Joined.
    fn join(&mut self, ctx: &egui::Context, channel: String) {
        let Some(account) = &mut self.account else { return };
        if account.joining.is_some() || account.room.as_ref().is_some_and(|room| room.id() == channel) {
            return;
        }
        account.joining = Some(channel.clone());
        let previous = account.room.take();
        let (db, uid, tx, repaint) = (account.db.clone(), account.session.uid.clone(), self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            if let Some(room) = previous {
                room.leave();
            }
            let notify = repaint.clone();
            let result = Room::join(&db, &uid, &channel, APP_LABEL, move || notify.request_repaint());
            let _ = tx.send(Msg::Joined(channel, result.map_err(|e| e.to_string())));
            repaint.request_repaint();
        });
    }

    fn leave(&mut self) {
        let Some(account) = &mut self.account else { return };
        account.joining = None;
        if let Some(room) = account.room.take() {
            std::thread::spawn(move || room.leave());
        }
    }

    fn act(&mut self, ctx: &egui::Context, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::ToggleCategory(id) => {
                    if !self.collapsed.remove(&id) {
                        self.collapsed.insert(id);
                    }
                }
                Action::SignOut => self.account = None,
                Action::Join(channel) | Action::Watch(channel, _) => self.join(ctx, channel),
                Action::Leave => self.leave(),
                // Assistir, perfil e administração chegam nas próximas etapas.
                Action::OpenAdmin | Action::EditProfile => {}
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
            if !self.notice.is_empty() {
                self.notice_bar(ui, area);
            }
        });
    }

    /// Palco do canal em que a pessoa está. Nesta etapa só o cabeçalho e o aviso de palco vazio:
    /// assistir e transmitir entram com o player e o motor.
    fn stage(&self, ui: &mut Ui, data: &Snapshot, room: &RoomSnapshot, channel_name: &str) {
        let frame = egui::Frame::new().fill(theme::BG);
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            let area = ui.max_rect();
            let painter = ui.painter().clone();
            let head = Rect::from_min_size(area.min, vec2(area.width(), 56.0));
            painter.hline(head.x_range(), head.bottom() - 0.5, (1.0, theme::BORDER));
            let title = painter.layout_no_wrap(channel_name.to_owned(), theme::bold(16.0), theme::TEXT);
            let mut x = head.left() + 16.0;
            let title_width = title.size().x;
            painter.galley(pos2(x, head.center().y - title.size().y / 2.0), title, theme::TEXT);
            x += title_width + 10.0;
            icon(&painter, pos2(x + 7.0, head.center().y), Icon::Users, 14.0, theme::MUTED);
            let count = painter.layout_no_wrap(room.members.len().to_string(), theme::regular(13.0), theme::MUTED);
            let count_width = count.size().x;
            painter.galley(pos2(x + 18.0, head.center().y - count.size().y / 2.0), count, theme::MUTED);
            x += 18.0 + count_width + 14.0;
            // Quem está ao vivo no canal (as etiquetas do topo do site).
            let lives = data.presence.get(&room.room).map(|here| &here.streams);
            let mut any_live = false;
            for (member, live) in lives.into_iter().flatten().filter(|(member, _)| room.members.contains(member)) {
                any_live = true;
                let name = if *member == data.uid { "Você" } else { data.profiles.get(member).map_or("Participante", String::as_str) };
                let label = painter.layout_no_wrap(name.to_owned(), theme::semibold(13.0), theme::TEXT);
                let width = 10.0 + 8.0 + 6.0 + label.size().x + if live.camera { 19.0 } else { 0.0 } + 12.0;
                let chip = Rect::from_min_size(pos2(x, head.center().y - 15.0), vec2(width, 30.0));
                painter.rect_filled(chip, 15.0, theme::SURFACE_2);
                painter.circle_filled(pos2(chip.left() + 14.0, chip.center().y), 4.0, theme::LIVE);
                let label_width = label.size().x;
                painter.galley(pos2(chip.left() + 24.0, chip.center().y - label.size().y / 2.0), label, theme::TEXT);
                if live.camera {
                    icon(&painter, pos2(chip.left() + 24.0 + label_width + 12.0, chip.center().y), Icon::Video, 13.0, theme::TEXT);
                }
                x += width + 6.0;
            }

            let body = Rect::from_min_max(pos2(area.left(), head.bottom()), area.max);
            let (strong, hint) = if any_live {
                ("Escolha uma live para assistir", "Assistir dentro do app nativo chega na próxima etapa. Por enquanto, as lives aparecem aqui em cima e na barra lateral.")
            } else {
                ("Ninguém ao vivo por aqui", "Compartilhar tela e webcam pelo app nativo chega nas próximas etapas.")
            };
            let strong = painter.layout_no_wrap(strong.to_owned(), theme::bold(17.0), theme::TEXT);
            let hint = painter.layout(hint.to_owned(), theme::regular(14.0), theme::MUTED, 420.0);
            let height = 90.0 + 8.0 + strong.size().y + 8.0 + hint.size().y;
            let mut y = body.center().y - height / 2.0;
            let logo = Rect::from_min_size(pos2(body.center().x - 45.0, y), vec2(90.0, 90.0));
            painter.image(self.logo.id(), logo, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE.gamma_multiply(0.9));
            y += 98.0;
            painter.galley(pos2(body.center().x - strong.size().x / 2.0, y), strong.clone(), theme::TEXT);
            y += strong.size().y + 8.0;
            painter.galley(pos2(body.center().x - hint.size().x / 2.0, y), hint, theme::MUTED);
            if !self.notice.is_empty() {
                self.notice_bar(ui, body);
            }
        });
    }

    /// Aviso no rodapé da área principal (não deu para entrar, saiu do canal...).
    fn notice_bar(&self, ui: &Ui, area: Rect) {
        let painter = ui.painter();
        let text = painter.layout(self.notice.clone(), theme::regular(13.0), theme::TEXT, (area.width() - 80.0).min(560.0));
        let size = text.size() + vec2(28.0, 20.0);
        let bar = Rect::from_center_size(pos2(area.center().x, area.bottom() - 24.0 - size.y / 2.0), size);
        painter.rect_filled(bar, 10.0, theme::SURFACE_2);
        painter.rect_stroke(bar, 10.0, (1.0, theme::DANGER.gamma_multiply(0.6)), egui::StrokeKind::Inside);
        painter.galley(bar.min + vec2(14.0, 10.0), text, theme::TEXT);
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
                ready = data.loaded && account.online && (self.args.join.is_none() || account.room.is_some());
                if data.loaded && account.room.is_none() && account.joining.is_none() && !self.auto_joined {
                    if let Some(channel) = self.args.join.clone() {
                        self.auto_joined = true;
                        actions.push(Action::Join(channel));
                    }
                }
                let room = account.room.as_ref().map(|room| room.snapshot());
                // Saiu do canal sem pedir (perdeu o acesso ou entrou em outro lugar): avisa e sai.
                let kicked = room.as_ref().and_then(|room| room.issue.clone()).filter(|issue| issue.fatal);
                if let Some(issue) = &kicked {
                    self.notice = issue.message.clone();
                    actions.push(Action::Leave);
                }
                let room = room.filter(|_| kicked.is_none());
                let current = room.as_ref().map_or(account.joining.as_deref().unwrap_or(""), |room| room.room.as_str());
                let channel_name = data.channels.get(current).map_or("", |channel| channel.name.as_str());
                if data.loaded {
                    Sidebar {
                        data: &data,
                        collapsed: &self.collapsed,
                        room: current,
                        logo: &self.logo,
                        version: env!("CARGO_PKG_VERSION"),
                        connection: room.as_ref().map(|room| Connection { online: room.online, channel: channel_name }),
                    }
                    .show(ui, &mut actions);
                    match &room {
                        Some(room) => self.stage(ui, &data, room, channel_name),
                        None => {
                            let nickname = data.profiles.get(&data.uid).cloned().unwrap_or_default();
                            self.home(ui, &nickname);
                        }
                    }
                } else {
                    self.gate(ui, 230.0, |ui| {
                        ui.add_space(20.0);
                        ui.add(egui::Spinner::new().size(28.0).color(theme::MUTED));
                    });
                }
            }
        }
        self.act(&ctx, actions);
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
