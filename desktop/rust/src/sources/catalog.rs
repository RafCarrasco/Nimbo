//! Host-owned descriptive catalog. No entry is a native capture capability.
use super::{NativeMonitor, NativeProcess, NativeWindow};
use serde_json::Value;
use std::collections::HashMap;
#[derive(Clone)]
pub struct Catalog {
    entries: Vec<Value>,
    diagnostics_body: String,
    identities: HashMap<String, Vec<String>>,
}
/// A metadata candidate, NOT permission to open a source. Revalidate against the OS before capture.
#[derive(Clone)]
pub struct Candidate {
    id: String,
    identity: Vec<String>,
}
impl Candidate {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn same_source(&self, other: &Self) -> bool {
        self.id == other.id && self.identity == other.identity
    }
}
pub trait MetadataProvider {
    type Error;
    fn monitors(&self) -> Result<Vec<NativeMonitor>, Self::Error>;
    fn windows(&self, own: &[u32]) -> Result<Vec<NativeWindow>, Self::Error>;
    fn processes(&self) -> Result<Vec<NativeProcess>, Self::Error>;
}
#[derive(Debug)]
pub enum CollectError<E> {
    Provider(E),
    Invalid(&'static str),
}
impl Catalog {
    /// Explicit local diagnostics only; never log this title-bearing text automatically.
    pub fn diagnostics(&self, windows_version: &str) -> String {
        format!(
            "Nimbo {} · Windows {}\nEnumeração: Win32/Rust\n{}",
            crate::policy::APP_VERSION,
            windows_version,
            self.diagnostics_body
        )
    }
    pub fn collect<P: MetadataProvider>(
        provider: &P,
        own: &[u32],
    ) -> Result<Self, CollectError<P::Error>> {
        let m = provider.monitors().map_err(CollectError::Provider)?;
        let w = provider.windows(own).map_err(CollectError::Provider)?;
        let p = provider.processes().map_err(CollectError::Provider)?;
        Self::from_native(&m, &w, &p, own).map_err(CollectError::Invalid)
    }
    pub fn resolve(&self, id: &str) -> Option<Candidate> {
        self.identities.get(id).map(|identity| Candidate {
            id: id.into(),
            identity: identity.clone(),
        })
    }
    pub fn from_native(
        monitors: &[NativeMonitor],
        windows: &[NativeWindow],
        processes: &[NativeProcess],
        own: &[u32],
    ) -> Result<Self, &'static str> {
        let mut entries:Vec<Value>=monitors.iter().enumerate().map(|(n,m)|serde_json::json!({"id":format!("screen:{}:0",m.handle),"name":if monitors.len()==1 {"Tela inteira".to_owned()} else {format!("Tela {}",n+1)},"type":"screen","thumbnail":"","icon":""})).collect();
        entries.extend(
            super::missing_sources(&[], windows, processes, own, |_| String::new())
                .iter()
                .map(|s| s.to_json()),
        );
        let mut identities = HashMap::new();
        for row in &entries {
            let id = row["id"].as_str().ok_or("invalid source metadata")?;
            let identity = if let Some(m) = monitors
                .iter()
                .find(|m| format!("screen:{}:0", m.handle) == id)
            {
                vec!["monitor".into(), m.device_name.clone()]
            } else if let Some(w) = windows
                .iter()
                .find(|w| format!("window:{}:0", w.hwnd) == id)
            {
                vec![
                    "window".into(),
                    w.pid.to_string(),
                    w.path.clone(),
                    w.class_name.clone(),
                ]
            } else if let Some(p) = processes.iter().find(|p| format!("game:{}", p.pid) == id) {
                vec!["process".into(), p.name.clone()]
            } else {
                return Err("source identity unavailable");
            };
            if identities.insert(id.to_owned(), identity).is_some() {
                return Err("duplicate source identity");
            }
        }
        let windows: Vec<_> = windows.iter().filter(|w| !own.contains(&w.pid)).collect();
        let relevant: Vec<_> = windows
            .iter()
            .copied()
            .filter(|w| w.visible || super::is_game_like(w) || w.minimized)
            .collect();
        let games: Vec<_> = processes
            .iter()
            .filter(|p| !own.contains(&p.pid) && super::game_name(&p.name).is_some())
            .map(|p| format!("{} ({})", p.name, p.pid))
            .collect();
        let mut lines = vec![
            format!(
                "Jogos em execução: {}",
                if games.is_empty() {
                    "nenhum".into()
                } else {
                    games.join(", ")
                }
            ),
            format!("Janelas: {} (mostrando {})", windows.len(), relevant.len()),
        ];
        for w in relevant {
            let flags: Vec<_> = [
                ("visible", w.visible),
                ("owned", w.owned),
                ("tool", w.tool),
                ("cloaked", w.cloaked),
                ("minimized", w.minimized),
                ("fullscreen", w.fullscreen),
            ]
            .into_iter()
            .filter_map(|(name, yes)| yes.then_some(name))
            .collect();
            let basename = if w.path.is_empty() {
                "?"
            } else {
                w.path.rsplit(['\\', '/']).next().unwrap_or("?")
            };
            lines.push(format!(
                "- {} | {} | {} | {}{}",
                w.title,
                basename,
                w.class_name,
                if flags.is_empty() {
                    "-".into()
                } else {
                    flags.join(",")
                },
                if super::is_game_like(w) {
                    " | JOGO"
                } else {
                    ""
                }
            ));
        }
        Ok(Self {
            entries,
            identities,
            diagnostics_body: lines.join("\n"),
        })
    }
    pub fn list(&self) -> Value {
        Value::Array(self.entries.clone())
    }
}
