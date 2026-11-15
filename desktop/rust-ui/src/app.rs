//! Estado do app e as telas: entrando, aguardando aprovação e a tela principal.
//!
//! O app entra com uma conta, espera a aprovação e mostra a comunidade ao vivo. Dentro de um canal,
//! assiste às lives nativas (as do motor do app) pelo player e transmite uma janela pelo motor
//! (`live.rs`). O que ainda falta está em `docs/rust-app.md`, seção "App full Rust".

use std::collections::{BTreeSet, HashMap};
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
use nimbo_core::room::{Room, RoomSnapshot, Signal};
use nimbo_core::rtdb::{Db, Event, Status};
use nimbo_media::engine::{AudioSource, Source};
use nimbo_media::viewer;
use serde_json::json;

use crate::live::{BroadcastState, Broadcasting, Capture, LiveState, Watching, live_key, parse_key};
use crate::login::{self, LoginManager, Outcome, Timing};
use crate::session_store;
use crate::sidebar::{Action, Connection, Sidebar};
use crate::stage::{Stage, notice_bar};
use crate::theme;
use crate::video::Videos;
use crate::widgets::icon;

/// Como este app se apresenta aos outros no canal e no diagnóstico (o site mostra "0.5.5 (Rust)").
const APP_LABEL: &str = concat!(env!("CARGO_PKG_VERSION"), " (nativo)");
/// Depois de entrar num canal, por quanto tempo as lives que já estavam no ar abrem sozinhas.
const AUTO_OPEN: Duration = Duration::from_secs(5);
/// Quantas lives abrem sozinhas ao entrar.
const AUTO_OPEN_MAX: usize = 4;

#[derive(Default)]
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
    /// Transmite esta janela (o identificador dela no Windows) assim que entrar no canal (para
    /// testar sem clicar).
    pub broadcast: Option<isize>,
    /// Junto com `broadcast`: manda também o som do computador, menos o do próprio app.
    pub sound: bool,
    /// Sem som: o volume de toda live fica em zero. Obrigatório nos testes automáticos.
    pub mute: bool,
    /// Abre no palco toda live do canal, também as que começarem depois (para testar sem clicar).
    pub watch_all: bool,
    /// Imprime os números das lives a cada 2 s, uma linha de JSON por vez.
    pub stats: bool,
    /// Fecha o app depois de tantos segundos.
    pub exit_after: Option<u64>,
    /// Espera pelo menos tantos segundos antes da foto.
    pub shot_after: Option<u64>,
    /// Converte o vídeo na CPU em vez de na placa (para medir os dois caminhos).
    pub cpu_video: bool,
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

/// Lives do canal em que a pessoa está: o que ela assiste, o que transmite e o que está no palco.
struct Lives {
    watching: Watching,
    broadcasting: Option<Broadcasting>,
    /// Lives abertas no palco, por chave "uid:screen" / "uid:camera".
    visible: Vec<String>,
    /// Logo depois de entrar, as lives que já estavam no ar (ou só as da pessoa escolhida) abrem
    /// sozinhas. A lista do canal chega um instante depois da entrada, por isso a janela de tempo.
    auto_open: Option<(Instant, Option<String>)>,
    reconnects: u64,
    /// Lives no ar (a própria e as dos outros), na ordem das etiquetas do topo.
    active: Vec<String>,
    /// As de `visible` que estão no ar: são os ladrilhos do palco.
    shown: Vec<String>,
}

impl Lives {
    fn new(room: Arc<Room>, uid: &str, volume: f32, watch: Option<String>, ctx: &egui::Context) -> Self {
        Self {
            watching: Watching::new(room, uid, volume, ctx),
            broadcasting: None,
            visible: Vec::new(),
            auto_open: Some((Instant::now() + AUTO_OPEN, watch)),
            reconnects: 0,
            active: Vec::new(),
            shown: Vec::new(),
        }
    }

    /// Lives dos outros no ar (de uma pessoa, ou de todos).
    fn keys_of(snapshot: &RoomSnapshot, uid: &str, member: Option<&str>) -> Vec<String> {
        snapshot
            .broadcasts
            .iter()
            .filter(|(id, _)| *id != uid && member.is_none_or(|member| *id == member))
            .flat_map(|(id, kinds)| kinds.keys().map(move |kind| live_key(id, kind)))
            .collect()
    }

    fn open(&mut self, keys: Vec<String>) {
        for key in keys {
            if !self.visible.contains(&key) {
                self.visible.push(key);
            }
        }
    }

    /// A cada quadro da interface: acerta o que está no palco, as conexões de quem assiste e
    /// entrega a sinalização que chegou.
    fn update(&mut self, room: &Room, snapshot: &RoomSnapshot, uid: &str, watch_all: bool) {
        // A conexão com o banco caiu e voltou: as ofertas feitas durante a queda se perderam.
        if snapshot.reconnects != self.reconnects {
            self.reconnects = snapshot.reconnects;
            self.watching.reset(None);
        }
        // A transmissão acabou sozinha (não começou, ou foi encerrada): não fica um ladrilho morto.
        if self.broadcasting.as_ref().is_some_and(|live| live.state() == BroadcastState::Stopped) {
            self.broadcasting = None;
        }
        let others = Self::keys_of(snapshot, uid, None);
        if let Some((until, member)) = &self.auto_open {
            if Instant::now() > *until {
                self.auto_open = None;
            } else {
                let keys = Self::keys_of(snapshot, uid, member.as_deref());
                if !keys.is_empty() {
                    self.auto_open = None;
                    self.open(keys);
                    self.visible.truncate(AUTO_OPEN_MAX);
                }
            }
        }
        if watch_all {
            self.open(others.clone());
        }
        self.active = self.broadcasting.iter().map(|_| live_key(uid, "screen")).chain(others).collect();
        self.shown = self.visible.iter().filter(|key| self.active.contains(key)).cloned().collect();
        let on_stage: BTreeSet<String> = self.shown.iter().cloned().collect();
        self.watching.sync(&snapshot.broadcasts, &snapshot.members, &on_stage);
        self.watching.tick();
        if let Some(live) = &self.broadcasting {
            live.set_members(&snapshot.members);
        }
        for signal in room.take_signals() {
            let Signal::Native { from, kind, answer, sdp, nonce } = signal;
            if answer {
                self.watching.on_answer(&from, &kind, &sdp, &nonce);
            } else if kind == "screen"
                && let Some(live) = &self.broadcasting
            {
                live.offer(&from, &sdp, &nonce);
            }
        }
    }
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
    lives: Option<Lives>,
    /// Canal em que o app está entrando agora, e a pessoa cuja live abrir ao entrar.
    joining: Option<String>,
    watch_on_join: Option<String>,
    stop: Arc<AtomicBool>,
}

impl Drop for Account {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.community = None;
        self.lives = None;
        // Fechar a conexão já tira a presença do canal: o servidor cumpre o pedido feito ao entrar.
        self.room = None;
        self.db.close();
    }
}

/// O que foi medido da última vez, para imprimir a diferença (`--stats`).
struct Meter {
    at: Instant,
    cpu: Duration,
    viewers: HashMap<String, viewer::Stats>,
    drawn: HashMap<String, u64>,
    encoded: usize,
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
    videos: Videos,
    /// Quadros de cada live que já foram para a tela (para medir o que foi desenhado de fato).
    drawn: HashMap<String, u64>,
    meter: Meter,
    started: Instant,
    shot_requested: bool,
    auto_joined: bool,
    auto_broadcast: bool,
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
        let videos = Videos::new(cc.wgpu_render_state.as_ref(), args.cpu_video);
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
            videos,
            drawn: HashMap::new(),
            meter: Meter {
                at: Instant::now(),
                cpu: nimbo_media::process_cpu(),
                viewers: HashMap::new(),
                drawn: HashMap::new(),
                encoded: 0,
            },
            started: Instant::now(),
            shot_requested: false,
            auto_joined: false,
            auto_broadcast: false,
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
        self.account = Some(Account {
            session,
            db,
            approved: None,
            requested: false,
            online: false,
            community: None,
            room: None,
            lives: None,
            joining: None,
            watch_on_join: None,
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
                            let room = Arc::new(room);
                            let volume = if self.args.mute { 0.0 } else { 1.0 };
                            let watch = account.watch_on_join.take();
                            account.lives = Some(Lives::new(room.clone(), &account.session.uid, volume, watch, ctx));
                            account.room = Some(room);
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
                        account.lives = None;
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

    /// Entra num canal (saindo do atual, se houver); `watch` é a pessoa cuja live abrir ao entrar.
    /// A conversa com o servidor fica fora da thread que desenha; o resultado volta como Msg::Joined.
    fn join(&mut self, ctx: &egui::Context, channel: String, watch: Option<String>) {
        let Some(account) = &mut self.account else { return };
        if let Some(room) = account.room.as_ref().filter(|room| room.id() == channel) {
            // Já está no canal: clicar em quem está ao vivo só abre a live dela no palco.
            if let (Some(member), Some(lives)) = (watch, account.lives.as_mut()) {
                let keys = Lives::keys_of(&room.snapshot(), &account.session.uid, Some(&member));
                lives.open(keys);
            }
            return;
        }
        if account.joining.is_some() {
            return;
        }
        account.joining = Some(channel.clone());
        account.watch_on_join = watch;
        // As lives do canal anterior param antes de sair dele.
        account.lives = None;
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
        account.lives = None;
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
                Action::Join(channel) => self.join(ctx, channel, None),
                Action::Watch(channel, member) => self.join(ctx, channel, Some(member)),
                Action::Leave => self.leave(),
                Action::ToggleLive(key) => {
                    if let Some(lives) = self.account.as_mut().and_then(|account| account.lives.as_mut()) {
                        match lives.visible.iter().position(|open| *open == key) {
                            Some(index) => drop(lives.visible.remove(index)),
                            None => lives.visible.push(key),
                        }
                    }
                }
                Action::RetryLive(uid) => {
                    if let Some(lives) = self.account.as_mut().and_then(|account| account.lives.as_mut()) {
                        lives.watching.reset(Some(&uid));
                    }
                }
                Action::StopBroadcast => {
                    if let Some(lives) = self.account.as_mut().and_then(|account| account.lives.as_mut()) {
                        lives.broadcasting = None;
                    }
                }
                // Perfil e administração chegam nas próximas etapas.
                Action::OpenAdmin | Action::EditProfile => {}
            }
        }
    }

    /// Começa a transmitir no canal atual e abre a própria live no palco.
    fn start_broadcast(lives: &mut Lives, room: &Arc<Room>, uid: &str, capture: Capture, ctx: &egui::Context) {
        lives.broadcasting = Some(Broadcasting::start(room.clone(), capture, ctx));
        lives.open(vec![live_key(uid, "screen")]);
    }

    /// Tela principal de uma conta aprovada: barra lateral e, à direita, a tela inicial ou o palco
    /// do canal. Devolve se já está tudo carregado (para a foto de teste).
    fn main_screen(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) -> bool {
        let ctx = ui.ctx().clone();
        let Self { account, args, videos, drawn, collapsed, logo, notice, auto_joined, auto_broadcast, meter, .. } = self;
        let Some(account) = account.as_mut() else { return false };
        let data = account.community.as_ref().map(Community::snapshot).unwrap_or_default();
        let mut ready = data.loaded && account.online && (args.join.is_none() || account.room.is_some());
        if data.loaded
            && account.room.is_none()
            && account.joining.is_none()
            && !*auto_joined
            && let Some(channel) = args.join.clone()
        {
            *auto_joined = true;
            actions.push(Action::Join(channel));
        }
        let room = account.room.as_ref().map(|room| room.snapshot());
        // Saiu do canal sem pedir (perdeu o acesso ou entrou em outro lugar): avisa e sai.
        let kicked = room.as_ref().and_then(|room| room.issue.clone()).filter(|issue| issue.fatal);
        if let Some(issue) = &kicked {
            *notice = issue.message.clone();
            actions.push(Action::Leave);
        }
        let room = room.filter(|_| kicked.is_none());
        if let (Some(snapshot), Some(handle), Some(lives)) = (&room, &account.room, account.lives.as_mut()) {
            if let Some(window) = args.broadcast.filter(|_| !*auto_broadcast) {
                *auto_broadcast = true;
                let audio = args.sound.then(|| AudioSource::Exclude(std::process::id()));
                Self::start_broadcast(lives, handle, &data.uid, Capture { source: Source::Window(window), audio }, &ctx);
            }
            lives.update(handle, snapshot, &data.uid, args.watch_all);
            for key in lives.shown.iter().filter(|key| parse_key(key).0 != data.uid) {
                if let Some(frame) = lives.watching.frame(key)
                    && videos.upload(&ctx, key, &frame)
                {
                    *drawn.entry(key.clone()).or_default() += 1;
                }
            }
            videos.keep_only(&lives.shown);
            // Nos testes sem clique, a foto espera a live estar no palco.
            if args.watch_all {
                ready &= lives.shown.iter().any(|key| lives.watching.state(key) == LiveState::Playing && videos.size(key).is_some());
            }
            if args.broadcast.is_some() {
                ready &= lives.broadcasting.as_ref().is_some_and(|live| live.state() == BroadcastState::Live);
            }
            if args.stats {
                report(meter, lives, videos, drawn);
            }
            // Os prazos das conexões e os números da transmissão andam mesmo com a tela parada.
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        let lives = account.lives.as_ref().filter(|_| room.is_some());
        let current = room.as_ref().map_or(account.joining.as_deref().unwrap_or(""), |room| room.room.as_str());
        let channel_name = data.channels.get(current).map_or("", |channel| channel.name.as_str());
        if !data.loaded {
            gate(ui, logo, 230.0, |ui| {
                ui.add_space(20.0);
                ui.add(egui::Spinner::new().size(28.0).color(theme::MUTED));
            });
            return ready;
        }
        Sidebar {
            data: &data,
            collapsed,
            room: current,
            logo,
            version: env!("CARGO_PKG_VERSION"),
            connection: room.as_ref().map(|room| Connection {
                online: room.online,
                channel: channel_name,
                broadcasting: lives.and_then(|lives| lives.broadcasting.as_ref()).map(Broadcasting::state),
            }),
        }
        .show(ui, actions);
        match (&room, lives) {
            (Some(room), Some(lives)) => Stage {
                data: &data,
                room,
                channel_name,
                active: &lives.active,
                shown: &lives.shown,
                watching: &lives.watching,
                broadcasting: lives.broadcasting.as_ref(),
                videos,
                logo,
                notice,
            }
            .show(ui, actions),
            _ => {
                let nickname = data.profiles.get(&data.uid).cloned().unwrap_or_default();
                home(ui, logo, &nickname, notice);
            }
        }
        ready
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
        let least = Duration::from_secs(self.args.shot_after.unwrap_or(3));
        // Com o login pelo navegador no meio, a espera é maior: alguém precisa terminar o login lá.
        let limit = least + Duration::from_secs(if self.args.login { 90 } else { 12 });
        if !self.shot_requested && ((ready && waited > least) || waited > limit) {
            self.shot_requested = true;
            let (done, repaint) = (self.shot_done.clone(), ctx.clone());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |shot| {
                let [width, height] = shot.size;
                let pixels: Vec<u8> = shot.pixels.iter().flat_map(|pixel| pixel.to_array()).collect();
                let result = image::save_buffer(&path, &pixels, width as u32, height as u32, image::ColorType::Rgba8);
                println!("{}", json!({ "foto": path, "largura": width, "altura": height, "ok": result.is_ok(), "pronto": ready }));
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
                gate(ui, &self.logo, if signing_in && !in_browser { 230.0 } else { 400.0 }, |ui| {
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
                gate(ui, &self.logo, if waiting { 230.0 } else { 384.0 }, |ui| {
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
            Some(_) => ready = self.main_screen(ui, &mut actions),
        }
        self.act(&ctx, actions);
        self.screenshot(ui, ready);
        if self.args.exit_after.is_some_and(|seconds| self.started.elapsed() >= Duration::from_secs(seconds)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// `--stats`: a cada 2 s, uma linha de JSON com os números das lives (o que chegou, o que foi
/// decodificado, o que foi desenhado) e da transmissão, mais a CPU do processo.
fn report(meter: &mut Meter, lives: &Lives, videos: &Videos, drawn: &HashMap<String, u64>) {
    let elapsed = meter.at.elapsed();
    if elapsed < Duration::from_secs(2) {
        return;
    }
    let seconds = elapsed.as_secs_f64();
    let cpu = nimbo_media::process_cpu();
    let per_second = |now: u64, before: u64| (now.saturating_sub(before) as f64 / seconds * 10.0).round() / 10.0;
    let mut watching = Vec::new();
    let mut current = HashMap::new();
    for (key, stats) in lives.watching.stats() {
        let before = meter.viewers.get(&key);
        let decoded = stats.frames_decoded.saturating_sub(before.map_or(0, |old| old.frames_decoded));
        let time = stats.decode_time.saturating_sub(before.map_or(Duration::ZERO, |old| old.decode_time));
        let delay = stats.latency.saturating_sub(before.map_or(Duration::ZERO, |old| old.latency));
        let each = |total: Duration| if decoded == 0 { 0.0 } else { (total.as_secs_f64() * 1000.0 / decoded as f64 * 100.0).round() / 100.0 };
        watching.push(json!({
            "live": key.chars().take(6).collect::<String>() + ":" + parse_key(&key).1,
            "estado": stats.state.as_str(),
            "rota": stats.route,
            "decodificador": stats.decoder,
            "resolucao": format!("{}x{}", stats.width, stats.height),
            "recebidos_por_s": per_second(stats.frames_received, before.map_or(0, |old| old.frames_received)),
            "decodificados_por_s": per_second(stats.frames_decoded, before.map_or(0, |old| old.frames_decoded)),
            "desenhados_por_s": per_second(drawn.get(&key).copied().unwrap_or(0), meter.drawn.get(&key).copied().unwrap_or(0)),
            "descartados": stats.frames_dropped,
            "pacotes_faltando": stats.packets_missing,
            "quadros_completos_pedidos": stats.keyframes_requested,
            "decodificar_ms_por_quadro": each(time),
            "do_pacote_ao_quadro_ms": each(delay),
            "mbps": (f64::from(stats.bitrate) / 1e5).round() / 10.0,
            "som_ms_por_s": per_second(stats.audio_played, before.map_or(0, |old| old.audio_played)) / 48.0,
            "som_secas": stats.audio_underruns,
        }));
        current.insert(key, stats);
    }
    let broadcast = lives.broadcasting.as_ref().map(|live| {
        let stats = live.stats();
        let encoded = stats.frames_encoded;
        let line = json!({
            "estado": format!("{:?}", live.state()),
            "espectadores": stats.viewers,
            "codificador": stats.encoder,
            "alvo_mbps": (f64::from(stats.target_bitrate) / 1e5).round() / 10.0,
            "produzido_mbps": (f64::from(stats.produced_bitrate) / 1e5).round() / 10.0,
            "quadros_codificados_por_s": per_second(encoded as u64, meter.encoded as u64),
            "som": stats.audio,
        });
        meter.encoded = encoded;
        line
    });
    println!(
        "{}",
        json!({
            "pid": std::process::id(),
            "cpu_um_nucleo_pct": (cpu.saturating_sub(meter.cpu).as_secs_f64() / seconds * 1000.0).round() / 10.0,
            "desenho": videos.path(),
            "assistindo": watching,
            "transmitindo": broadcast,
        })
    );
    meter.at = Instant::now();
    meter.cpu = cpu;
    meter.viewers = current;
    meter.drawn = drawn.clone();
}

/// Fundo e cartão central das telas de entrada, como `.gate` no site.
fn gate(ui: &mut Ui, logo: &TextureHandle, height: f32, add: impl FnOnce(&mut Ui)) {
    egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG)).show(ui, |ui| {
        let area = ui.max_rect();
        let card = Rect::from_center_size(area.center(), vec2(400.0_f32.min(area.width() - 32.0), height));
        let painter = ui.painter().clone();
        painter.rect_filled(card, 18.0, Color32::from_rgb(0x13, 0x1a, 0x2e));
        painter.rect_stroke(card, 18.0, (1.0, theme::BORDER), egui::StrokeKind::Inside);
        let mascot = Rect::from_center_size(pos2(card.center().x, card.top() + 36.0 + 42.0), vec2(84.0, 84.0));
        painter.image(logo.id(), mascot, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        let inner = Rect::from_min_max(pos2(card.left() + 30.0, mascot.bottom() + 14.0), pos2(card.right() - 30.0, card.bottom() - 30.0));
        let mut content =
            ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Center)));
        add(&mut content);
    });
}

/// Tela inicial, antes de entrar num canal.
fn home(ui: &mut Ui, logo: &TextureHandle, nickname: &str, notice: &str) {
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
        let mascot = Rect::from_min_size(pos2(area.center().x - 55.0, y), vec2(110.0, 110.0));
        painter.image(logo.id(), mascot, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE.gamma_multiply(0.95));
        y += 120.0;
        painter.galley(pos2(area.center().x - title.size().x / 2.0, y), title.clone(), theme::TEXT);
        y += title.size().y + 10.0;
        painter.galley(pos2(area.center().x - hint.size().x / 2.0, y), hint, theme::MUTED);
        if !notice.is_empty() {
            notice_bar(ui, area, notice);
        }
    });
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
