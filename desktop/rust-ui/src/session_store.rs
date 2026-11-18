//! Sessão guardada entre aberturas: o token de renovação do Firebase da conta, cifrado pelo Windows
//! para o usuário atual (DPAPI). É o equivalente ao que o navegador embutido guarda no perfil dele.
//! Nenhuma senha passa por aqui: o login do Google acontece no navegador do sistema.

use std::path::PathBuf;

use serde_json::json;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

pub struct Saved {
    pub refresh_token: String,
    pub email: String,
}

/// Uma pasta por ambiente: a sessão dos emuladores não se mistura com a de verdade.
fn file(emulator: bool) -> Option<PathBuf> {
    let base = std::env::var_os("NIMBO_NATIVE_DATA").or_else(|| std::env::var_os("APPDATA"))?;
    Some(PathBuf::from(base).join("Nimbo Nativo").join(if emulator {
        "sessao-emulador.bin"
    } else {
        "sessao.bin"
    }))
}

pub fn save(emulator: bool, session: &Saved) {
    let Some(path) = file(emulator) else { return };
    let plain = json!({ "refresh": session.refresh_token, "email": session.email }).to_string();
    let Some(sealed) = seal(plain.as_bytes(), true) else {
        return;
    };
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    let _ = std::fs::write(path, sealed);
}

pub fn load(emulator: bool) -> Option<Saved> {
    let sealed = std::fs::read(file(emulator)?).ok()?;
    let plain = seal(&sealed, false)?;
    let value: serde_json::Value = serde_json::from_slice(&plain).ok()?;
    Some(Saved {
        refresh_token: value["refresh"]
            .as_str()
            .filter(|token| !token.is_empty())?
            .to_owned(),
        email: value["email"].as_str().unwrap_or("").to_owned(),
    })
}

pub fn clear(emulator: bool) {
    if let Some(path) = file(emulator) {
        let _ = std::fs::remove_file(path);
    }
}

/// Cifra (`protect`) ou decifra com a chave do usuário do Windows. Só esta conta, nesta máquina,
/// consegue ler de volta.
fn seal(data: &[u8], protect: bool) -> Option<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len()).ok()?,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: `input` aponta para `data`, que vive até o fim da chamada e não é alterado por ela;
    // `output` é preenchido pelo Windows com um bloco que copiamos e devolvemos com LocalFree.
    unsafe {
        let done = if protect {
            CryptProtectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        done.ok()?;
        if output.pbData.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Some(bytes)
    }
}
