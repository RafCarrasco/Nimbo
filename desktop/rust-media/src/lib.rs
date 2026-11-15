//! Motor de mídia do Nimbo em Rust: captura, codificação pela placa, áudio do jogo em Opus e
//! envio WebRTC. E o player (`viewer`): o lado de quem assiste.
//! Ver `docs/rust-app.md` para o plano e os números medidos.

mod audio;
mod decode;
pub mod engine;
pub mod hw;
mod playback;
pub mod viewer;

use std::time::Duration;

pub type AnyError = Box<dyn std::error::Error + Send + Sync>;

/// Tempo de CPU (usuário + núcleo) que a thread que chama já gastou. Serve para saber onde o tempo
/// de um processo vai (rede, decodificador, desenho).
pub fn thread_cpu() -> Duration {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: os quatro ponteiros apontam para variáveis locais válidas durante a chamada.
    let ok = unsafe {
        GetThreadTimes(
            GetCurrentThread(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    if ok.is_err() {
        return Duration::ZERO;
    }
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
}

/// Tempo de CPU (usuário + núcleo) que este processo já gastou.
pub fn process_cpu() -> Duration {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: os quatro ponteiros apontam para variáveis locais válidas durante a chamada.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    if ok.is_err() {
        return Duration::ZERO;
    }
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
}
