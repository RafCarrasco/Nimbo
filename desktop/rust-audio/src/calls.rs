//! Apps de chamada de voz. No modo "som do computador" ([`crate::Mode::SystemExcept`]) o som deles
//! nunca vai para a live: quem está na chamada com quem transmite não pode se ouvir na live (o
//! Discord faz o mesmo ao transmitir a tela inteira).
//!
//! Para incluir um app, acrescente o nome do executável sem o `.exe` (maiúsculas e minúsculas não
//! importam). Os processos filhos do app também ficam de fora: o Teams novo, por exemplo, toca a
//! chamada num `msedgewebview2.exe` filho do `ms-teams.exe`.
//!
//! Não cobre chamada feita pelo navegador (Discord pelo site, Google Meet): o som sai do processo
//! de áudio do navegador, junto com o de tudo mais que toca nele, e tirar um tiraria o outro.

/// Executáveis (sem `.exe`) dos apps de chamada de voz.
pub const CALL_APPS: &[&str] = &[
    // Discord e as versões de teste dele.
    "Discord",
    "DiscordPTB",
    "DiscordCanary",
    // TeamSpeak 5 e TeamSpeak 3 (64 e 32 bits).
    "TeamSpeak",
    "ts3client_win64",
    "ts3client_win32",
    "Mumble",
    "Skype",
    // Microsoft Teams clássico e o novo.
    "Teams",
    "ms-teams",
    "Zoom",
    "Slack",
    // WhatsApp; o app novo para Windows roda como `WhatsApp.Root.exe`.
    "WhatsApp",
    "WhatsApp.Root",
    "Telegram",
    "Guilded",
    "Element",
];

/// `name` é o nome do executável, com ou sem `.exe`.
pub fn is_call_app(name: &str) -> bool {
    let stem = match name
        .len()
        .checked_sub(4)
        .and_then(|at| name.split_at_checked(at))
    {
        Some((stem, ext)) if ext.eq_ignore_ascii_case(".exe") => stem,
        _ => name,
    };
    CALL_APPS.iter().any(|app| app.eq_ignore_ascii_case(stem))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_executable_name_in_any_case() {
        assert!(is_call_app("Discord.exe"));
        assert!(is_call_app("discord.EXE"));
        assert!(is_call_app("DISCORDCANARY"));
        assert!(is_call_app("ms-teams.exe"));
        assert!(is_call_app("ts3client_win64.exe"));
        assert!(is_call_app("WhatsApp.Root.exe"));
    }

    #[test]
    fn other_programs_are_not_calls() {
        assert!(!is_call_app("DiscordGame.exe"));
        assert!(!is_call_app("msedgewebview2.exe"));
        assert!(!is_call_app("game.exe"));
        assert!(!is_call_app(".exe"));
        assert!(!is_call_app(""));
        assert!(!is_call_app("ção"));
    }
}
