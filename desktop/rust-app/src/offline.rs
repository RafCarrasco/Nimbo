//! Tela do próprio Nimbo para quando o site não abre (sem internet, site fora do ar).
//!
//! A interface do app é o site dentro do WebView2. Sem isto, quando o site não carrega, a janela
//! mostra a página de erro do Edge ("Hmmm... can't reach this page"). Aqui o app desliga essa
//! página, percebe a falha e mostra uma tela dele, servida de um endereço interno
//! (`http://nimbo-offline.localhost/`), que volta ao Nimbo sozinha ou pelo botão.
//!
//! A tela não é o Nimbo: tem outra origem. Por isso não recebe a ponte `nimboDesktop` e não passa
//! na permissão dos comandos (ver `bridge.rs`). Tudo o que ela sabe fazer é abrir o endereço do app.

use std::borrow::Cow;
use std::cell::Cell;
use std::rc::Rc;

use tauri::http::{Request, Response, StatusCode, header};
use tauri::{Manager, Runtime, UriSchemeContext, WebviewWindow};
use url::{Position, Url};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED, ICoreWebView2,
};
use webview2_com::{
    ContentLoadingEventHandler, NavigationCompletedEventHandler, ProcessFailedEventHandler,
    take_pwstr,
};
use windows::core::{BOOL, HSTRING, PWSTR};

use crate::bridge::Shell;

/// Nome do protocolo do app que serve a tela. No Windows o endereço vira `http://<nome>.localhost`.
pub const SCHEME: &str = "nimbo-offline";
/// Endereço da tela dentro da janela.
const PAGE_URL: &str = "http://nimbo-offline.localhost/";
/// A mesma tela com o texto de "a página travou" (o `offline.html` lê o que vem depois do `#`).
const CRASHED_URL: &str = "http://nimbo-offline.localhost/#falha";

const PAGE: &str = include_str!("offline.html");
const ICON: &[u8] = include_bytes!("../../../public/nimbo-icon-256.png");

/// A navegação é para a tela sem conexão? Só o endereço exato, com ou sem o motivo depois do `#`.
pub fn is_page(url: &Url) -> bool {
    url[..Position::AfterQuery] == *PAGE_URL
}

/// Responde aos pedidos do protocolo: a tela e o ícone dela, mais nada.
pub fn serve<R: Runtime>(
    context: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
) -> Response<Cow<'static, [u8]>> {
    let shell = context.app_handle().state::<Shell>();
    respond(request.uri().host(), request.uri().path(), &shell.app_url)
}

fn respond(host: Option<&str>, path: &str, app_url: &Url) -> Response<Cow<'static, [u8]>> {
    let (kind, body): (&str, Cow<'static, [u8]>) = match (host, path) {
        (Some("localhost"), "/") => (
            "text/html; charset=utf-8",
            page(app_url).into_bytes().into(),
        ),
        (Some("localhost"), "/icone.png") => ("image/png", ICON.into()),
        _ => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Cow::Borrowed(&[][..]))
                .expect("resposta só com o código sempre monta");
        }
    };
    // A tela só carrega o que é dela e só fala com o endereço do app, para saber se ele voltou.
    let policy = format!(
        "default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; \
         connect-src {}; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        app_url.origin().ascii_serialization()
    );
    Response::builder()
        .header(header::CONTENT_TYPE, kind)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONTENT_SECURITY_POLICY, policy)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(body)
        .expect("cabeçalhos fixos e uma origem sempre montam a resposta")
}

/// A tela, com o endereço do app no lugar do marcador.
fn page(app_url: &Url) -> String {
    // `<` escapado: o texto vai dentro de um <script> e não pode fechar a tag.
    let app_url = serde_json::to_string(app_url.as_str())
        .expect("uma string sempre vira JSON")
        .replace('<', "\\u003c");
    PAGE.replace("__NIMBO_APP_URL__", &app_url)
}

/// Liga a troca da página de erro do navegador pela tela do Nimbo. Falhar aqui só mantém a página
/// de erro do Edge; o app segue funcionando.
pub fn watch(window: &WebviewWindow) {
    let attached = window.with_webview(|webview| {
        // SAFETY: chamada COM no navegador embutido, na thread da interface (é onde o Tauri roda
        // este bloco); as interfaces são contadas por referência pelos próprios tipos.
        let result = unsafe { webview.controller().CoreWebView2() }.and_then(|core| attach(&core));
        if let Err(e) = result {
            eprintln!("[nimbo] tela sem conexão indisponível: {e}");
        }
    });
    if let Err(e) = attached {
        eprintln!("[nimbo] tela sem conexão indisponível: {e}");
    }
}

fn attach(core: &ICoreWebView2) -> windows::core::Result<()> {
    // Navegação que terminou na página de erro do navegador (sem rede, site fora do ar...). O
    // código de erro não serve para decidir: conexão recusada chega como "desconhecido". Navegação
    // cancelada (link para fora, barrado em main.rs) ou que virou download não carrega documento
    // novo e não passa por aqui.
    let failed = Rc::new(Cell::new(None::<u64>));
    let mut token = 0;

    // SAFETY (o bloco todo): chamadas COM na thread da interface, como em `watch`; os tratadores
    // também rodam nela, e cada ponteiro de saída aponta para uma variável local viva.
    unsafe {
        core.add_ContentLoading(
            &ContentLoadingEventHandler::create(Box::new({
                let failed = failed.clone();
                move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut is_error = BOOL::default();
                    args.IsErrorPage(&mut is_error)?;
                    let mut navigation = 0;
                    args.NavigationId(&mut navigation)?;
                    failed.set(is_error.as_bool().then_some(navigation));
                    Ok(())
                }
            })),
            &mut token,
        )?;
        // A troca espera a navegação que falhou terminar, para não atropelar o navegador.
        core.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(Box::new(move |webview, args| {
                let (Some(webview), Some(args)) = (webview, args) else {
                    return Ok(());
                };
                let mut navigation = 0;
                args.NavigationId(&mut navigation)?;
                if failed.get() != Some(navigation) {
                    return Ok(());
                }
                failed.set(None);
                // Se a própria tela falhasse, insistir nela viraria um laço.
                if source(&webview).starts_with(PAGE_URL) {
                    eprintln!("[nimbo] a tela sem conexão não abriu");
                    return Ok(());
                }
                webview.Navigate(&HSTRING::from(PAGE_URL))
            })),
            &mut token,
        )?;
        // Com a página de erro do navegador desligada, o processo da página morrer deixaria a
        // janela vazia. A mesma tela avisa e abre o Nimbo de novo.
        core.add_ProcessFailed(
            &ProcessFailedEventHandler::create(Box::new(|webview, args| {
                let (Some(webview), Some(args)) = (webview, args) else {
                    return Ok(());
                };
                let mut kind = Default::default();
                args.ProcessFailedKind(&mut kind)?;
                if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED {
                    webview.Navigate(&HSTRING::from(CRASHED_URL))?;
                }
                Ok(())
            })),
            &mut token,
        )?;
        // Por último: a página de erro do Edge só some se a troca acima ficou ligada.
        core.Settings()?.SetIsBuiltInErrorPageEnabled(false)
    }
}

/// Endereço do documento na janela (vazio se o navegador não informar). Numa página de erro é o
/// endereço que falhou.
fn source(webview: &ICoreWebView2) -> String {
    let mut uri = PWSTR::null();
    // SAFETY: chamada COM na thread da interface; `uri` recebe um texto alocado pelo navegador,
    // que `take_pwstr` copia e libera.
    match unsafe { webview.Source(&mut uri) } {
        Ok(()) => take_pwstr(uri),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SITE: &str = "https://golive-ad118.web.app/";

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn only_the_exact_address_is_the_offline_screen() {
        assert!(is_page(&url(PAGE_URL)));
        assert!(is_page(&url(CRASHED_URL)));
        for other in [
            "https://nimbo-offline.localhost/",
            "http://nimbo-offline.localhost/outra",
            "http://nimbo-offline.localhost/icone.png",
            "http://nimbo-offline.localhost/?x=1",
            "http://nimbo-offline.localhost:8080/",
            "http://user@nimbo-offline.localhost/",
            "http://nimbo-offline.localhost.evil.invalid/",
            "http://nimbo-offline.evil.invalid/",
            "nimbo-offline://localhost/",
            SITE,
            "about:blank",
        ] {
            assert!(!is_page(&url(other)), "{other}");
        }
    }

    #[test]
    fn addresses_match_the_protocol_name_and_the_page_script() {
        assert_eq!(PAGE_URL, format!("http://{SCHEME}.localhost/"));
        assert_eq!(CRASHED_URL, format!("{PAGE_URL}#falha"));
        assert!(PAGE.contains("location.hash === '#falha'"));
        assert!(PAGE.contains(r#"src="/icone.png""#));
    }

    #[test]
    fn serves_the_screen_and_its_icon_and_nothing_else() {
        let app = url(SITE);
        let value_of = |response: &Response<Cow<'static, [u8]>>, name: &str| {
            response.headers()[name].to_str().unwrap().to_owned()
        };

        let screen = respond(Some("localhost"), "/", &app);
        assert_eq!(screen.status(), StatusCode::OK);
        assert_eq!(
            value_of(&screen, "content-type"),
            "text/html; charset=utf-8"
        );
        let policy = value_of(&screen, "content-security-policy");
        assert!(policy.starts_with("default-src 'none';"), "{policy}");
        assert!(
            policy.contains(" connect-src https://golive-ad118.web.app;"),
            "{policy}"
        );
        let html = std::str::from_utf8(screen.body()).unwrap();
        for text in [
            "Sem conexão com o Nimbo",
            "Tentar de novo",
            "O Nimbo parou de responder",
            r#"const APP_URL = "https://golive-ad118.web.app/";"#,
            "const RETRY_MS = 5000;",
            "a cada 5 segundos",
        ] {
            assert!(html.contains(text), "{text}");
        }
        assert!(!html.contains("__NIMBO_APP_URL__"));
        // A tela não usa a ponte nem os comandos do app.
        assert!(!html.contains("nimboDesktop") && !html.contains("__TAURI"));

        let icon = respond(Some("localhost"), "/icone.png", &app);
        assert_eq!(icon.status(), StatusCode::OK);
        assert_eq!(value_of(&icon, "content-type"), "image/png");
        assert!(icon.body().starts_with(b"\x89PNG\r\n\x1a\n"));

        for (host, path) in [
            (Some("localhost"), "/outra"),
            (Some("localhost"), "/icone.png/"),
            (Some("evil.invalid"), "/"),
            (None, "/"),
        ] {
            let refused = respond(host, path, &app);
            assert_eq!(refused.status(), StatusCode::NOT_FOUND, "{host:?} {path}");
            assert!(refused.body().is_empty());
        }
    }

    #[test]
    fn test_address_goes_into_the_script_and_the_policy_whole() {
        let app = url("http://localhost:5173/?devUser=owner");
        let screen = respond(Some("localhost"), "/", &app);
        let policy = screen.headers()["content-security-policy"]
            .to_str()
            .unwrap();
        assert!(
            policy.contains(" connect-src http://localhost:5173;"),
            "{policy}"
        );
        let html = std::str::from_utf8(screen.body()).unwrap();
        assert!(html.contains(r#"const APP_URL = "http://localhost:5173/?devUser=owner";"#));
    }

    #[test]
    fn app_address_cannot_close_the_script_tag() {
        let html = page(&url(
            "http://localhost:5173/?a=</script><script>alert(1)//#</script>",
        ));
        assert_eq!(html.matches("</script").count(), 1);
        assert!(!html.contains("<script>alert"));
    }
}
