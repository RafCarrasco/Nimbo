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

use crate::login::{self, LoginManager, Outcome, Timing};
use crate::session_store;
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
    /// Fala com o Firebase de verdade em vez dos emuladores.
    pub production: bool,
    /// Endereço do site que faz o login do Google (a página `/desktop-login`).
    pub site: Option<String>,
    /// Começa o login pelo navegador ao abrir, como se a pessoa tivesse clicado (para testar).
    pub login: bool,
}

const PRODUCTION_SITE: &str = "https://golive-ad118.web.app";
const EMULATOR_SITE: &str = "http://localhost:5173";

enum Msg {
    /// Resultado de um login. `None` no erro: não havia sessão guardada, ou a pessoa cancelou.
    SignedIn(Result<Session, Option<String>>),
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
    /// Login em andamento no navegador (a pessoa pode cancelar).
    in_browser: bool,
    logins: Arc<LoginManager>,
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
        let project = if args.production { Project::production() } else { Project::emulator() };
        let mut app = Self {
            args,
            project,
            tx,
            rx,
            account: None,
            signing_in: false,
            in_browser: false,
            logins: Arc::new(LoginManager::default()),
            error: String::new(),
            logo,
            collapsed: BTreeSet::new(),
            notice: String::new(),
            started: Instant::now(),
            shot_requested: false,
            auto_joined: false,
            shot_done: Arc::new(AtomicBool::new(false)),
        };
        match app.args.dev_user.clone() {
            Some(name) if app.project.emulator => app.sign_in(&cc.egui_ctx, &name),
            _ => app.resume(&cc.egui_ctx),
        }
        if app.args.login && !app.signing_in {
            app.sign_in_with_browser(&cc.egui_ctx);
        }
        app
    }

    fn sign_in(&mut self, ctx: &egui::Context, dev_user: &str) {
        self.signing_in = true;
        self.error.clear();
        let (project, tx, ctx, credential) = (self.project.clone(), self.tx.clone(), ctx.clone(), auth::dev_credential(dev_user));
        std::thread::spawn(move || {
            let result = auth::sign_in_google(&project, &credential, "").map_err(|e| Some(e.to_string()));
            let _ = tx.send(Msg::SignedIn(result));
            ctx.request_repaint();
        });
    }

    /// Ao abrir: se há sessão guardada, entra com ela sem passar pelo navegador.
    fn resume(&mut self, ctx: &egui::Context) {
        let Some(saved) = session_store::load(self.project.emulator) else { return };
        self.signing_in = true;
        let (project, tx, ctx) = (self.project.clone(), self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            // Sessão vencida ou revogada: volta para a tela de entrada, sem mensagem de erro.
            let result = auth::resume(&project, &saved.refresh_token, &saved.email).map_err(|_| None);
            let _ = tx.send(Msg::SignedIn(result));
            ctx.request_repaint();
        });
    }

    /// "Entrar com Google": abre o navegador do sistema na página de login do site, que devolve a
    /// credencial do Google para uma porta local deste app (o mesmo caminho do app atual).
    fn sign_in_with_browser(&mut self, ctx: &egui::Context) {
        let login = match self.logins.begin(Timing::default()) {
            Ok(login) => login,
            Err(e) => {
                self.error = format!("Não foi possível iniciar o login: {e}");
                return;
            }
        };
        let site = self.args.site.clone().unwrap_or_else(|| {
            if self.project.emulator { EMULATOR_SITE } else { PRODUCTION_SITE }.to_owned()
        });
        let url = login::login_url(&site, login.port, &login.state);
        // O link leva só a porta e o código de uso único; a credencial nunca passa por aqui.
        if std::env::var_os("NIMBO_LOGIN_PRINT").is_some() {
            println!("{}", json!({ "login": url }));
        } else if let Err(e) = open::that_detached(&url) {
            self.error = format!("Não foi possível abrir o navegador: {e}");
            return;
        }
        self.signing_in = true;
        self.in_browser = true;
        self.error.clear();
        let (project, tx, ctx) = (self.project.clone(), self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = match login.wait() {
                Outcome::SignedIn(credentials) => {
                    auth::sign_in_google(&project, &credentials.id_token, &credentials.access_token).map_err(|e| Some(e.to_string()))
                }
                Outcome::MissingCredential => Err(Some(login::MISSING_CREDENTIAL.to_owned())),
                Outcome::Cancelled => Err(None),
                Outcome::Expired => Err(Some(login::EXPIRED.to_owned())),
            };
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
                Msg::SignedIn(result) => {
                    self.signing_in = false;
                    self.in_browser = false;
                    match result {
                        Ok(session) => {
                            self.remember(&session);
                            self.signed_in(ctx, session);
                            // Volta para a frente: a pessoa estava no navegador.
                            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        }
                        Err(error) => self.error = error.unwrap_or_default(),
                    }
                }
                Msg::Renewed(session) => {
                    self.remember(&session);
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

    /// Guarda a sessão para a próxima abertura. Contas de teste (`--dev-user`) não são guardadas.
    fn remember(&self, session: &Session) {
        if self.args.dev_user.is_none() {
            let saved = session_store::Saved { refresh_token: session.refresh_token.clone(), email: session.email.clone() };
            session_store::save(self.project.emulator, &saved);
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
                Action::SignOut => {
                    self.account = None;
                    session_store::clear(self.project.emulator);
                }
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
            let hint = theme::centered(&painter, hint, theme::regular(14.0), theme::MUTED, 420.0);
            let height = 90.0 + 8.0 + strong.size().y + 8.0 + hint.size().y;
            let mut y = body.center().y - height / 2.0;
            let logo = Rect::from_min_size(pos2(body.center().x - 45.0, y), vec2(90.0, 90.0));
            painter.image(self.logo.id(), logo, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE.gamma_multiply(0.9));
            y += 98.0;
            painter.galley(pos2(body.center().x - strong.size().x / 2.0, y), strong.clone(), theme::TEXT);
            y += strong.size().y + 8.0;
            painter.galley(pos2(body.center().x, y), hint, theme::MUTED);
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
        // Com o login pelo navegador no meio, a espera é maior: alguém precisa terminar o login lá.
        let limit = Duration::from_secs(if self.args.login { 90 } else { 12 });
        if !self.shot_requested && ((ready && waited > Duration::from_secs(3)) || waited > limit) {
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
                let (signing_in, in_browser, error) = (self.signing_in, self.in_browser, self.error.clone());
                let (mut start, mut cancel) = (false, false);
                self.gate(ui, if signing_in && !in_browser { 230.0 } else { 400.0 }, |ui| {
                    if signing_in && !in_browser {
                        ui.add_space(20.0);
                        ui.add(egui::Spinner::new().size(28.0).color(theme::MUTED));
                        return;
                    }
                    wordmark(ui);
                    ui.add_space(14.0);
                    ui.label(egui::RichText::new("Suas lives. Sua comunidade.").color(theme::MUTED));
                    ui.add_space(20.0);
                    if in_browser {
                        ui.add(egui::Spinner::new().size(22.0).color(theme::MUTED));
                        ui.add_space(12.0);
                        ui.label(egui::RichText::new("Termine o login na janela do navegador que abriu.").font(theme::regular(12.0)).color(theme::MUTED));
                        ui.add_space(10.0);
                        cancel = ghost_button(ui, "Cancelar");
                    } else {
                        start = google_button(ui);
                        ui.add_space(14.0);
                        ui.label(
                            egui::RichText::new("O acesso depende da aprovação de um administrador.")
                                .font(theme::regular(12.0))
                                .color(theme::MUTED),
                        );
                    }
                    if !error.is_empty() {
                        ui.add_space(10.0);
                        ui.label(egui::RichText::new(error).font(theme::regular(13.0)).color(theme::DANGER));
                    }
                });
                if start {
                    self.sign_in_with_browser(&ctx);
                }
                if cancel {
                    self.logins.cancel();
                }
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

/// Botão branco "Entrar com Google" (`.google-button` no site).
fn google_button(ui: &mut Ui) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    let painter = ui.painter();
    let fill = if response.hovered() { Color32::from_rgb(0xf1, 0xf3, 0xf9) } else { Color32::WHITE };
    painter.rect_filled(rect, 10.0, fill);
    let dark = Color32::from_rgb(0x1f, 0x23, 0x33);
    let label = painter.layout_no_wrap("Entrar com Google".to_owned(), theme::semibold(14.0), dark);
    let mark = painter.layout_no_wrap("G".to_owned(), theme::bold(17.0), Color32::from_rgb(0x19, 0x76, 0xd2));
    let x = rect.center().x - (mark.size().x + 10.0 + label.size().x) / 2.0;
    let label_x = x + mark.size().x + 10.0;
    painter.galley(pos2(x, rect.center().y - mark.size().y / 2.0), mark, dark);
    painter.galley(pos2(label_x, rect.center().y - label.size().y / 2.0), label, dark);
    response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
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
