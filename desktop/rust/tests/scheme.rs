//! CP2 S3 portable scheme-registration model (plan §3.5, §6.2) with INJECTED registry and file
//! probes and a recording writer. No registry, HKCU, ShellExecute or Windows file system is
//! touched: native behaviour is the user-run NATIVE-GATE (tests/windows_host.rs, ignored).
use nimbo_desktop::login::Scheme;
use nimbo_desktop::scheme::*;
use nimbo_desktop::shell::test_host_hash;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::Path;

const EXE: &str = "C:\\Program Files\\Nimbo\\nimbo-desktop.exe";
const OTHER: &str = "C:\\Other\\other.exe";
const URL: &str = "http://127.0.0.1:4000/";
const DIR: &str = "C:\\scratch\\t";

fn quoted(exe: &str) -> String {
    format!("\"{exe}\" \"%1\"")
}

// ---------- S1/S2: command generation and the supported grammar ----------

#[test]
fn command_line_quotes_exe_and_percent_one() {
    assert_eq!(
        release_command(EXE).unwrap(),
        "\"C:\\Program Files\\Nimbo\\nimbo-desktop.exe\" \"%1\""
    );
    assert_eq!(
        release_command("D:\\Aplicações\\Nimbo\\nimbo.exe").unwrap(),
        "\"D:\\Aplicações\\Nimbo\\nimbo.exe\" \"%1\""
    );
    assert_eq!(
        release_command("\\\\?\\C:\\Nimbo\\nimbo.exe").unwrap(),
        "\"\\\\?\\C:\\Nimbo\\nimbo.exe\" \"%1\""
    );
    assert_eq!(
        test_host_command(EXE, URL, DIR).unwrap(),
        format!(
            "\"{EXE}\" --test-host-url \"{URL}\" --test-host-dir \"{DIR}\" --test-host-link \"%1\""
        )
    );
    let long = format!("C:\\{}.exe", "a".repeat(1020));
    for bad in [
        "C:\\Nim\"bo\\x.exe",
        "C:\\Nimbo\\x\u{1}.exe",
        "C:\\Nimbo\\x\u{7f}.exe",
        "C:\\Nimbo\\x\n.exe",
        "nimbo.exe",
        "Nimbo\\nimbo.exe",
        "\\Nimbo\\nimbo.exe",
        "C:nimbo.exe",
        "\\\\server\\share\\nimbo.exe",
        "\\\\?\\UNC\\server\\share\\nimbo.exe",
        "%LOCALAPPDATA%\\Nimbo\\nimbo.exe",
        "C:\\Nimbo\\",
        "",
        long.as_str(),
    ] {
        assert_eq!(release_command(bad), Err(CommandError), "{bad:?}");
    }
    for (url, dir) in [
        ("http://127.0.0.1:4000/\"", DIR),
        (URL, "C:\\scratch\\t\\"),
        (URL, "C:\\scr\"atch"),
        (URL, "C:\\scratch\u{1}"),
        ("", DIR),
    ] {
        assert_eq!(
            test_host_command(EXE, url, dir),
            Err(CommandError),
            "{url:?} {dir:?}"
        );
    }
    assert!(supported_exe_path(EXE) && !supported_exe_path("C:\\a\\%x%.exe"));
    assert_eq!(quote_arg("a b").unwrap(), "\"a b\"");
}

#[test]
fn parse_command_supported_grammar_matrix() {
    let q = |exe: &str, rest: &str| Parsed::Quoted {
        exe: exe.to_owned(),
        rest: rest.to_owned(),
    };
    assert_eq!(parse_command(&quoted(EXE)), q(EXE, "\"%1\""));
    assert_eq!(parse_command(&format!("\"{EXE}\"")), q(EXE, ""));
    assert_eq!(
        parse_command("\"D:\\Aplicações\\Nimbo\\nimbo.exe\" \"%1\""),
        q("D:\\Aplicações\\Nimbo\\nimbo.exe", "\"%1\"")
    );
    assert_eq!(
        parse_command(&test_host_command(EXE, URL, DIR).unwrap()),
        q(
            EXE,
            &format!("--test-host-url \"{URL}\" --test-host-dir \"{DIR}\" --test-host-link \"%1\"")
        )
    );
    for bad in [
        "C:\\X\\x.exe \"%1\"".to_owned(),
        "C:\\X\\x.exe".to_owned(),
        "C:\\Program Files\\Nimbo\\nimbo-desktop.exe \"%1\"".to_owned(),
        "C:\\Apps.exe\\Nimbo\\nimbo.exe \"%1\"".to_owned(),
        "C:\\Apps.exe x\\nimbo.exe \"%1\"".to_owned(),
        "nimbo.exe \"%1\"".to_owned(),
        "\"nimbo.exe\" \"%1\"".to_owned(),
        "\"%LOCALAPPDATA%\\Nimbo\\nimbo.exe\" \"%1\"".to_owned(),
        "\"\\\\server\\share\\nimbo.exe\" \"%1\"".to_owned(),
        format!("\"{EXE}\u{1}\" \"%1\""),
        format!("\"{EXE}\" \"%1\"\n"),
        // Conservative: a TAB separator (a control character) is not a supported form.
        format!("\"{EXE}\"\t\"%1\""),
        format!("\"{EXE} \"%1\""),
        format!("\"{EXE}"),
        format!("\"{EXE}\"x \"%1\""),
        format!("\"{EXE}\"\"%1\""),
        "\"C:\\Nimbo\\\\\" \"%1\"".to_owned(),
        format!(" \"{EXE}\" \"%1\""),
        String::new(),
        "\"\" \"%1\"".to_owned(),
    ] {
        assert_eq!(parse_command(&bad), Parsed::Unsupported, "{bad:?}");
    }
}

// ---------- injected probes ----------

#[derive(Default)]
struct Fake {
    roots: RefCell<VecDeque<RootRead>>,
    last: RefCell<Option<RootRead>>,
    targets: HashMap<String, Result<(), Option<i32>>>,
    drive: bool,
    same: HashMap<String, Result<bool, RegistryError>>,
    reads: RefCell<usize>,
    target_calls: RefCell<Vec<String>>,
}
impl Fake {
    fn new(roots: Vec<RootRead>) -> Self {
        let mut f = Self {
            roots: RefCell::new(roots.into()),
            drive: true,
            ..Self::default()
        };
        f.targets.insert(EXE.to_owned(), Ok(()));
        f.same.insert(EXE.to_owned(), Ok(true));
        f.targets.insert(OTHER.to_owned(), Ok(()));
        f.same.insert(OTHER.to_owned(), Ok(false));
        f
    }
    fn one(root: RootRead) -> Self {
        Self::new(vec![root])
    }
}
impl RegistryProbe for Fake {
    fn read_root(&self) -> RootRead {
        *self.reads.borrow_mut() += 1;
        let next = self.roots.borrow_mut().pop_front();
        if let Some(r) = next {
            *self.last.borrow_mut() = Some(r.clone());
            r
        } else {
            self.last.borrow().clone().unwrap_or(RootRead::Unknown)
        }
    }
    fn target(&self, exe: &str) -> Result<(), Option<i32>> {
        self.target_calls.borrow_mut().push(exe.to_owned());
        self.targets.get(exe).cloned().unwrap_or(Err(Some(2)))
    }
    fn drive_root_exists(&self, _exe: &str) -> bool {
        self.drive
    }
    fn same_file(&self, exe: &str) -> Result<bool, RegistryError> {
        self.same.get(exe).cloned().unwrap_or(Ok(false))
    }
}

#[derive(Default)]
struct Rec {
    disposition: Option<Result<Disposition, RegistryError>>,
    fail_set_at: Option<usize>,
    sets: usize,
    calls: Vec<String>,
    delete_fails: bool,
}
impl RegistryWriter for Rec {
    fn create_root(&mut self) -> Result<Disposition, RegistryError> {
        self.calls.push("create".into());
        self.disposition.unwrap_or(Ok(Disposition::CreatedNew))
    }
    fn set(&mut self, key: &str, name: &str, value: &str) -> Result<(), RegistryError> {
        let i = self.sets;
        self.sets += 1;
        if self.fail_set_at == Some(i) {
            self.calls.push(format!("set-failed {key}|{name}"));
            return Err(RegistryError);
        }
        self.calls.push(format!("set {key}|{name}={value}"));
        Ok(())
    }
    fn delete_tree(&mut self) -> Result<(), RegistryError> {
        self.calls.push("delete_tree".into());
        if self.delete_fails {
            Err(RegistryError)
        } else {
            Ok(())
        }
    }
}

fn release() -> Intended {
    Intended::release(EXE).unwrap()
}
/// A complete registration of `command` with the D2 values.
fn full(display: &str, icon: &str, command: &str) -> Tree {
    Tree::root()
        .with_sz("", "", display)
        .with_sz("", "url protocol", "")
        .with_sz("defaulticon", "", icon)
        .with_sz("shell\\open\\command", "", command)
}
fn own_tree() -> Tree {
    full(DISPLAY_RELEASE, &format!("{EXE},0"), &quoted(EXE))
}
fn decision(probe: &Fake) -> Action {
    decide(&classify(probe, &release()))
}

#[test]
fn ownership_and_decision_matrix_with_injected_probes() {
    use Action::*;
    let foreign = Leave("scheme-foreign");
    // Missing / unknown root.
    assert_eq!(decision(&Fake::one(RootRead::Missing)), Register);
    assert_eq!(
        decision(&Fake::one(RootRead::Unknown)),
        Leave("scheme-probe-failed")
    );
    // Own: identical -> Noop; different (case/prefix of the same file) -> Refresh.
    assert_eq!(decision(&Fake::one(RootRead::Exists(own_tree()))), Noop);
    let mut f = Fake::one(RootRead::Exists(full(
        DISPLAY_RELEASE,
        "x",
        &quoted("\\\\?\\C:\\PROGRAM FILES\\Nimbo\\nimbo-desktop.exe"),
    )));
    f.targets.insert(
        "\\\\?\\C:\\PROGRAM FILES\\Nimbo\\nimbo-desktop.exe".into(),
        Ok(()),
    );
    f.same.insert(
        "\\\\?\\C:\\PROGRAM FILES\\Nimbo\\nimbo-desktop.exe".into(),
        Ok(true),
    );
    assert_eq!(decision(&f), Refresh);
    // Root exists without our understood structure.
    let cmd = quoted(EXE);
    let rows: Vec<(&str, Tree)> = vec![
        ("root only", Tree::root()),
        (
            "no command key",
            Tree::root()
                .with_sz("", "", "URL:Other")
                .with_key("shell\\open"),
        ),
        (
            "command key without default",
            Tree::root().with_key("shell\\open\\command"),
        ),
        (
            "DelegateExecute",
            own_tree().with_sz(
                "shell\\open\\command",
                "delegateexecute",
                "{00000000-0000-0000-0000-000000000000}",
            ),
        ),
        (
            "DropTarget",
            own_tree().with_sz("shell\\open\\droptarget", "clsid", "{0}"),
        ),
        (
            "ddeexec",
            own_tree().with_sz("shell\\open\\ddeexec", "", "[open]"),
        ),
        (
            "other verb",
            own_tree().with_sz("shell\\edit\\command", "", &cmd),
        ),
        (
            "default verb not open",
            own_tree().with_sz("shell", "", "edit"),
        ),
        (
            "REG_EXPAND_SZ",
            Tree::root().with_value("shell\\open\\command", "", REG_EXPAND_SZ, sz(&cmd)),
        ),
        (
            "odd bytes",
            Tree::root().with_value("shell\\open\\command", "", REG_SZ, vec![b'"', 0, b'C']),
        ),
        (
            "embedded NUL",
            Tree::root().with_value("shell\\open\\command", "", REG_SZ, {
                let mut b = sz(&cmd);
                b.splice(0..0, [0, 0]);
                b
            }),
        ),
        (
            "value > 4096 bytes",
            Tree::root().with_sz(
                "shell\\open\\command",
                "",
                &format!("\"{EXE}\" \"%1\" {}", "x".repeat(2100)),
            ),
        ),
        (
            "unsupported bare",
            Tree::root().with_sz("shell\\open\\command", "", "C:\\X\\x.exe \"%1\""),
        ),
        (
            "extra arguments",
            Tree::root().with_sz("shell\\open\\command", "", &format!("\"{EXE}\" --x \"%1\"")),
        ),
        (
            "no %1",
            Tree::root().with_sz("shell\\open\\command", "", &format!("\"{EXE}\"")),
        ),
        (
            "target exists, other file",
            Tree::root().with_sz("shell\\open\\command", "", &quoted(OTHER)),
        ),
    ];
    for (what, tree) in rows {
        assert_eq!(
            decision(&Fake::one(RootRead::Exists(tree))),
            foreign,
            "{what}"
        );
    }
    // Shell default verb explicitly "open" is fine.
    assert_eq!(
        decision(&Fake::one(RootRead::Exists(
            own_tree().with_sz("shell", "", "Open")
        ))),
        Noop
    );
    // Dangling: only NotFound(2)/PathNotFound(3) with an existing drive root.
    let gone = "C:\\Gone\\old.exe";
    let tree = || RootRead::Exists(full("URL:Old", "x", &quoted(gone)));
    for (code, drive, want) in [
        (Some(2), true, TakeOverDangling),
        (Some(3), true, TakeOverDangling),
        (Some(2), false, foreign),
        (Some(5), true, foreign),
        (Some(53), true, foreign),
        (Some(21), true, foreign),
        (None, true, foreign),
    ] {
        let mut f = Fake::one(tree());
        f.targets.insert(gone.into(), Err(code));
        f.same.insert(gone.into(), Err(RegistryError));
        f.drive = drive;
        assert_eq!(decision(&f), want, "target {code:?} drive={drive}");
    }
    // same_file unknown on an existing target -> Leave.
    let mut f = Fake::one(RootRead::Exists(own_tree()));
    f.same.insert(EXE.into(), Err(RegistryError));
    assert_eq!(decision(&f), foreign);
    // M14: a functional AMBIGUOUS unquoted registration whose guessed first token is absent is
    // Unsupported before any existence probe: never Dangling, never written.
    let ambiguous = "C:\\Program Files\\Vendor App\\app.exe \"%1\"";
    let f = Fake::one(RootRead::Exists(Tree::root().with_sz(
        "shell\\open\\command",
        "",
        ambiguous,
    )));
    let mut w = Rec::default();
    assert_eq!(register(&release(), &f, &mut w), Some("scheme-foreign"));
    assert!(w.calls.is_empty(), "no write: {:?}", w.calls);
    assert!(
        f.target_calls.borrow().is_empty(),
        "no existence probe for unsupported commands"
    );
}

#[test]
fn test_host_scheme_is_isolated_and_derived_like_identity() {
    let dir = Path::new("/scratch/a");
    let scheme = Scheme::test_host(dir);
    let hash = test_host_hash(dir.as_os_str().as_encoded_bytes());
    assert_eq!(scheme, Scheme::TestHost(hash));
    assert_eq!(scheme.name(), format!("golive-testhost-{hash:016x}"));
    #[cfg(feature = "test-host")]
    assert!(
        nimbo_desktop::shell::InstanceIdentity::test_host(dir)
            .class
            .ends_with(&format!("{hash:016x}"))
    );
    assert!(scheme.is_isolated());
    assert!(!Scheme::Release.is_isolated());
    assert_eq!(Scheme::Release.name(), "golive");
    assert_ne!(Scheme::test_host(Path::new("/scratch/b")), scheme);
    assert!(Intended::test_host(Scheme::Release, EXE, URL, DIR).is_err());
    let t = Intended::test_host(scheme, EXE, URL, DIR).unwrap();
    assert_eq!(t.display, DISPLAY_TEST_HOST);
    assert!(!t.command.contains("golive://"));
    #[cfg(feature = "test-host")]
    {
        use nimbo_desktop::login::assert_isolated_for_registry;
        assert!(assert_isolated_for_registry(&Scheme::Release).is_err());
        assert!(assert_isolated_for_registry(&scheme).is_ok());
    }
}

fn th_intended(dir: &str) -> (Scheme, Intended) {
    let scheme = Scheme::test_host(Path::new(dir));
    (scheme, Intended::test_host(scheme, EXE, URL, dir).unwrap())
}
fn th_tree(i: &Intended) -> Tree {
    full(DISPLAY_TEST_HOST, &i.icon(), &i.command)
}

#[test]
fn unregister_ownership_proof_matrix() {
    let (scheme, i) = th_intended(DIR);
    let dir = Path::new(DIR);
    let ok = Fake::one(RootRead::Exists(th_tree(&i)));
    assert_eq!(
        unregister_allowed(scheme, dir, &ok, &i),
        Unregister::Allowed
    );
    let (_, other_dir) = th_intended("C:\\scratch\\other");
    let mut not_same = Fake::one(RootRead::Exists(th_tree(&i)));
    not_same.same.insert(EXE.into(), Ok(false));
    let mut same_unknown = Fake::one(RootRead::Exists(th_tree(&i)));
    same_unknown.same.insert(EXE.into(), Err(RegistryError));
    let refused: Vec<(&str, Scheme, Fake)> = vec![
        (
            "name of another dir",
            Scheme::test_host(Path::new("C:\\scratch\\other")),
            Fake::one(RootRead::Exists(th_tree(&i))),
        ),
        (
            "real golive",
            Scheme::Release,
            Fake::one(RootRead::Exists(th_tree(&i))),
        ),
        ("root missing", scheme, Fake::one(RootRead::Missing)),
        ("root unknown", scheme, Fake::one(RootRead::Unknown)),
        (
            "command missing",
            scheme,
            Fake::one(RootRead::Exists(Tree::root().with_sz(
                "",
                "",
                DISPLAY_TEST_HOST,
            ))),
        ),
        ("not same file", scheme, not_same),
        ("same file unknown", scheme, same_unknown),
        (
            "extras of another dir",
            scheme,
            Fake::one(RootRead::Exists(full(
                DISPLAY_TEST_HOST,
                &i.icon(),
                &other_dir.command,
            ))),
        ),
        (
            "dir as a substring only",
            scheme,
            Fake::one(RootRead::Exists(full(
                DISPLAY_TEST_HOST,
                &i.icon(),
                &format!("{} --x", i.command),
            ))),
        ),
        (
            "display differs",
            scheme,
            Fake::one(RootRead::Exists(full("URL:Nimbo", &i.icon(), &i.command))),
        ),
        (
            "URL Protocol differs",
            scheme,
            Fake::one(RootRead::Exists(th_tree(&i).with_sz(
                "",
                "url protocol",
                "x",
            ))),
        ),
        (
            "icon differs",
            scheme,
            Fake::one(RootRead::Exists(full(
                DISPLAY_TEST_HOST,
                "C:\\x.exe,0",
                &i.command,
            ))),
        ),
        (
            "unexpected value",
            scheme,
            Fake::one(RootRead::Exists(th_tree(&i).with_sz("", "extra", "1"))),
        ),
        (
            "unexpected subkey",
            scheme,
            Fake::one(RootRead::Exists(th_tree(&i).with_key("shell\\edit"))),
        ),
        (
            "delegated",
            scheme,
            Fake::one(RootRead::Exists(th_tree(&i).with_sz(
                "shell\\open\\command",
                "delegateexecute",
                "{0}",
            ))),
        ),
        (
            "REG_EXPAND_SZ command",
            scheme,
            Fake::one(RootRead::Exists(th_tree(&i).with_value(
                "shell\\open\\command",
                "",
                REG_EXPAND_SZ,
                sz(&i.command),
            ))),
        ),
    ];
    for (what, s, probe) in refused {
        assert_eq!(
            unregister_allowed(s, dir, &probe, &i),
            Unregister::Refused,
            "{what}"
        );
    }
    // unregister(): proof, immediate re-proof, delete; a change in between refuses.
    let mut w = Rec::default();
    assert_eq!(unregister(dir, &ok, &i, &mut w), "scheme-unregistered");
    assert_eq!(w.calls, vec!["delete_tree"]);
    let raced = Fake::new(vec![
        RootRead::Exists(th_tree(&i)),
        RootRead::Exists(th_tree(&i).with_sz("", "extra", "1")),
    ]);
    let mut w = Rec::default();
    assert_eq!(
        unregister(dir, &raced, &i, &mut w),
        "scheme-unregister-refused"
    );
    assert!(w.calls.is_empty());
    assert!(*raced.reads.borrow() >= 2, "re-proved before deletion");
    let mut w = Rec::default();
    assert_eq!(
        unregister(Path::new("C:\\scratch\\other"), &ok, &i, &mut w),
        "scheme-unregister-refused"
    );
    assert!(w.calls.is_empty());
}

#[test]
fn register_rollback_requires_creation_disposition() {
    let i = release();
    let values = |w: &Rec| w.calls.iter().filter(|c| c.starts_with("set ")).count();
    // Missing -> CreatedNew -> four values, command LAST.
    let f = Fake::one(RootRead::Missing);
    let mut w = Rec::default();
    assert_eq!(register(&i, &f, &mut w), Some("scheme-registered"));
    assert_eq!(
        w.calls,
        vec![
            "create".to_owned(),
            format!("set |={DISPLAY_RELEASE}"),
            "set |URL Protocol=".to_owned(),
            format!("set DefaultIcon|={EXE},0"),
            format!("set shell\\open\\command|={}", quoted(EXE)),
        ]
    );
    // Created between probe and create: OpenedExisting -> no value written.
    let mut w = Rec {
        disposition: Some(Ok(Disposition::OpenedExisting)),
        ..Rec::default()
    };
    assert_eq!(register(&i, &f, &mut w), Some("scheme-race-leave"));
    assert_eq!(w.calls, vec!["create"]);
    // create failure.
    let mut w = Rec {
        disposition: Some(Err(RegistryError)),
        ..Rec::default()
    };
    assert_eq!(register(&i, &f, &mut w), Some("scheme-write-failed"));
    assert_eq!(w.calls, vec!["create"]);
    // CreatedNew + failing value write: rollback ONLY when the root still holds exactly what
    // this call wrote.
    let partial = Tree::root()
        .with_sz("", "", DISPLAY_RELEASE)
        .with_sz("", "url protocol", "");
    let f = Fake::new(vec![
        RootRead::Missing,
        RootRead::Exists(partial.clone().with_key("defaulticon")),
    ]);
    let mut w = Rec {
        fail_set_at: Some(2),
        ..Rec::default()
    };
    assert_eq!(register(&i, &f, &mut w), Some("scheme-write-failed"));
    assert_eq!(
        w.calls.iter().filter(|c| *c == "delete_tree").count(),
        1,
        "{:?}",
        w.calls
    );
    assert_eq!(values(&w), 2);
    // ... a foreign change since creation -> no deletion, residual left.
    let f = Fake::new(vec![
        RootRead::Missing,
        RootRead::Exists(partial.clone().with_sz("", "foreign", "1")),
    ]);
    let mut w = Rec {
        fail_set_at: Some(2),
        ..Rec::default()
    };
    assert_eq!(register(&i, &f, &mut w), Some("scheme-write-failed"));
    assert!(
        !w.calls.contains(&"delete_tree".to_owned()),
        "{:?}",
        w.calls
    );
    let f = Fake::new(vec![RootRead::Missing, RootRead::Unknown]);
    let mut w = Rec {
        fail_set_at: Some(0),
        ..Rec::default()
    };
    assert_eq!(register(&i, &f, &mut w), Some("scheme-write-failed"));
    assert!(!w.calls.contains(&"delete_tree".to_owned()));
    // Failing rollback still reports the write failure.
    let f = Fake::new(vec![RootRead::Missing, RootRead::Exists(Tree::root())]);
    let mut w = Rec {
        fail_set_at: Some(0),
        delete_fails: true,
        ..Rec::default()
    };
    assert_eq!(register(&i, &f, &mut w), Some("scheme-write-failed"));
    assert_eq!(w.calls.last().map(String::as_str), Some("delete_tree"));

    // TakeOverDangling: full recheck, then the D2 values; failure NEVER deletes.
    let gone = "C:\\Gone\\old.exe";
    let dangling =
        Tree::root()
            .with_sz("", "", "URL:Old")
            .with_sz("shell\\open\\command", "", &quoted(gone));
    let dangling_probe = |roots: Vec<RootRead>| {
        let mut f = Fake::new(roots);
        f.targets.insert(gone.into(), Err(Some(2)));
        f
    };
    let f = dangling_probe(vec![RootRead::Exists(dangling.clone())]);
    let mut w = Rec::default();
    assert_eq!(register(&i, &f, &mut w), Some("scheme-registered"));
    assert_eq!(values(&w), 4);
    assert!(!w.calls.iter().any(|c| c == "create" || c == "delete_tree"));
    let f = dangling_probe(vec![RootRead::Exists(dangling.clone())]);
    let mut w = Rec {
        fail_set_at: Some(1),
        ..Rec::default()
    };
    assert_eq!(register(&i, &f, &mut w), Some("scheme-write-failed"));
    assert!(
        !w.calls.contains(&"delete_tree".to_owned()),
        "no rollback of a preexisting root"
    );
    // TOCTOU: the recheck observes a change -> no write at all.
    let f = dangling_probe(vec![
        RootRead::Exists(dangling.clone()),
        RootRead::Exists(dangling.clone().with_sz(
            "shell\\open\\command",
            "delegateexecute",
            "{0}",
        )),
    ]);
    let mut w = Rec::default();
    assert_eq!(register(&i, &f, &mut w), Some("scheme-race-leave"));
    assert!(w.calls.is_empty());
    // Refresh rewrites the command only; its recheck is required too.
    let stale = own_tree().with_sz(
        "shell\\open\\command",
        "",
        &quoted("C:\\PROGRAM FILES\\Nimbo\\nimbo-desktop.exe"),
    );
    let mut f = Fake::one(RootRead::Exists(stale.clone()));
    f.targets
        .insert("C:\\PROGRAM FILES\\Nimbo\\nimbo-desktop.exe".into(), Ok(()));
    f.same.insert(
        "C:\\PROGRAM FILES\\Nimbo\\nimbo-desktop.exe".into(),
        Ok(true),
    );
    let mut w = Rec::default();
    assert_eq!(register(&i, &f, &mut w), Some("scheme-refreshed"));
    assert_eq!(
        w.calls,
        vec![format!("set shell\\open\\command|={}", quoted(EXE))]
    );
    let mut f = Fake::new(vec![
        RootRead::Exists(stale.clone()),
        RootRead::Exists(own_tree()),
    ]);
    f.targets
        .insert("C:\\PROGRAM FILES\\Nimbo\\nimbo-desktop.exe".into(), Ok(()));
    f.same.insert(
        "C:\\PROGRAM FILES\\Nimbo\\nimbo-desktop.exe".into(),
        Ok(true),
    );
    let mut w = Rec::default();
    assert_eq!(register(&i, &f, &mut w), Some("scheme-race-leave"));
    assert!(w.calls.is_empty());
    // Noop and Leave never write.
    let mut w = Rec::default();
    assert_eq!(
        register(&i, &Fake::one(RootRead::Exists(own_tree())), &mut w),
        None
    );
    assert_eq!(
        register(&i, &Fake::one(RootRead::Unknown), &mut w),
        Some("scheme-probe-failed")
    );
    assert!(w.calls.is_empty());
}
