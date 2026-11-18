//! Login no Firebase pela API REST: troca a credencial do Google pelo token da conta e renova esse
//! token antes de vencer. É o que `signInWithCredential` faz no site.
//!
//! Nada aqui guarda senha: o app recebe do navegador só a credencial do Google (como hoje) e fica
//! com o token de renovação do Firebase, que quem chama decide onde guardar.

use std::time::{Duration, Instant};

use serde_json::json;

use crate::Json;
use crate::config::Project;

/// Renova quando faltar menos que isto para o token vencer (ele dura 1 hora).
const REFRESH_BEFORE: Duration = Duration::from_secs(300);

#[derive(Clone, Debug)]
pub struct Session {
    pub uid: String,
    pub email: String,
    pub id_token: String,
    pub refresh_token: String,
    pub expires_at: Instant,
}

impl Session {
    pub fn needs_refresh(&self) -> bool {
        Instant::now() + REFRESH_BEFORE >= self.expires_at
    }
}

#[derive(Debug)]
pub enum AuthError {
    /// Sem resposta do servidor de login (rede).
    Network(String),
    /// O servidor recusou: credencial inválida, conta desativada, token de renovação revogado.
    Rejected(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Network(text) => write!(f, "sem conexão com o login: {text}"),
            AuthError::Rejected(text) => write!(f, "login recusado: {text}"),
        }
    }
}
impl std::error::Error for AuthError {}

/// Credencial de uma conta Google fictícia do emulador, igual à de `?devUser=nome` no site: o
/// mesmo nome cai na mesma conta nos dois.
pub fn dev_credential(name: &str) -> String {
    json!({ "sub": format!("dev-{name}"), "email": format!("{name}@nimbo.test"), "email_verified": true })
        .to_string()
}

/// Entra com a credencial do Google (`id_token` e, se houver, `access_token`).
pub fn sign_in_google(
    project: &Project,
    id_token: &str,
    access_token: &str,
) -> Result<Session, AuthError> {
    let mut post = format!("id_token={}&providerId=google.com", encode(id_token));
    if !access_token.is_empty() {
        post.push_str(&format!("&access_token={}", encode(access_token)));
    }
    let url = format!(
        "{}/v1/accounts:signInWithIdp?key={}",
        project.identity_base, project.api_key
    );
    let body = json!({
        "postBody": post,
        "requestUri": "http://localhost",
        "returnSecureToken": true,
        "returnIdpCredential": true,
    });
    let reply = call(|agent| agent.post(&url).send_json(&body))?;
    session(
        &reply,
        "localId",
        "idToken",
        "refreshToken",
        "expiresIn",
        reply["email"].as_str().unwrap_or(""),
    )
}

/// Troca o token de renovação por um token novo da mesma conta.
pub fn refresh(project: &Project, old: &Session) -> Result<Session, AuthError> {
    let url = format!("{}/v1/token?key={}", project.token_base, project.api_key);
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", old.refresh_token.as_str()),
    ];
    let reply = call(|agent| agent.post(&url).send_form(form))?;
    session(
        &reply,
        "user_id",
        "id_token",
        "refresh_token",
        "expires_in",
        &old.email,
    )
}

/// Volta a entrar com um token de renovação guardado de uma abertura anterior.
pub fn resume(project: &Project, refresh_token: &str, email: &str) -> Result<Session, AuthError> {
    let saved = Session {
        uid: String::new(),
        email: email.to_owned(),
        id_token: String::new(),
        refresh_token: refresh_token.to_owned(),
        expires_at: Instant::now(),
    };
    refresh(project, &saved)
}

fn session(
    reply: &Json,
    uid: &str,
    token: &str,
    refresh: &str,
    expires: &str,
    email: &str,
) -> Result<Session, AuthError> {
    let text = |key: &str| {
        reply[key]
            .as_str()
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let (Some(uid), Some(id_token), Some(refresh_token)) = (text(uid), text(token), text(refresh))
    else {
        return Err(AuthError::Rejected("resposta sem a conta".into()));
    };
    let seconds = reply[expires]
        .as_str()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600u64);
    Ok(Session {
        uid,
        email: email.to_owned(),
        id_token,
        refresh_token,
        expires_at: Instant::now() + Duration::from_secs(seconds),
    })
}

fn call(
    send: impl FnOnce(&ureq::Agent) -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<Json, AuthError> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        // Erro do servidor vem com explicação no corpo: lemos em vez de receber só o código.
        .http_status_as_error(false)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .build(),
        )
        .build()
        .into();
    let mut response = send(&agent).map_err(|e| AuthError::Network(e.to_string()))?;
    let status = response.status();
    let reply: Json = response
        .body_mut()
        .read_json()
        .map_err(|e| AuthError::Network(e.to_string()))?;
    if status.is_success() {
        return Ok(reply);
    }
    let message = reply["error"]["message"]
        .as_str()
        .unwrap_or("erro desconhecido");
    Err(AuthError::Rejected(format!(
        "{message} ({})",
        status.as_u16()
    )))
}

/// Codifica um valor para ir dentro de `chave=valor&...`.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_escapes_reserved_characters() {
        assert_eq!(encode("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
        assert_eq!(encode("Az09-_.~"), "Az09-_.~");
    }

    #[test]
    fn dev_credential_matches_the_site() {
        let credential: Json = serde_json::from_str(&dev_credential("ana")).unwrap();
        assert_eq!(credential["sub"], "dev-ana");
        assert_eq!(credential["email"], "ana@nimbo.test");
        assert_eq!(credential["email_verified"], true);
    }
}
