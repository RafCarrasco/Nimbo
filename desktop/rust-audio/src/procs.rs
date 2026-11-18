//! Quem pode ir para a live no modo "som do computador": a árvore de processos do Windows e as
//! regras de exclusão (o próprio Nimbo e os apps de chamada, com os filhos).
//!
//! O fluxo "só este programa" do Windows pega o programa e todos os descendentes dele. Então um
//! programa só pode entrar se nem ele nem nenhum descendente for excluído: abrir o fluxo do
//! `explorer.exe`, pai de quase tudo, levaria junto o Discord e o próprio Nimbo.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::calls::is_call_app;
use crate::{Error, win};

/// Mais que isso de ancestrais é laço (PID reaproveitado) ou árvore absurda: para de subir.
const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone)]
pub(crate) struct Proc {
    /// Quem criou o processo. Pode já ter fechado, e o PID pode ter sido reaproveitado.
    pub parent: u32,
    /// Nome do executável (`Discord.exe`).
    pub name: String,
    /// Quando o processo nasceu (FILETIME); `None` se o Windows não deixou ver. Só é lido quando
    /// alguém pergunta (abrir 200 processos a cada foto custaria mais de 1 ms).
    born: OnceCell<Option<u64>>,
}

/// Uma foto dos processos que existem agora.
#[derive(Default)]
pub(crate) struct Procs(HashMap<u32, Proc>);

struct Snapshot(HANDLE);

impl Drop for Snapshot {
    fn drop(&mut self) {
        // SAFETY: o handle veio do CreateToolhelp32Snapshot e é fechado uma vez, aqui.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Quando o processo nasceu. Processos protegidos (alguns antitrapaça, serviços do sistema) não
/// deixam ver: `None`.
fn created(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    // SAFETY: só consulta; o handle é fechado logo abaixo.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut times = [FILETIME::default(); 4];
    let [creation, exit, kernel, user] = &mut times;
    // SAFETY: handle válido e ponteiros para variáveis locais.
    let ok = unsafe { GetProcessTimes(handle, creation, exit, kernel, user) }.is_ok();
    // SAFETY: handle aberto acima, fechado uma vez.
    let _ = unsafe { CloseHandle(handle) };
    ok.then(|| (u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime))
}

impl Procs {
    pub fn snapshot() -> Result<Self, Error> {
        // SAFETY: chamada simples; o handle é fechado pelo `Snapshot`.
        let snapshot = Snapshot(
            unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
                .map_err(win("CreateToolhelp32Snapshot"))?,
        );
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut list = HashMap::new();
        // SAFETY: handle válido e `entry` com o tamanho preenchido, como a API pede.
        let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_ok();
        while more {
            let name = &entry.szExeFile;
            let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
            list.insert(
                entry.th32ProcessID,
                Proc {
                    parent: entry.th32ParentProcessID,
                    name: String::from_utf16_lossy(&name[..len]),
                    born: OnceCell::new(),
                },
            );
            // SAFETY: idem.
            more = unsafe { Process32NextW(snapshot.0, &mut entry) }.is_ok();
        }
        Ok(Self(list))
    }

    #[cfg(test)]
    pub fn from_list(list: impl IntoIterator<Item = (u32, Proc)>) -> Self {
        Self(list.into_iter().collect())
    }

    pub fn contains(&self, pid: u32) -> bool {
        self.0.contains_key(&pid)
    }

    /// Quando o processo nasceu; `None` se ele não está na foto ou o Windows não deixa ver.
    pub fn created(&self, pid: u32) -> Option<u64> {
        *self.0.get(&pid)?.born.get_or_init(|| created(pid))
    }

    /// O pai, se ele ainda existe e é mesmo o pai: um PID reaproveitado por um processo que
    /// nasceu depois do filho não conta.
    fn parent(&self, pid: u32) -> Option<u32> {
        let parent = self.0.get(&pid)?.parent;
        if parent == 0 || parent == pid || !self.0.contains_key(&parent) {
            return None;
        }
        match (self.created(parent), self.created(pid)) {
            (Some(born), Some(child_born)) if born > child_born => None,
            _ => Some(parent),
        }
    }

    /// Pai, avô... até a raiz.
    pub fn ancestors(&self, pid: u32) -> impl Iterator<Item = u32> + '_ {
        let mut current = pid;
        (0..MAX_DEPTH).map_while(move |_| {
            current = self.parent(current)?;
            (current != pid).then_some(current)
        })
    }
}

/// Quem não pode ir para a live.
pub(crate) struct Rules {
    /// O próprio Nimbo, os apps de chamada e todos os descendentes deles.
    blocked: HashSet<u32>,
    /// Ancestrais de quem está bloqueado: o fluxo deles levaria junto o som bloqueado.
    above_blocked: HashSet<u32>,
}

impl Rules {
    pub fn new(procs: &Procs, own: u32) -> Self {
        // De cima para baixo: o Nimbo e os apps de chamada, depois os filhos de cada um.
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for (&pid, proc) in &procs.0 {
            children.entry(proc.parent).or_default().push(pid);
        }
        let mut pending: Vec<u32> = procs
            .0
            .iter()
            .filter(|(_, proc)| is_call_app(&proc.name))
            .map(|(&pid, _)| pid)
            .collect();
        pending.push(own);
        let mut blocked = HashSet::new();
        while let Some(pid) = pending.pop() {
            if !blocked.insert(pid) {
                continue;
            }
            let kids = children.get(&pid).into_iter().flatten();
            pending.extend(kids.filter(|&&kid| procs.parent(kid) == Some(pid)));
        }
        let above_blocked = blocked
            .iter()
            .flat_map(|&pid| procs.ancestors(pid))
            .collect();
        Self {
            blocked,
            above_blocked,
        }
    }

    pub fn allowed(&self, pid: u32) -> bool {
        !self.blocked.contains(&pid) && !self.above_blocked.contains(&pid)
    }
}

/// Dos `pids`, só os que não descendem de outro da lista: o fluxo do ancestral já pega o som do
/// descendente, e abrir os dois dobraria esse som. A ordem é mantida.
pub(crate) fn roots(procs: &Procs, pids: &[u32]) -> Vec<u32> {
    let set: HashSet<u32> = pids.iter().copied().collect();
    pids.iter()
        .copied()
        .filter(|&pid| !procs.ancestors(pid).any(|up| set.contains(&up)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, parent: u32, name: &str, created: u64) -> (u32, Proc) {
        (
            pid,
            Proc {
                parent,
                name: name.into(),
                born: OnceCell::from(Some(created)),
            },
        )
    }

    fn forget_birth(procs: &mut Procs, pid: u32) {
        procs.0.get_mut(&pid).unwrap().born = OnceCell::from(None);
    }

    /// explorer(10) ─┬─ nimbo(20) ── msedgewebview2(21) ── msedgewebview2(22)
    ///               ├─ Discord(30) ── Discord(31)
    ///               ├─ ms-teams(40) ── msedgewebview2(41)
    ///               ├─ steam(50) ── game(51)
    ///               ├─ chrome(60) ── chrome(61)
    ///               └─ launcher(70) ── Discord(71)
    fn machine() -> Procs {
        Procs::from_list([
            proc(10, 4, "explorer.exe", 100),
            proc(20, 10, "nimbo-app.exe", 200),
            proc(21, 20, "msedgewebview2.exe", 201),
            proc(22, 21, "msedgewebview2.exe", 202),
            proc(30, 10, "Discord.exe", 300),
            proc(31, 30, "Discord.exe", 301),
            proc(40, 10, "ms-teams.exe", 400),
            proc(41, 40, "msedgewebview2.exe", 401),
            proc(50, 10, "steam.exe", 500),
            proc(51, 50, "game.exe", 501),
            proc(60, 10, "chrome.exe", 600),
            proc(61, 60, "chrome.exe", 601),
            proc(70, 10, "launcher.exe", 700),
            proc(71, 70, "Discord.exe", 701),
        ])
    }

    #[test]
    fn nimbo_calls_and_their_children_are_blocked() {
        let procs = machine();
        let rules = Rules::new(&procs, 20);
        for pid in [20, 21, 22, 30, 31, 40, 41, 71] {
            assert!(!rules.allowed(pid), "{pid}");
        }
        for pid in [50, 51, 60, 61] {
            assert!(rules.allowed(pid), "{pid}");
        }
    }

    #[test]
    fn whoever_has_a_blocked_descendant_is_blocked_too() {
        let procs = machine();
        let rules = Rules::new(&procs, 20);
        // O fluxo do explorer levaria o Nimbo e o Discord; o do launcher, o Discord filho dele.
        assert!(!rules.allowed(10));
        assert!(!rules.allowed(70));
    }

    #[test]
    fn a_reused_pid_is_not_a_parent() {
        // O pai do Discord (o atualizador, 90) fechou e o PID 90 foi reaproveitado por um jogo que
        // nasceu depois: o jogo não é pai do Discord e pode entrar.
        let procs = Procs::from_list([
            proc(10, 4, "explorer.exe", 100),
            proc(90, 10, "game.exe", 900),
            proc(30, 90, "Discord.exe", 300),
        ]);
        let rules = Rules::new(&procs, 1);
        assert!(rules.allowed(90));
        assert_eq!(procs.ancestors(30).count(), 0);
    }

    #[test]
    fn unknown_birth_keeps_the_link() {
        // Sem a hora de nascimento não dá para provar que o pai é falso: na dúvida, bloqueia.
        let mut procs = Procs::from_list([
            proc(10, 4, "explorer.exe", 100),
            proc(30, 10, "Discord.exe", 50),
        ]);
        forget_birth(&mut procs, 10);
        assert!(!Rules::new(&procs, 1).allowed(10));
    }

    #[test]
    fn loops_end() {
        let mut procs = Procs::from_list([proc(1, 2, "a.exe", 0), proc(2, 1, "b.exe", 0)]);
        forget_birth(&mut procs, 1);
        assert!(procs.ancestors(1).count() <= MAX_DEPTH);
        assert!(!Rules::new(&procs, 1).allowed(2));
    }

    #[test]
    fn descendants_of_a_chosen_program_are_dropped() {
        let procs = machine();
        assert_eq!(roots(&procs, &[61, 51, 60]), [51, 60]);
        assert_eq!(roots(&procs, &[51]), [51]);
        // Um processo que não existe mais não tem ancestrais: fica.
        assert_eq!(roots(&procs, &[999, 61]), [999, 61]);
    }
}
