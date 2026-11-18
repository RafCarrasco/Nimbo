//! Para qual Firebase o app fala: o de verdade ou os emuladores locais (`npm run emulators`).
//! Os valores são os mesmos de `src/firebase.ts`; a chave de API da web é pública por natureza.

#[derive(Clone, Debug)]
pub struct Project {
    pub api_key: String,
    /// Endereço do banco, sem esquema: `host` ou `host:porta`.
    pub db_host: String,
    pub db_namespace: String,
    /// `true` para wss/https.
    pub db_secure: bool,
    /// Base das chamadas de login, até antes de `/v1/...`.
    pub identity_base: String,
    pub token_base: String,
    pub emulator: bool,
}

impl Project {
    pub fn production() -> Self {
        Self {
            api_key: "AIzaSyDMW8WLXjug0v9QSes9a3pwDqa6MUqYDc0".into(),
            db_host: "golive-ad118-default-rtdb.firebaseio.com".into(),
            db_namespace: "golive-ad118-default-rtdb".into(),
            db_secure: true,
            identity_base: "https://identitytoolkit.googleapis.com".into(),
            token_base: "https://securetoken.googleapis.com".into(),
            emulator: false,
        }
    }

    /// Emuladores locais, nas portas de `firebase.json` (banco 9000, login 9099). Outro par de
    /// emuladores (para dois testes não dividirem o mesmo banco) entra por `NIMBO_EMULATOR_DB` e
    /// `NIMBO_EMULATOR_AUTH`, no formato `host:porta`.
    pub fn emulator() -> Self {
        let address = |name: &str, default: &str| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| default.to_owned())
        };
        let (db, auth) = (
            address("NIMBO_EMULATOR_DB", "127.0.0.1:9000"),
            address("NIMBO_EMULATOR_AUTH", "127.0.0.1:9099"),
        );
        Self {
            api_key: "demo-key".into(),
            db_host: db,
            db_namespace: "demo-nimbo-default-rtdb".into(),
            db_secure: false,
            identity_base: format!("http://{auth}/identitytoolkit.googleapis.com"),
            token_base: format!("http://{auth}/securetoken.googleapis.com"),
            emulator: true,
        }
    }
}
