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

    /// Emuladores locais, nas portas de `firebase.json` (banco 9000, login 9099).
    pub fn emulator() -> Self {
        Self {
            api_key: "demo-key".into(),
            db_host: "127.0.0.1:9000".into(),
            db_namespace: "demo-nimbo-default-rtdb".into(),
            db_secure: false,
            identity_base: "http://127.0.0.1:9099/identitytoolkit.googleapis.com".into(),
            token_base: "http://127.0.0.1:9099/securetoken.googleapis.com".into(),
            emulator: true,
        }
    }
}
