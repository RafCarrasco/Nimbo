//! Atualização automática do app instalado, no contrato que o site já usava com o app antigo
//! em Electron (`watchUpdate`, `installUpdate` e `onUpdateProgress` de `src/desktop.ts`):
//! confere ao abrir e a cada 30 minutos, baixa em segundo plano, avisa a página e instala quando ela
//! pede (fechando e reabrindo o app) ou, sem reabrir, quando a pessoa fecha o app.
//!
//! O canal é o `latest.json` do Release mais recente do repositório (o endereço fica em
//! `tauri.conf.json`). O app antigo em Electron lia `latest.yml` no mesmo Release; como esse arquivo
//! não é mais publicado, ele simplesmente não acha atualização. Por isso não se publica mais
//! Release do Electron como "latest": ele tiraria o `latest.json` do ar para este app.
//!
//! Qualquer falha (sem rede, 404, assinatura errada) vai só para `nimbo-update.log`, na pasta de
//! dados do app: a pessoa não vê erro, e a próxima conferência tenta de novo.
//!
//! `NIMBO_UPDATE_URL=<endereço do latest.json>` troca o servidor, só para testar o ciclo
//! inteiro. A assinatura continua sendo conferida com a chave pública que está no app.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Value, json};
use tauri::ipc::Channel;
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, RunEvent, Wry};
use tauri_plugin_updater::{Update, UpdaterExt};
use url::{Host, Url};

use crate::bridge::{FromApp, Reply};

/// Outro servidor de atualização, só para teste (o app em Electron tem a mesma variável).
const UPDATE_URL_ENV: &str = "NIMBO_UPDATE_URL";
/// Era de 6 horas, como no app em Electron. Em 15/11/2026 uma correção ficou parada porque quem
/// transmitia estava com o app aberto havia horas e não soube da versão nova; conferir custa um
/// arquivo pequeno, então vale conferir bem mais vezes.
const CHECK_EVERY: Duration = Duration::from_secs(30 * 60);
/// Depois de uma falha, tenta de novo logo (rede que caiu, filtro local que cortou a conexão) e
/// vai espaçando; na quarta falha seguida volta ao intervalo normal.
const RETRY_AFTER: [Duration; 3] = [
    Duration::from_secs(30),
    Duration::from_secs(2 * 60),
    Duration::from_secs(10 * 60),
];
/// Sem prazo, uma conexão que parou de responder prenderia a conferência para sempre.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const LOG_FILE: &str = "nimbo-update.log";
/// O registro recomeça ao passar disto; ele serve para olhar o que aconteceu por último.
const LOG_LIMIT: u64 = 256 * 1024;

/// O plugin do Tauri que confere, baixa e instala. Os comandos dele para a página
/// (`plugin:updater|...`) não recebem permissão: a página só fala com os dois comandos daqui.
pub fn updater() -> TauriPlugin<Wry, tauri_plugin_updater::Config> {
    tauri_plugin_updater::Builder::new().build()
}

/// Liga o ciclo de atualização ao app: estado, conferência periódica e instalação ao fechar.
pub fn plugin() -> TauriPlugin<Wry> {
    tauri::plugin::Builder::new("nimbo-update")
        .setup(|app, _api| {
            app.manage(Updates::new(app));
            Ok(())
        })
        .on_event(|app, event| match event {
            // Só depois de tudo pronto: a conferência usa o estado do plugin `updater`.
            RunEvent::Ready => start(app.clone()),
            RunEvent::Exit => install_on_exit(app),
            _ => {}
        })
        .build()
}

/// Aplica `NIMBO_UPDATE_URL` na configuração antes de o app subir. Sem a variável, nada muda.
pub fn prepare(config: &mut tauri::Config) {
    let configured = std::env::var(UPDATE_URL_ENV).ok();
    // Endereço inválido não cai no servidor de verdade: `start` desliga a atualização e registra.
    if let Ok(Some(url)) = endpoint_override(configured.as_deref()) {
        point_at(&mut config.plugins.0, &url);
    }
}

/// Endereço de teste, se pedido. `http` só vale para esta máquina; fora dela, só `https`.
fn endpoint_override(configured: Option<&str>) -> Result<Option<Url>, String> {
    let Some(text) = configured.filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let url = Url::parse(text).map_err(|e| e.to_string())?;
    let local = match url.host() {
        Some(Host::Domain(name)) => name == "localhost",
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    match url.scheme() {
        "https" => Ok(Some(url)),
        "http" if local => Ok(Some(url)),
        "http" => Err("http só vale para 127.0.0.1 ou localhost".into()),
        _ => Err("precisa ser um endereço http(s)".into()),
    }
}

/// Troca o endereço do `latest.json` na configuração do plugin.
fn point_at(plugins: &mut HashMap<String, Value>, url: &Url) {
    let settings = plugins.entry("updater".into()).or_insert_with(|| json!({}));
    let Some(settings) = settings.as_object_mut() else {
        return;
    };
    settings.insert("endpoints".into(), json!([url.as_str()]));
    if url.scheme() == "http" {
        // O plugin recusa http em build de produção. Aqui é o servidor de teste local, e o
        // instalador baixado continua tendo que passar pela assinatura.
        settings.insert("dangerousInsecureTransportProtocol".into(), json!(true));
    }
}

/// A chave pública é base64 puro. O marcador que fica no repositório até a chave de verdade ser
/// criada não é, e com ele o app nem tenta atualizar (baixaria para sempre recusar a assinatura).
fn usable_pubkey(key: &str) -> bool {
    key.len() >= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

/// O instalador deixa um `uninstall.exe` ao lado do app. Sem ele é um build de desenvolvimento:
/// atualizar dali instalaria o Nimbo na máquina de quem só rodou `cargo run`.
fn installed(exe: &Path) -> bool {
    exe.parent()
        .is_some_and(|folder| folder.join("uninstall.exe").is_file())
}

/// Por que este app não se atualiza, se for o caso.
fn disabled_reason(app: &AppHandle) -> Option<String> {
    if let Err(reason) = endpoint_override(std::env::var(UPDATE_URL_ENV).ok().as_deref()) {
        return Some(format!("{UPDATE_URL_ENV} inválida ({reason})"));
    }
    let pubkey = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|settings| settings.get("pubkey"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !usable_pubkey(pubkey) {
        return Some("sem chave pública de atualização neste build".into());
    }
    if !std::env::current_exe().is_ok_and(|exe| installed(&exe)) {
        return Some("app não instalado (sem uninstall.exe ao lado)".into());
    }
    None
}

/// Quanto esperar até a próxima conferência, dado o número de falhas seguidas.
fn next_check(failures: usize) -> Duration {
    match failures {
        0 => CHECK_EVERY,
        n => RETRY_AFTER.get(n - 1).copied().unwrap_or(CHECK_EVERY),
    }
}

/// Porcentagem baixada, quando o servidor informa o tamanho.
fn percent(received: u64, total: Option<u64>) -> Option<u8> {
    let total = total.filter(|total| *total > 0)?;
    let done = u128::from(received.min(total)) * 100 / u128::from(total);
    u8::try_from(done).ok()
}

/// O que o Rust manda para a página pelo canal entregue em `watch_update`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Event {
    Progress { percent: u8 },
    Ready { version: String },
}

/// `{ version }`, como o `updateReady` do app em Electron.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Info {
    version: String,
}

/// Atualização baixada e com a assinatura já conferida, esperando a hora de instalar.
struct Downloaded {
    update: Update,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct State {
    ready: Option<Downloaded>,
    /// Página que pediu os avisos. Só a última: o site chama uma vez por carregamento.
    watcher: Option<Channel<Event>>,
    /// Download em andamento, para a página que chega no meio dele.
    percent: u8,
}

/// Estado da atualização, um por app.
pub struct Updates {
    state: Mutex<State>,
    log: Log,
    /// Uma conferência por vez: a da rotina de 30 minutos e a pedida pela página não baixam o mesmo
    /// instalador duas vezes ao mesmo tempo.
    checking: tauri::async_runtime::Mutex<()>,
}

impl Updates {
    fn new(app: &AppHandle) -> Self {
        // Mesma pasta dos dados do WebView2: `NIMBO_DATA_DIR` nos testes, a do identificador no resto.
        let folder = std::env::var_os("NIMBO_DATA_DIR")
            .filter(|folder| !folder.is_empty())
            .map(PathBuf::from)
            .or_else(|| app.path().app_local_data_dir().ok());
        Self {
            state: Mutex::default(),
            checking: tauri::async_runtime::Mutex::new(()),
            log: Log {
                file: folder.map(|folder| folder.join(LOG_FILE)),
                version: app.package_info().version.to_string(),
            },
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // Nada aqui deixa o estado pela metade; se outra thread quebrou, segue com o que há.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn send(&self, event: Event) {
        let watcher = self.state().watcher.clone();
        if let Some(watcher) = watcher {
            // Página recarregada ou fechada: não há para quem avisar, e tudo bem.
            let _ = watcher.send(event);
        }
    }

    fn progress(&self, percent: u8) {
        self.state().percent = percent;
        self.send(Event::Progress { percent });
    }

    fn has(&self, version: &str) -> bool {
        self.state()
            .ready
            .as_ref()
            .is_some_and(|downloaded| downloaded.update.version == version)
    }

    fn store(&self, update: Update, bytes: Vec<u8>) {
        let version = update.version.clone();
        self.state().ready = Some(Downloaded { update, bytes });
        self.log.write(&format!("baixada {version}"));
        self.send(Event::Ready { version });
    }

    fn watch(&self, events: Channel<Event>) -> Option<Info> {
        let (ready, percent) = {
            let mut state = self.state();
            state.watcher = Some(events.clone());
            let ready = state.ready.as_ref().map(|downloaded| Info {
                version: downloaded.update.version.clone(),
            });
            (ready, state.percent)
        };
        if ready.is_none() && (1..100).contains(&percent) {
            let _ = events.send(Event::Progress { percent });
        }
        ready
    }

    fn ready(&self) -> Option<Info> {
        self.state().ready.as_ref().map(|downloaded| Info {
            version: downloaded.update.version.clone(),
        })
    }

    fn take(&self) -> Option<Downloaded> {
        self.state().ready.take()
    }
}

/// A página pede os avisos de atualização e recebe a que já estiver baixada, se houver.
#[tauri::command]
pub async fn watch_update(
    _caller: FromApp,
    app: AppHandle,
    events: Channel<Event>,
) -> Reply<Option<Info>> {
    Reply::ok(app.state::<Updates>().watch(events))
}

/// Instala a atualização baixada, sem janela, e reabre o app. `false` se não há o que instalar.
#[tauri::command]
pub async fn install_update(_caller: FromApp, app: AppHandle) -> Reply<bool> {
    let Some(downloaded) = app.state::<Updates>().take() else {
        return Reply::ok(false);
    };
    std::thread::spawn(move || {
        // Um instante para a resposta chegar à página antes de o app fechar (o Electron faz igual).
        std::thread::sleep(Duration::from_millis(300));
        let updates = app.state::<Updates>();
        updates
            .log
            .write(&format!("instalando {}", downloaded.update.version));
        // No Windows isto só volta se der errado: abre o instalador e encerra o app.
        if let Err(e) = downloaded.update.install(&downloaded.bytes) {
            updates.log.write(&format!("erro ao instalar: {e}"));
            // Devolve o download para a página poder pedir de novo.
            updates.state().ready.get_or_insert(downloaded);
        }
    });
    Reply::ok(true)
}

/// Confere agora e devolve a atualização baixada, se houver. A página pede quando vê que o site
/// publicado mudou: sem isto, o site novo carregava dentro do app velho, que só conferia de 30 em
/// 30 minutos, e a pessoa tinha de fechar e abrir o Nimbo (relatado em 17/11/2026).
#[tauri::command]
pub async fn check_update(_caller: FromApp, app: AppHandle) -> Reply<Option<Info>> {
    let updates = app.state::<Updates>();
    if let Some(reason) = disabled_reason(&app) {
        updates.log.write(&format!(
            "conferência pedida pela página, desligada: {reason}"
        ));
        return Reply::ok(None);
    }
    updates.log.write("conferência pedida pela página");
    match check(&app).await {
        Ok(()) => Reply::ok(updates.ready()),
        Err(e) => {
            updates.log.write(&format!("erro: {e}"));
            updates.progress(0);
            Reply::error(format!("Não deu para procurar atualização agora: {e}"))
        }
    }
}

/// Começa a conferir, numa thread própria: ao abrir e depois a cada 30 minutos.
fn start(app: AppHandle) {
    let updates = app.state::<Updates>();
    if let Some(reason) = disabled_reason(&app) {
        updates.log.write(&format!("desligada: {reason}"));
        return;
    }
    updates.log.write("aberto");
    let worker = app.clone();
    let spawned = std::thread::Builder::new()
        .name("nimbo-update".into())
        .spawn(move || {
            let mut failures = 0;
            loop {
                match tauri::async_runtime::block_on(check(&worker)) {
                    Ok(()) => failures = 0,
                    Err(e) => {
                        failures += 1;
                        let updates = worker.state::<Updates>();
                        updates.log.write(&format!("erro: {e}"));
                        // Tira da tela o "baixando" de um download que não terminou.
                        updates.progress(0);
                    }
                }
                std::thread::sleep(next_check(failures));
            }
        });
    if let Err(e) = spawned {
        updates
            .log
            .write(&format!("erro: a conferência não começou: {e}"));
    }
}

/// Uma conferência: se há versão nova que ainda não foi baixada, baixa e avisa a página.
async fn check(app: &AppHandle) -> tauri_plugin_updater::Result<()> {
    let updates = app.state::<Updates>();
    let _one_at_a_time = updates.checking.lock().await;
    let updater = app.updater_builder().timeout(CHECK_TIMEOUT).build()?;
    let Some(mut update) = updater.check().await? else {
        return Ok(());
    };
    if updates.has(&update.version) {
        return Ok(());
    }
    updates.log.write(&format!("disponível {}", update.version));
    update.timeout = Some(DOWNLOAD_TIMEOUT);
    let mut received = 0u64;
    let mut shown = None;
    // `download` só devolve os bytes depois de conferir a assinatura com a chave pública do app.
    let bytes = update
        .download(
            |chunk, total| {
                received += chunk as u64;
                let now = percent(received, total).filter(|now| Some(*now) != shown);
                if let Some(now) = now {
                    shown = Some(now);
                    updates.progress(now);
                }
            },
            || {},
        )
        .await?;
    // Servidor que não informa o tamanho: a página ainda precisa ver o download terminar.
    updates.progress(100);
    updates.store(update, bytes);
    Ok(())
}

/// A pessoa fechou o app com uma atualização baixada: instala sem janela e sem reabrir, para a
/// próxima abertura já ser a versão nova (o `autoInstallOnAppQuit` do app em Electron).
fn install_on_exit(app: &AppHandle) {
    let updates = app.state::<Updates>();
    let Some(downloaded) = updates.take() else {
        return;
    };
    updates.log.write(&format!(
        "instalando {} ao fechar",
        downloaded.update.version
    ));
    let update = downloaded.update.restart_after_install(false);
    if let Err(e) = update.install(&downloaded.bytes) {
        updates.log.write(&format!("erro ao instalar: {e}"));
    }
}

/// Registro do que a atualização fez, uma linha por acontecimento: hora, versão do app, mensagem.
struct Log {
    file: Option<PathBuf>,
    version: String,
}

impl Log {
    fn write(&self, message: &str) {
        let Some(file) = &self.file else {
            return;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let line = format!("{} {} {message}\n", timestamp(now), self.version);
        if let Some(folder) = file.parent() {
            let _ = std::fs::create_dir_all(folder);
        }
        let restart = std::fs::metadata(file).is_ok_and(|meta| meta.len() > LOG_LIMIT);
        let written = OpenOptions::new()
            .create(true)
            .append(!restart)
            .write(true)
            .truncate(restart)
            .open(file)
            .and_then(|mut file| file.write_all(line.as_bytes()));
        // Sem registro o app segue igual; só não dá para olhar depois o que houve.
        if let Err(e) = written {
            eprintln!("[nimbo] atualização: {message} (sem registro: {e})");
        }
    }
}

/// Segundos desde 1970 em `AAAA-MM-DDTHH:MM:SSZ` (UTC), como o registro do app em Electron.
fn timestamp(seconds: u64) -> String {
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Dias para data civil (Howard Hinnant), com o ano começando em março.
    let shifted = days + 719_468;
    let (era, day_of_era) = (shifted / 146_097, shifted % 146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = include_str!("../tauri.conf.json");
    const FEED: &str = "https://github.com/RafCarrasco/Nimbo/releases/latest/download/latest.json";
    /// Formato de uma chave pública do Tauri (base64 do arquivo do minisign). Não é chave de ninguém.
    const SAMPLE_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDAxMjM0NTY3ODlBQkNERUYKUldUdnphdUpaMFVqQVFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUEK";

    fn config() -> Value {
        serde_json::from_str(CONFIG).unwrap()
    }

    #[test]
    fn percent_follows_the_download_and_never_passes_100() {
        assert_eq!(percent(0, Some(200)), Some(0));
        assert_eq!(percent(1, Some(200)), Some(0));
        assert_eq!(percent(2, Some(200)), Some(1));
        assert_eq!(percent(199, Some(200)), Some(99));
        assert_eq!(percent(200, Some(200)), Some(100));
        // Servidor que mandou mais do que anunciou.
        assert_eq!(percent(999, Some(200)), Some(100));
        assert_eq!(percent(u64::MAX, Some(u64::MAX)), Some(100));
        // Sem tamanho não há porcentagem.
        assert_eq!(percent(10, None), None);
        assert_eq!(percent(10, Some(0)), None);
    }

    #[test]
    fn retries_soon_after_a_failure_then_goes_back_to_the_usual_interval() {
        assert_eq!(next_check(0), CHECK_EVERY);
        assert_eq!(next_check(1), Duration::from_secs(30));
        assert_eq!(next_check(2), Duration::from_secs(120));
        assert_eq!(next_check(3), Duration::from_secs(600));
        assert_eq!(next_check(4), CHECK_EVERY);
        assert_eq!(next_check(usize::MAX), CHECK_EVERY);
        assert_eq!(CHECK_EVERY, Duration::from_secs(1_800));
    }

    #[test]
    fn test_server_is_https_or_this_machine() {
        let chosen = |text: &str| endpoint_override(Some(text)).map(|url| url.map(String::from));
        assert_eq!(endpoint_override(None), Ok(None));
        assert_eq!(endpoint_override(Some("")), Ok(None));
        for good in [
            "http://127.0.0.1:8765/latest.json",
            "http://localhost:8765/latest.json",
            "http://[::1]:8765/latest.json",
            "https://example.com/beta/latest.json",
        ] {
            assert_eq!(chosen(good), Ok(Some(good.to_string())), "{good}");
        }
        for bad in [
            "http://example.com/latest.json",
            "http://192.168.0.10/latest.json",
            "http://localhost.evil.invalid/latest.json",
            "http://127.0.0.1.evil.invalid/latest.json",
            "file:///C:/latest.json",
            "ftp://127.0.0.1/latest.json",
            "latest.json",
        ] {
            assert!(chosen(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn test_server_replaces_the_feed_and_only_http_lowers_the_transport_check() {
        let mut plugins: HashMap<String, Value> =
            serde_json::from_value(config()["plugins"].clone()).unwrap();
        let before = plugins["updater"].clone();

        let secure = Url::parse("https://example.com/latest.json").unwrap();
        point_at(&mut plugins, &secure);
        assert_eq!(plugins["updater"]["endpoints"], json!([secure.as_str()]));
        assert!(
            plugins["updater"]
                .get("dangerousInsecureTransportProtocol")
                .is_none()
        );

        let local = Url::parse("http://127.0.0.1:8765/latest.json").unwrap();
        point_at(&mut plugins, &local);
        assert_eq!(plugins["updater"]["endpoints"], json!([local.as_str()]));
        assert_eq!(
            plugins["updater"]["dangerousInsecureTransportProtocol"],
            json!(true)
        );
        // A chave pública e o resto não mudam: o instalador de teste também precisa de assinatura.
        for kept in ["pubkey", "requireSignedVersion", "windows"] {
            assert_eq!(plugins["updater"][kept], before[kept], "{kept}");
        }
        // E o plugin aceita o resultado (em build de produção ele recusa http sem a opção).
        serde_json::from_value::<tauri_plugin_updater::Config>(plugins["updater"].clone()).unwrap();
    }

    #[test]
    fn placeholder_key_turns_the_updater_off() {
        assert!(usable_pubkey(SAMPLE_PUBKEY));
        for placeholder in [
            "",
            "TROQUE-PELA-CHAVE-PUBLICA-ANTES-DO-PRIMEIRO-RELEASE (desktop/rust-app/README.md)",
            "TROQUE",
            "dW50cnVzdGVk",
        ] {
            assert!(!usable_pubkey(placeholder), "{placeholder}");
        }
    }

    #[test]
    fn only_an_installed_app_updates_itself() {
        let folder = std::env::temp_dir().join(format!("nimbo-update-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let exe = folder.join("nimbo-app.exe");
        assert!(!installed(&exe));
        std::fs::write(folder.join("uninstall.exe"), b"").unwrap();
        assert!(installed(&exe));
        std::fs::remove_dir_all(&folder).unwrap();
        assert!(!installed(Path::new("nimbo-app.exe")));
    }

    #[test]
    fn messages_have_the_shape_the_bridge_reads() {
        fn json(value: &impl Serialize) -> String {
            serde_json::to_string(value).unwrap()
        }
        assert_eq!(
            json(&Event::Progress { percent: 42 }),
            r#"{"kind":"progress","percent":42}"#
        );
        assert_eq!(
            json(&Event::Ready {
                version: "0.1.1".into()
            }),
            r#"{"kind":"ready","version":"0.1.1"}"#
        );
        // O site lê `r.ok && r.value` e depois `r.value.version`.
        assert_eq!(
            json(&Reply::ok(Some(Info {
                version: "0.1.1".into()
            }))),
            r#"{"ok":true,"value":{"version":"0.1.1"}}"#
        );
        assert_eq!(
            json(&Reply::ok(None::<Info>)),
            r#"{"ok":true,"value":null}"#
        );
        assert_eq!(json(&Reply::ok(true)), r#"{"ok":true,"value":true}"#);
    }

    #[test]
    fn timestamps_are_utc_dates() {
        assert_eq!(timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(timestamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(timestamp(1_709_251_199), "2024-02-29T23:59:59Z");
        assert_eq!(timestamp(1_794_491_109), "2026-11-12T13:45:09Z");
    }

    #[test]
    fn app_replaces_the_electron_one_without_sharing_its_data() {
        let config = config();
        let electron: Value = serde_json::from_str(include_str!("../../../package.json")).unwrap();

        // Mesmo nome do app antigo (é o Nimbo, e os atalhos novos tomam o lugar dos antigos), mas
        // identificador e executável diferentes: dados, trava de instância única e pasta de
        // instalação não se misturam com os do Electron, que o instalador remove.
        assert_eq!(config["productName"], "Nimbo");
        assert_eq!(config["productName"], electron["build"]["productName"]);
        assert_eq!(config["identifier"], "app.nimbo.rust");
        assert_eq!(config["mainBinaryName"], "nimbo-app");
        assert_ne!(config["identifier"], electron["build"]["appId"]);
        let hooks = include_str!("../installer-hooks.nsh");
        assert!(hooks.contains("NSIS_HOOK_PREINSTALL"));
        assert!(hooks.contains(r"$LOCALAPPDATA\Programs\Nimbo\Nimbo.exe"));
        // O app novo não pode ter o nome de executável que o gancho encerra.
        assert!(hooks.contains("taskkill /F /T /IM Nimbo.exe"));
        assert_ne!(config["mainBinaryName"], "Nimbo");
        assert_eq!(
            config["bundle"]["windows"]["nsis"]["installerHooks"],
            "./installer-hooks.nsh"
        );

        // A versão vem só do Cargo.toml.
        assert!(config.get("version").is_none());

        // Instalador NSIS por usuário (sem pedir administrador), com o artefato de atualização.
        let bundle = &config["bundle"];
        assert_eq!(bundle["active"], true);
        assert_eq!(bundle["targets"], json!(["nsis"]));
        assert_eq!(bundle["createUpdaterArtifacts"], true);
        assert_eq!(bundle["windows"]["nsis"]["installMode"], "currentUser");
        assert_eq!(
            bundle["windows"]["webviewInstallMode"]["type"],
            "downloadBootstrapper"
        );

        // Um endereço só: o `latest.json` do Release mais recente (o app antigo lia `latest.yml`).
        let updater = &config["plugins"]["updater"];
        assert_eq!(updater["endpoints"], json!([FEED]));
        assert!(FEED.ends_with("/releases/latest/download/latest.json"));
        assert!(!FEED.ends_with("latest.yml"));
        assert_eq!(updater["requireSignedVersion"], true);
        assert_eq!(updater["windows"]["installMode"], "quiet");
        assert!(updater.get("dangerousInsecureTransportProtocol").is_none());
        // E o plugin aceita esta configuração como está.
        let parsed: tauri_plugin_updater::Config = serde_json::from_value(updater.clone()).unwrap();
        assert_eq!(parsed.endpoints.len(), 1);
    }

    #[test]
    fn updater_reads_the_manifest_the_pack_script_writes() {
        // Mesmo formato que scripts/pack.ps1 grava em latest.json.
        let manifest = json!({
            "version": "0.5.1",
            "notes": "Nimbo 0.5.1",
            "pub_date": "2026-11-12T13:45:09Z",
            "platforms": {
                "windows-x86_64": {
                    "signature": "c2lnbmF0dXJl",
                    "url": "https://github.com/RafCarrasco/Nimbo/releases/download/v0.5.1/Nimbo-Setup.exe"
                }
            }
        });
        let release: tauri_plugin_updater::RemoteRelease =
            serde_json::from_value(manifest).unwrap();
        assert_eq!(release.version.to_string(), "0.5.1");
        assert!(release.pub_date.is_some());
        assert!(
            release
                .download_url("windows-x86_64")
                .unwrap()
                .as_str()
                .ends_with("/v0.5.1/Nimbo-Setup.exe")
        );
        assert_eq!(release.signature("windows-x86_64").unwrap(), "c2lnbmF0dXJl");
        let script = include_str!("../scripts/pack.ps1");
        for field in [
            "version",
            "notes",
            "pub_date",
            "platforms",
            "'windows-x86_64'",
            "signature",
            "url",
        ] {
            assert!(script.contains(field), "{field}");
        }
    }

    #[test]
    fn commands_are_declared_guarded_and_granted_like_sign_in() {
        let build = include_str!("../build.rs");
        let main = include_str!("main.rs");
        let bridge = include_str!("bridge.js");
        let this = include_str!("update.rs");
        for (command, permission) in [
            ("watch_update", "allow-watch-update"),
            ("install_update", "allow-install-update"),
            ("check_update", "allow-check-update"),
        ] {
            assert!(build.contains(&format!("\"{command}\"")), "{command}");
            assert!(main.contains(&format!("update::{command}")), "{command}");
            assert!(main.contains(&format!(".permission(\"{permission}\")")));
            assert!(bridge.contains(&format!("invoke('{command}'")), "{command}");
            // O guarda é o primeiro argumento do comando.
            let (_, arguments) = this
                .split_once(&format!("pub async fn {command}("))
                .unwrap();
            assert!(
                arguments.trim_start().starts_with("_caller: FromApp,"),
                "{command}"
            );
        }
        // Nenhuma permissão do próprio Tauri (eventos, janela, o plugin de atualização) é dada.
        assert!(!main.contains(".permission(\"core:"));
        assert!(!main.contains(".permission(\"updater:"));
    }
}
