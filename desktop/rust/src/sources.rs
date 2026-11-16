//! Portable metadata rules; these DTOs do not authorize native capture or prove OS ownership.
use serde_json::{Value, json};
use std::collections::HashSet;

mod enumeration;
pub use enumeration::{DesktopRect, NativeMonitor};
#[cfg(windows)]
pub mod windows;

#[derive(Debug, Clone)]
pub struct NativeWindow {
    pub hwnd: String,
    pub pid: u32,
    pub title: String,
    pub path: String,
    pub class_name: String,
    pub visible: bool,
    pub owned: bool,
    pub tool: bool,
    pub cloaked: bool,
    pub minimized: bool,
    pub fullscreen: bool,
}

#[derive(Debug, Clone)]
pub struct NativeProcess {
    pub pid: u32,
    pub name: String,
}

pub struct PickerSource {
    id: String,
    name: String,
    icon: String,
    minimized: bool,
    process: bool,
}
impl PickerSource {
    pub fn to_json(&self) -> Value {
        let mut result = json!({"id": self.id, "name": self.name, "type": "window",
            "thumbnail": "", "icon": self.icon, "minimized": self.minimized});
        if self.process {
            result["process"] = json!(true);
        }
        result
    }
}

/// Executable basename only, with the same aliases/case/suffix rules as Electron.
pub fn game_name(exe: &str) -> Option<&'static str> {
    let lower = exe.to_lowercase();
    match lower.strip_suffix(".exe").unwrap_or(&lower) {
        "valorant-win64-shipping" | "valorant" => Some("VALORANT"),
        "cs2" => Some("Counter-Strike 2"),
        "csgo" => Some("Counter-Strike"),
        "league of legends" => Some("League of Legends"),
        "fortniteclient-win64-shipping" => Some("Fortnite"),
        "r5apex" | "r5apex_dx12" => Some("Apex Legends"),
        "gta5" | "gtav" => Some("GTA V"),
        "rocketleague" => Some("Rocket League"),
        "overwatch" => Some("Overwatch 2"),
        "dota2" => Some("Dota 2"),
        "eldenring" => Some("Elden Ring"),
        "javaw" => Some("Minecraft"),
        "robloxplayerbeta" => Some("Roblox"),
        "cod" => Some("Call of Duty"),
        "tslgame" => Some("PUBG"),
        "destiny2" => Some("Destiny 2"),
        "rainbowsix" => Some("Rainbow Six Siege"),
        "marvel-win64-shipping" => Some("Marvel Rivals"),
        "deadlock" | "project8" => Some("Deadlock"),
        _ => None,
    }
}

pub fn missing_sources(
    known: &[String],
    windows: &[NativeWindow],
    processes: &[NativeProcess],
    own_pids: &[u32],
    mut icon_for_path: impl FnMut(&str) -> String,
) -> Vec<PickerSource> {
    let seen: HashSet<&str> = known.iter().filter_map(|id| id.split(':').nth(1)).collect();
    let all: Vec<&NativeWindow> = windows
        .iter()
        .filter(|w| !own_pids.contains(&w.pid) && !seen.contains(w.hwnd.as_str()))
        .collect();
    let mut selected: Vec<&NativeWindow> = all
        .iter()
        .copied()
        .filter(|w| w.visible && !w.owned && !w.tool && !w.cloaked)
        .collect();
    let listed: HashSet<&str> = seen
        .iter()
        .copied()
        .chain(selected.iter().copied().map(|w| w.hwnd.as_str()))
        .collect();
    let listed_pids: HashSet<u32> = selected
        .iter()
        .map(|w| w.pid)
        .chain(
            windows
                .iter()
                .filter(|w| seen.contains(w.hwnd.as_str()))
                .map(|w| w.pid),
        )
        .collect();
    let score = |w: &NativeWindow| {
        u8::from(w.visible) * 4 + u8::from(!w.owned) * 2 + u8::from(!w.tool) + u8::from(w.minimized)
    };
    let mut best: Vec<&NativeWindow> = Vec::new();
    for w in all.iter().copied() {
        let game_like = game_name(w.path.rsplit(['\\', '/']).next().unwrap_or("")).is_some()
            || w.class_name.to_ascii_lowercase().contains("unrealwindow")
            || (w.fullscreen && w.visible && !w.cloaked && !w.tool);
        if listed.contains(w.hwnd.as_str())
            || listed_pids.contains(&w.pid)
            || !game_like
            || matches!(w.class_name.as_str(), "IME" | "MSCTFIME UI")
        {
            continue;
        }
        if let Some(index) = best.iter().position(|current| current.pid == w.pid) {
            if score(w) > score(best[index]) {
                best[index] = w;
            }
        } else {
            best.push(w);
        }
    }
    selected.extend(best);
    let shown: HashSet<u32> = listed_pids
        .iter()
        .copied()
        .chain(selected.iter().map(|w| w.pid))
        .collect();
    let mut by_game: Vec<(&str, &NativeProcess)> = Vec::new();
    for p in processes {
        let Some(name) = game_name(&p.name) else {
            continue;
        };
        if shown.contains(&p.pid) || own_pids.contains(&p.pid) {
            continue;
        }
        if let Some(index) = by_game.iter().position(|(current, _)| *current == name) {
            if p.name.to_ascii_lowercase().contains("shipping") {
                by_game[index] = (name, p);
            }
        } else {
            by_game.push((name, p));
        }
    }
    let mut sources: Vec<PickerSource> = selected
        .into_iter()
        .map(|w| {
            let name = if !w.title.is_empty() {
                w.title.clone()
            } else {
                let exe = w.path.rsplit(['\\', '/']).next().unwrap_or("");
                if let Some(game) = game_name(exe) {
                    game.to_owned()
                } else {
                    let plain = if exe.to_ascii_lowercase().ends_with(".exe") {
                        &exe[..exe.len() - 4]
                    } else {
                        exe
                    };
                    if plain.is_empty() {
                        "Janela sem título".to_owned()
                    } else {
                        plain.to_owned()
                    }
                }
            };
            PickerSource {
                id: format!("window:{}:0", w.hwnd),
                name,
                icon: if w.path.is_empty() {
                    String::new()
                } else {
                    icon_for_path(&w.path)
                },
                minimized: w.minimized,
                process: false,
            }
        })
        .collect();
    sources.extend(by_game.into_iter().map(|(name, p)| PickerSource {
        id: format!("game:{}", p.pid),
        name: name.to_owned(),
        icon: String::new(),
        minimized: false,
        process: true,
    }));
    sources
}
