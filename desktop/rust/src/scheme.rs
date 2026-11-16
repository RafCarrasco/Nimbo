//! Conservative per-user URI-scheme registration (CP2 S3, plan §3.5). A PORTABLE model over an
//! injected bounded registry snapshot and file probes decides; the Windows adapter (`native`)
//! only reads/writes `HKCU\Software\Classes\<scheme>`. Ownership recognition is deliberately
//! narrower than Windows command resolution: only a fully quoted absolute local executable is
//! understood; every bare, delegated, expanded, unknown or uncertain form is Foreign => Leave.
//! Read-then-write rechecks reduce races; they are NOT an atomic registry compare-and-swap.
use crate::login::Scheme;
use std::collections::{BTreeMap, BTreeSet};

pub const REG_SZ: u32 = 1;
pub const REG_EXPAND_SZ: u32 = 2;
/// Longest command value ever examined (bytes) and generated (UTF-16 units).
pub const MAX_VALUE_BYTES: usize = 4096;
pub const MAX_COMMAND_UNITS: usize = 1024;
pub const DISPLAY_RELEASE: &str = "URL:Nimbo";
pub const DISPLAY_TEST_HOST: &str = "URL:Nimbo Test Host";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandError;
/// A registry or file probe/write that failed or could not be decided (=> never a write).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryError;

/// Absolute local drive path `X:\…` or `\\?\X:\…`; no `%` (never expanded), no UNC, no quote,
/// no control character, no trailing backslash. Unicode accepted.
pub fn supported_exe_path(path: &str) -> bool {
    let local = path.strip_prefix("\\\\?\\").unwrap_or(path);
    let b = local.as_bytes();
    b.len() > 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && b[2] == b'\\'
        && !local.ends_with('\\')
        && !path.chars().any(|c| c == '%' || c == '"' || c.is_control())
}

/// `"<value>"`; refuses values a quoted argument cannot carry unambiguously.
pub fn quote_arg(value: &str) -> Result<String, CommandError> {
    // A trailing backslash would escape the closing quote (CommandLineToArgvW rules).
    if value.is_empty()
        || value.ends_with('\\')
        || value.chars().any(|c| c == '"' || c.is_control())
    {
        return Err(CommandError);
    }
    Ok(format!("\"{value}\""))
}
fn bounded(command: String) -> Result<String, CommandError> {
    if command.encode_utf16().count() > MAX_COMMAND_UNITS {
        return Err(CommandError);
    }
    Ok(command)
}

/// `"<exe>" "%1"`.
pub fn release_command(exe: &str) -> Result<String, CommandError> {
    if !supported_exe_path(exe) {
        return Err(CommandError);
    }
    bounded(format!("{} \"%1\"", quote_arg(exe)?))
}

/// `"<exe>" --test-host-url "<u>" --test-host-dir "<d>" --test-host-link "%1"`.
pub fn test_host_command(exe: &str, url: &str, dir: &str) -> Result<String, CommandError> {
    if !supported_exe_path(exe) {
        return Err(CommandError);
    }
    bounded(format!(
        "{} --test-host-url {} --test-host-dir {} --test-host-link \"%1\"",
        quote_arg(exe)?,
        quote_arg(url)?,
        quote_arg(dir)?
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// Leading quoted supported executable; `rest` = arguments after the separating whitespace.
    Quoted {
        exe: String,
        rest: String,
    },
    Unsupported,
}
pub fn parse_command(cmd: &str) -> Parsed {
    if cmd.chars().any(char::is_control) {
        return Parsed::Unsupported;
    }
    let Some((exe, after)) = cmd.strip_prefix('"').and_then(|s| s.split_once('"')) else {
        return Parsed::Unsupported;
    };
    let rest = match after.chars().next() {
        None => "",
        Some(' ') => after.trim_start_matches(' '),
        Some(_) => return Parsed::Unsupported,
    };
    if !supported_exe_path(exe) {
        return Parsed::Unsupported;
    }
    Parsed::Quoted {
        exe: exe.to_owned(),
        rest: rest.to_owned(),
    }
}

/// Bounded registry snapshot of a scheme root. Key paths are relative to the root ("" = the
/// root itself), value names and key paths ASCII-lowercased; data are the raw bytes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tree {
    pub keys: BTreeSet<String>,
    pub values: BTreeMap<(String, String), (u32, Vec<u8>)>,
}
impl Tree {
    /// A tree holding only the root key.
    pub fn root() -> Self {
        let mut t = Self::default();
        t.keys.insert(String::new());
        t
    }
    /// Adds `key` (and every parent key).
    pub fn with_key(mut self, key: &str) -> Self {
        let key = key.to_ascii_lowercase();
        let mut path = String::new();
        self.keys.insert(String::new());
        for part in key.split('\\').filter(|p| !p.is_empty()) {
            if !path.is_empty() {
                path.push('\\');
            }
            path.push_str(part);
            self.keys.insert(path.clone());
        }
        self
    }
    pub fn with_value(self, key: &str, name: &str, ty: u32, data: Vec<u8>) -> Self {
        let mut t = self.with_key(key);
        t.values.insert(
            (key.to_ascii_lowercase(), name.to_ascii_lowercase()),
            (ty, data),
        );
        t
    }
    pub fn with_sz(self, key: &str, name: &str, text: &str) -> Self {
        self.with_value(key, name, REG_SZ, sz(text))
    }
    fn get(&self, key: &str, name: &str) -> Option<&(u32, Vec<u8>)> {
        self.values.get(&(key.to_owned(), name.to_owned()))
    }
}
/// REG_SZ bytes: UTF-16LE plus the terminating NUL.
pub fn sz(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootRead {
    /// `ERROR_FILE_NOT_FOUND` on the root: nothing registered.
    Missing,
    /// Access denied, overflow, malformed data or any other failure.
    Unknown,
    Exists(Tree),
}

/// Read-only probes (native: HKCU `KEY_READ` and std::fs; tests: injected).
pub trait RegistryProbe {
    fn read_root(&self) -> RootRead;
    /// `symlink_metadata(exe)`: `Ok`, or the raw OS error (`None` when unknown).
    fn target(&self, exe: &str) -> Result<(), Option<i32>>;
    /// The drive root (`X:\`) of `exe` exists.
    fn drive_root_exists(&self, exe: &str) -> bool;
    /// `exe` is this running executable (canonicalized, case-insensitive); `Err` = unknown.
    fn same_file(&self, exe: &str) -> Result<bool, RegistryError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    CreatedNew,
    OpenedExisting,
}
/// Writes under the scheme root only (native: HKCU `KEY_WRITE`).
pub trait RegistryWriter {
    /// Creates (or opens) the root and reports the disposition observed by THIS call.
    fn create_root(&mut self) -> Result<Disposition, RegistryError>;
    /// Creates `key` (relative, "" = root) if needed and sets the REG_SZ value `name`.
    fn set(&mut self, key: &str, name: &str, value: &str) -> Result<(), RegistryError>;
    /// Deletes the whole root.
    fn delete_tree(&mut self) -> Result<(), RegistryError>;
}

/// What this executable would register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intended {
    pub scheme: Scheme,
    pub display: &'static str,
    pub exe: String,
    pub command: String,
    test_host: bool,
}
impl Intended {
    pub fn release(exe: &str) -> Result<Self, CommandError> {
        Ok(Self {
            scheme: Scheme::Release,
            display: DISPLAY_RELEASE,
            exe: exe.to_owned(),
            command: release_command(exe)?,
            test_host: false,
        })
    }
    /// Isolated test schemes only.
    pub fn test_host(
        scheme: Scheme,
        exe: &str,
        url: &str,
        dir: &str,
    ) -> Result<Self, CommandError> {
        if !scheme.is_isolated() {
            return Err(CommandError);
        }
        Ok(Self {
            scheme,
            display: DISPLAY_TEST_HOST,
            exe: exe.to_owned(),
            command: test_host_command(exe, url, dir)?,
            test_host: true,
        })
    }
    pub fn icon(&self) -> String {
        format!("{},0", self.exe)
    }
    /// The documented D2 values in write order (command LAST).
    pub fn values(&self) -> [(&'static str, &'static str, String); 4] {
        [
            ("", "", self.display.to_owned()),
            ("", "URL Protocol", String::new()),
            ("DefaultIcon", "", self.icon()),
            ("shell\\open\\command", "", self.command.clone()),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootState {
    Missing,
    Exists,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    Own { identical_command: bool },
    Dangling,
    Foreign,
}
/// One complete bounded observation; a recheck must reproduce it exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    pub root: RootState,
    pub ownership: Option<Ownership>,
    tree: Option<Tree>,
}
pub fn classify(probe: &dyn RegistryProbe, intended: &Intended) -> Classified {
    match probe.read_root() {
        RootRead::Missing => Classified {
            root: RootState::Missing,
            ownership: None,
            tree: None,
        },
        RootRead::Unknown => Classified {
            root: RootState::Unknown,
            ownership: None,
            tree: None,
        },
        RootRead::Exists(tree) => Classified {
            root: RootState::Exists,
            ownership: Some(ownership(&tree, probe, intended)),
            tree: Some(tree),
        },
    }
}

/// Lossless REG_SZ text: even length, at most one terminating NUL, no other NUL.
fn sz_text(ty: u32, data: &[u8]) -> Option<String> {
    if ty != REG_SZ || data.len() > MAX_VALUE_BYTES || !data.len().is_multiple_of(2) {
        return None;
    }
    let mut units: Vec<u16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u16::from_le_bytes(*p))
        .collect();
    if units.last() == Some(&0) {
        units.pop();
    }
    let text = String::from_utf16(&units).ok()?;
    (!text.contains('\0')).then_some(text)
}
fn sz_value(tree: &Tree, key: &str, name: &str) -> Option<String> {
    tree.get(key, name)
        .and_then(|(ty, data)| sz_text(*ty, data))
}
/// The supported undelegated `shell\open\command` structure; its command text.
fn supported_command(tree: &Tree) -> Option<String> {
    const OPEN: &str = "shell\\open";
    const COMMAND: &str = "shell\\open\\command";
    let verb_ok = match tree.get("shell", "") {
        None => true,
        Some(_) => sz_value(tree, "shell", "").is_some_and(|v| v.eq_ignore_ascii_case("open")),
    };
    // No other verb, no DropTarget/ddeexec/other subkey under the verb.
    let keys_ok = tree
        .keys
        .iter()
        .filter(|k| k.starts_with("shell\\"))
        .all(|k| k == OPEN || k == COMMAND);
    // No DelegateExecute (or any other value) beside the command default; none on the verb.
    let values_ok = tree.values.keys().all(|(k, n)| match k.as_str() {
        COMMAND => n.is_empty(),
        OPEN => false,
        _ => true,
    });
    if !(verb_ok && keys_ok && values_ok && tree.keys.contains(COMMAND)) {
        return None;
    }
    sz_value(tree, COMMAND, "")
}
fn ownership(tree: &Tree, probe: &dyn RegistryProbe, intended: &Intended) -> Ownership {
    let Some(command) = supported_command(tree) else {
        return Ownership::Foreign;
    };
    let Parsed::Quoted { exe, rest } = parse_command(&command) else {
        return Ownership::Foreign;
    };
    let arguments_ok = if intended.test_host {
        command == intended.command
    } else {
        rest == "\"%1\""
    };
    if !arguments_ok {
        return Ownership::Foreign;
    }
    match probe.target(&exe) {
        Ok(()) => match probe.same_file(&exe) {
            Ok(true) => Ownership::Own {
                identical_command: command == intended.command,
            },
            Ok(false) | Err(RegistryError) => Ownership::Foreign,
        },
        // ERROR_FILE_NOT_FOUND / ERROR_PATH_NOT_FOUND on a present drive: provably absent.
        Err(Some(2 | 3)) if probe.drive_root_exists(&exe) => Ownership::Dangling,
        Err(_) => Ownership::Foreign,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Register,
    TakeOverDangling,
    Refresh,
    Noop,
    Leave(&'static str),
}
pub fn decide(c: &Classified) -> Action {
    match (c.root, c.ownership) {
        (RootState::Missing, _) => Action::Register,
        (RootState::Exists, Some(Ownership::Dangling)) => Action::TakeOverDangling,
        (
            RootState::Exists,
            Some(Ownership::Own {
                identical_command: true,
            }),
        ) => Action::Noop,
        (
            RootState::Exists,
            Some(Ownership::Own {
                identical_command: false,
            }),
        ) => Action::Refresh,
        (RootState::Exists, _) => Action::Leave("scheme-foreign"),
        (RootState::Unknown, _) => Action::Leave("scheme-probe-failed"),
    }
}

/// Every key path from the root down to `key` (lowercase).
fn key_prefixes(key: &str) -> Vec<String> {
    let tree = Tree::default().with_key(key);
    tree.keys.into_iter().collect()
}
/// Before deleting a root THIS call created: it must hold exactly the values written so far
/// and no key beyond those written or the one being attempted.
fn only_written(probe: &dyn RegistryProbe, written: &Tree, attempted: &str) -> bool {
    let RootRead::Exists(now) = probe.read_root() else {
        return false;
    };
    let allowed: std::collections::BTreeSet<String> = written
        .keys
        .iter()
        .cloned()
        .chain(key_prefixes(attempted))
        .collect();
    now.values == written.values
        && written.keys.is_subset(&now.keys)
        && now.keys.is_subset(&allowed)
}

/// Performs `action` (from `decide(before)`); returns the fixed code to log (`None` = Noop).
pub fn apply(
    action: Action,
    before: &Classified,
    intended: &Intended,
    probe: &dyn RegistryProbe,
    writer: &mut dyn RegistryWriter,
) -> Option<&'static str> {
    match action {
        Action::Noop => None,
        Action::Leave(code) => Some(code),
        Action::Register => Some(register_new(intended, probe, writer)),
        Action::TakeOverDangling | Action::Refresh => {
            // Full bounded re-probe immediately before opening anything for write.
            if classify(probe, intended) != *before {
                return Some("scheme-race-leave");
            }
            let values = intended.values();
            let selected = if action == Action::Refresh {
                &values[3..]
            } else {
                &values[..]
            };
            for (key, name, value) in selected {
                if writer.set(key, name, value).is_err() {
                    // Never delete a root that existed before this process.
                    return Some("scheme-write-failed");
                }
            }
            Some(if action == Action::Refresh {
                "scheme-refreshed"
            } else {
                "scheme-registered"
            })
        }
    }
}

fn register_new(
    intended: &Intended,
    probe: &dyn RegistryProbe,
    writer: &mut dyn RegistryWriter,
) -> &'static str {
    match writer.create_root() {
        Err(RegistryError) => return "scheme-write-failed",
        Ok(Disposition::OpenedExisting) => return "scheme-race-leave",
        Ok(Disposition::CreatedNew) => {}
    }
    let mut written = Tree::root();
    for (key, name, value) in intended.values() {
        if writer.set(key, name, &value).is_err() {
            // Rollback is justified only by THIS call's creation disposition AND an unchanged
            // owned content; otherwise the residual is left (classified Foreign next start).
            if only_written(probe, &written, key) {
                let _ = writer.delete_tree();
            }
            return "scheme-write-failed";
        }
        written = written.with_sz(key, name, &value);
    }
    "scheme-registered"
}

/// classify → decide → apply.
pub fn register(
    intended: &Intended,
    probe: &dyn RegistryProbe,
    writer: &mut dyn RegistryWriter,
) -> Option<&'static str> {
    let before = classify(probe, intended);
    apply(decide(&before), &before, intended, probe, writer)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unregister {
    Allowed,
    Refused,
}
/// Five-step ownership proof for deleting an isolated test key of THIS directory.
pub fn unregister_allowed(
    scheme: Scheme,
    this_dir: &std::path::Path,
    probe: &dyn RegistryProbe,
    intended: &Intended,
) -> Unregister {
    // (1) exactly golive-testhost-<hash of THIS dir>.
    let isolated = scheme.is_isolated()
        && scheme == Scheme::test_host(this_dir)
        && intended.test_host
        && intended.scheme == scheme;
    // (2) the root exists.
    let (true, RootRead::Exists(tree)) = (isolated, probe.read_root()) else {
        return Unregister::Refused;
    };
    // (3) supported REG_SZ command of a quoted executable that is this running file.
    let Some(command) = supported_command(&tree) else {
        return Unregister::Refused;
    };
    let Parsed::Quoted { exe, .. } = parse_command(&command) else {
        return Unregister::Refused;
    };
    if probe.same_file(&exe) != Ok(true) {
        return Unregister::Refused;
    }
    // (4) the WHOLE command equals the generated one (URL, THIS dir, link flag).
    if command != intended.command {
        return Unregister::Refused;
    }
    // (5) the owned display value, plus every other owned value and no unexpected key/value.
    if sz_value(&tree, "", "").as_deref() != Some(DISPLAY_TEST_HOST) {
        return Unregister::Refused;
    }
    let expected = intended
        .values()
        .into_iter()
        .fold(Tree::root(), |t, (k, n, v)| t.with_sz(k, n, &v));
    let names = |t: &Tree| t.values.keys().cloned().collect::<Vec<_>>();
    if tree.keys != expected.keys || names(&tree) != names(&expected) {
        return Unregister::Refused;
    }
    if sz_value(&tree, "", "url protocol").as_deref() != Some("")
        || sz_value(&tree, "defaulticon", "") != Some(intended.icon())
    {
        return Unregister::Refused;
    }
    Unregister::Allowed
}
/// Proof, immediate re-proof, then delete. `scheme-unregistered` or `scheme-unregister-refused`.
pub fn unregister(
    this_dir: &std::path::Path,
    probe: &dyn RegistryProbe,
    intended: &Intended,
    writer: &mut dyn RegistryWriter,
) -> &'static str {
    for _ in 0..2 {
        // Proof, then the immediate re-proof right before the deletion.
        if unregister_allowed(intended.scheme, this_dir, probe, intended) != Unregister::Allowed {
            return "scheme-unregister-refused";
        }
    }
    match writer.delete_tree() {
        Ok(()) => "scheme-unregistered",
        Err(RegistryError) => "scheme-unregister-refused",
    }
}

/// Windows adapter: `HKCU\Software\Classes\<scheme>` only (never HKLM, HKCR or UserChoice).
/// Reads are bounded (depth, keys, values, name and data sizes); any overflow, malformed data
/// or error makes the root `Unknown` (=> Leave). NATIVE-GATE: never executed in this pipeline.
#[cfg(windows)]
pub mod native {
    use super::{
        Disposition, Intended, MAX_VALUE_BYTES, RegistryError, RegistryProbe, RegistryWriter,
        RootRead, Tree, register, supported_exe_path, sz,
    };
    use crate::login::Scheme;
    use windows::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, WIN32_ERROR,
    };
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, KEY_WRITE, REG_CREATE_KEY_DISPOSITION,
        REG_CREATED_NEW_KEY, REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, REG_SZ as WIN_REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW,
        RegSetValueExW,
    };
    use windows::core::{HSTRING, PCWSTR, PWSTR};

    const MAX_DEPTH: usize = 4;
    const MAX_KEYS: usize = 32;
    const MAX_VALUES: u32 = 32;
    const MAX_SUBKEYS: u32 = 32;
    /// Registry key/value names are at most 255 characters (+ NUL).
    const NAME_UNITS: usize = 256;

    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: a key handle opened/created by this module, closed exactly once.
            let _ = unsafe { RegCloseKey(self.0) };
        }
    }
    fn open(parent: HKEY, sub: &str, access: REG_SAM_FLAGS) -> Result<Key, WIN32_ERROR> {
        let mut handle = HKEY::default();
        // SAFETY: valid NUL-terminated name and out pointer for the duration of the call.
        let status =
            unsafe { RegOpenKeyExW(parent, &HSTRING::from(sub), None, access, &mut handle) };
        if status == ERROR_SUCCESS {
            Ok(Key(handle))
        } else {
            Err(status)
        }
    }
    fn create(path: &str, access: REG_SAM_FLAGS) -> Result<(Key, REG_CREATE_KEY_DISPOSITION), ()> {
        let mut handle = HKEY::default();
        let mut disposition = REG_CREATE_KEY_DISPOSITION::default();
        // SAFETY: valid NUL-terminated name and out pointers for the duration of the call.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                &HSTRING::from(path),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                access,
                None,
                &mut handle,
                Some(&mut disposition),
            )
        };
        if status == ERROR_SUCCESS {
            Ok((Key(handle), disposition))
        } else {
            Err(())
        }
    }

    /// Bounded recursive snapshot of `key` into `tree` under the relative path `rel`.
    fn read_key(key: &Key, rel: &str, depth: usize, tree: &mut Tree) -> Result<(), ()> {
        if depth > MAX_DEPTH || tree.keys.len() >= MAX_KEYS {
            return Err(());
        }
        tree.keys.insert(rel.to_ascii_lowercase());
        for index in 0..=MAX_VALUES {
            let mut name = [0u16; NAME_UNITS];
            let mut name_len = NAME_UNITS as u32;
            let mut ty = 0u32;
            let mut data = vec![0u8; MAX_VALUE_BYTES];
            let mut data_len = MAX_VALUE_BYTES as u32;
            // SAFETY: buffers and lengths describe valid writable memory for the call.
            let status = unsafe {
                RegEnumValueW(
                    key.0,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut name_len,
                    None,
                    Some(&mut ty),
                    Some(data.as_mut_ptr()),
                    Some(&mut data_len),
                )
            };
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            // ERROR_MORE_DATA (oversized name/data) or any error, or too many values: unknown.
            if status != ERROR_SUCCESS || index == MAX_VALUES {
                return Err(());
            }
            let name = name.get(..name_len as usize).ok_or(())?;
            let name = String::from_utf16(name).map_err(drop)?;
            data.truncate(data_len as usize);
            tree.values.insert(
                (rel.to_ascii_lowercase(), name.to_ascii_lowercase()),
                (ty, data),
            );
        }
        let mut subkeys = Vec::new();
        for index in 0..=MAX_SUBKEYS {
            let mut name = [0u16; NAME_UNITS];
            let mut name_len = NAME_UNITS as u32;
            // SAFETY: buffer and length describe valid writable memory for the call.
            let status = unsafe {
                RegEnumKeyExW(
                    key.0,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut name_len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            if status != ERROR_SUCCESS || index == MAX_SUBKEYS {
                return Err(());
            }
            let name = name.get(..name_len as usize).ok_or(())?;
            subkeys.push(String::from_utf16(name).map_err(drop)?);
        }
        for sub in subkeys {
            if sub.is_empty() || sub.contains('\\') {
                return Err(());
            }
            let child = open(key.0, &sub, KEY_READ).map_err(drop)?;
            let path = if rel.is_empty() {
                sub
            } else {
                format!("{rel}\\{sub}")
            };
            read_key(&child, &path, depth + 1, tree)?;
        }
        Ok(())
    }

    /// HKCU probe/writer for one scheme root.
    pub struct Hkcu {
        root: String,
    }
    impl Hkcu {
        pub fn new(scheme: Scheme) -> Self {
            Self {
                root: format!("Software\\Classes\\{}", scheme.name()),
            }
        }
    }
    fn local(path: &str) -> &str {
        path.strip_prefix("\\\\?\\").unwrap_or(path)
    }
    fn canonical(path: &std::path::Path) -> Option<String> {
        std::fs::canonicalize(path)
            .ok()?
            .into_os_string()
            .into_string()
            .ok()
    }
    impl RegistryProbe for Hkcu {
        fn read_root(&self) -> RootRead {
            match open(HKEY_CURRENT_USER, &self.root, KEY_READ) {
                Err(status) if status == ERROR_FILE_NOT_FOUND => RootRead::Missing,
                Err(_) => RootRead::Unknown,
                Ok(key) => {
                    let mut tree = Tree::default();
                    match read_key(&key, "", 0, &mut tree) {
                        Ok(()) => RootRead::Exists(tree),
                        Err(()) => RootRead::Unknown,
                    }
                }
            }
        }
        fn target(&self, exe: &str) -> Result<(), Option<i32>> {
            std::fs::symlink_metadata(exe)
                .map(drop)
                .map_err(|e| e.raw_os_error())
        }
        fn drive_root_exists(&self, exe: &str) -> bool {
            local(exe)
                .get(..3)
                .is_some_and(|drive| std::fs::symlink_metadata(drive).is_ok())
        }
        fn same_file(&self, exe: &str) -> Result<bool, RegistryError> {
            let own = std::env::current_exe()
                .ok()
                .and_then(|p| canonical(&p))
                .ok_or(RegistryError)?;
            let other = canonical(std::path::Path::new(exe)).ok_or(RegistryError)?;
            Ok(own.eq_ignore_ascii_case(&other))
        }
    }
    impl RegistryWriter for Hkcu {
        fn create_root(&mut self) -> Result<Disposition, RegistryError> {
            let (_key, disposition) =
                create(&self.root, KEY_WRITE | KEY_READ).map_err(|()| RegistryError)?;
            Ok(if disposition == REG_CREATED_NEW_KEY {
                Disposition::CreatedNew
            } else {
                Disposition::OpenedExisting
            })
        }
        fn set(&mut self, key: &str, name: &str, value: &str) -> Result<(), RegistryError> {
            let path = if key.is_empty() {
                self.root.clone()
            } else {
                format!("{}\\{key}", self.root)
            };
            let (key, _) = create(&path, KEY_SET_VALUE).map_err(|()| RegistryError)?;
            let data = sz(value);
            // SAFETY: open key handle; `data` is a valid REG_SZ buffer for the call.
            let status = unsafe {
                RegSetValueExW(key.0, &HSTRING::from(name), None, WIN_REG_SZ, Some(&data))
            };
            if status == ERROR_SUCCESS {
                Ok(())
            } else {
                Err(RegistryError)
            }
        }
        fn delete_tree(&mut self) -> Result<(), RegistryError> {
            // SAFETY: valid NUL-terminated subkey name of our own scheme root.
            let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(&self.root)) };
            if status == ERROR_SUCCESS {
                Ok(())
            } else {
                Err(RegistryError)
            }
        }
    }

    /// The module path of this process (never argv/PATH), if it is a supported local path.
    fn self_exe() -> Option<String> {
        std::env::current_exe()
            .ok()?
            .into_os_string()
            .into_string()
            .ok()
            .filter(|p| supported_exe_path(p))
    }

    /// Release first instance: conservative registration of the real `golive` scheme.
    pub fn register_release() -> Option<&'static str> {
        let Some(intended) = self_exe().and_then(|exe| Intended::release(&exe).ok()) else {
            return Some("scheme-self-path-unavailable");
        };
        register(
            &intended,
            &Hkcu::new(Scheme::Release),
            &mut Hkcu::new(Scheme::Release),
        )
    }

    #[cfg(feature = "test-host")]
    fn test_host_intended(
        scheme: Scheme,
        url: &str,
        dir: &std::path::Path,
    ) -> Result<Intended, &'static str> {
        crate::login::assert_isolated_for_registry(&scheme)?;
        let exe = self_exe().ok_or("scheme-self-path-unavailable")?;
        let dir = dir.to_str().ok_or("scheme-self-path-unavailable")?;
        Intended::test_host(scheme, &exe, url, dir).map_err(|_| "scheme-self-path-unavailable")
    }
    /// Test host: register ONLY this directory's isolated scheme (Foreign is left untouched).
    #[cfg(feature = "test-host")]
    pub fn register_test_host(
        scheme: Scheme,
        url: &str,
        dir: &std::path::Path,
    ) -> Result<Option<&'static str>, &'static str> {
        let intended = test_host_intended(scheme, url, dir)?;
        Ok(register(
            &intended,
            &Hkcu::new(scheme),
            &mut Hkcu::new(scheme),
        ))
    }
    /// Test host: delete ONLY a proven-own isolated key of this directory.
    #[cfg(feature = "test-host")]
    pub fn unregister_test_host(scheme: Scheme, url: &str, dir: &std::path::Path) -> &'static str {
        match test_host_intended(scheme, url, dir) {
            Ok(intended) => {
                super::unregister(dir, &Hkcu::new(scheme), &intended, &mut Hkcu::new(scheme))
            }
            Err(_) => "scheme-unregister-refused",
        }
    }
}
