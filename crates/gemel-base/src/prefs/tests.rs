use super::*;

static BATCH: PrefDef<bool> = PrefDef::new(
    "batch",
    Category::Basic(Topic::Syncprocess_CLI),
    || false,
    "batch mode: ask no questions at all",
    "",
);
static TIMES: PrefDef<bool> = PrefDef::new(
    "times",
    Category::Basic(Topic::Sync),
    || true,
    "synchronize modification times",
    "",
);
static RETRY: PrefDef<i64> = PrefDef::new(
    "retry",
    Category::Advanced(Topic::Syncprocess),
    || 0,
    "re-try failed synchronizations N times",
    "",
);
static SSHCMD: PrefDef<String> = PrefDef::new(
    "sshcmd",
    Category::Advanced(Topic::Remote),
    || "ssh".into(),
    "path to the ssh executable",
    "",
);
static ROOT: PrefDef<Vec<String>> = PrefDef::new(
    "root",
    Category::Basic(Topic::General),
    Vec::new,
    "root of a replica",
    "",
);
static IGNORECASE: PrefDef<BoolWithDefault> = PrefDef::new(
    "ignorecase",
    Category::Advanced(Topic::Sync),
    BoolWithDefault::default,
    "identify upper/lowercase filenames (true/false/default)",
    "",
);
static PSEUDO: PrefDef<bool> = PrefDef::new(
    "someHostIsInsensitive",
    Category::Internal(Internal::Pseudo),
    || false,
    "*Pseudo-preference for internal use only",
    "",
);
static TESTSERVER: PrefDef<bool> = PrefDef::new(
    "testserver",
    Category::Advanced(Topic::Remote),
    || false,
    "exit after testing",
    "",
)
.cli_only();
static OLD: PrefDef<bool> =
    PrefDef::new("old", Category::Expert, || false, "an old option", "").deprecated();
static UPPER: PrefDef<String> = PrefDef::new("upper", Category::Expert, String::new, "", "")
    .intern(|_, s| {
        if s.is_empty() {
            Err("upper must not be empty".into())
        } else {
            Ok(s.to_uppercase())
        }
    });

fn prefs() -> Prefs {
    let mut p = Prefs::new();
    p.register(&BATCH).unwrap();
    p.register(&TIMES).unwrap();
    p.register(&RETRY).unwrap();
    p.register(&SSHCMD).unwrap();
    p.register(&ROOT).unwrap();
    p.register(&IGNORECASE).unwrap();
    p.register(&PSEUDO).unwrap();
    p.register(&TESTSERVER).unwrap();
    p.register(&OLD).unwrap();
    p.register(&UPPER).unwrap();
    p
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// A fresh directory for profile files, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("gemel-prefs-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn write(&self, name: &str, contents: &str) {
        fs::write(self.0.join(name), contents).unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn defaults_and_registration() {
    let mut p = prefs();
    assert!(!*p.read(&BATCH));
    assert_eq!(p.read(&SSHCMD), "ssh");
    assert_eq!(
        p.register(&BATCH),
        Err(Error::Fatal("Preference batch registered twice".into()))
    );
    assert_eq!(
        p.documentation("times").0,
        "synchronize modification times (default true)"
    );
    assert_eq!(
        p.documentation("someHostIsInsensitive"),
        (String::new(), String::new())
    );
    assert_eq!(p.kind("upper"), Some(Kind::Custom));
    assert_eq!(
        p.list(false),
        [
            "addprefsto",
            "batch",
            "ignorecase",
            "old",
            "retry",
            "root",
            "sshcmd",
            "times",
            "upper"
        ]
    );
    assert!(p.list(true).contains(&"include".to_owned()));
}

#[test]
fn command_line() {
    let mut p = prefs();
    p.parse_cmd_line(&args(&[
        "-batch",
        "-times=false",
        "-retry",
        "3",
        "-root",
        "/a",
        "-root=ssh://h//b",
        "-ignorecase",
        "yes",
        "-upper=x",
    ]))
    .unwrap();
    assert!(*p.read(&BATCH));
    assert!(!*p.read(&TIMES));
    assert_eq!(*p.read(&RETRY), 3);
    assert_eq!(p.read(&ROOT), &["ssh://h//b", "/a"]);
    assert_eq!(*p.read(&IGNORECASE), BoolWithDefault::True);
    assert_eq!(p.read(&UPPER), "X");

    let usage = |a: &[&str]| match prefs().parse_cmd_line(&args(a)) {
        Err(CmdLineError::Usage(e)) => e,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        usage(&["-someHostIsInsensitive"]),
        UsageError::Unknown("-someHostIsInsensitive".into())
    );
    assert_eq!(
        usage(&["x"]),
        UsageError::Message("Anonymous arguments not allowed".into())
    );
    assert_eq!(
        prefs().parse_cmd_line(&args(&["-upper="])),
        Err(CmdLineError::Fatal(Error::Fatal(
            "upper must not be empty".into()
        )))
    );
}

#[test]
fn aliases_share_the_value_and_travel_in_the_dump() {
    let mut p = prefs();
    p.alias(&RETRY, "retries").unwrap();
    p.parse_cmd_line(&args(&["-retries=2"])).unwrap();
    assert_eq!(*p.read(&RETRY), 2);
    assert_eq!(p.names(&RETRY), ["retries", "retry"]);
    assert_eq!(p.canonical_name("retries"), "retry");
    assert_eq!(
        p.category("retries"),
        Some(Category::Internal(Internal::Other))
    );
    assert!(!p.list(true).contains(&"retries".to_owned()));
}

#[test]
fn deprecated_preferences_warn() {
    let mut p = prefs();
    p.parse_cmd_line(&args(&["-old=false"])).unwrap();
    let w = p.take_warnings();
    assert_eq!(w.len(), 1);
    assert!(w[0].starts_with("Preference \"old\" is deprecated!"));
    assert!(w[0].ends_with("set this preference to its default value."));
}

#[test]
fn set_and_override_default() {
    let mut p = prefs();
    p.override_default(&SSHCMD, "a".into());
    assert_eq!(p.read(&SSHCMD), "a");
    p.set(&SSHCMD, "b".into());
    p.override_default(&SSHCMD, "c".into());
    assert_eq!(p.read(&SSHCMD), "b");
    p.reset_to_defaults();
    assert_eq!(p.read(&SSHCMD), "ssh");
}

#[test]
fn dump_encoding_matches_prefs_ml() {
    let mut p = Prefs::new();
    p.register(&BATCH).unwrap();
    p.register(&TESTSERVER).unwrap();
    p.set(&BATCH, true);
    let dumped = p.dump(1).unwrap();
    // Newest first; cli_only preferences (source, include, testserver) are
    // never sent.
    let names: Vec<_> = dumped.iter().map(|d| d.0.as_str()).collect();
    assert_eq!(names, ["batch", "addprefsto"]);
    let (_, local, value) = &dumped[0];
    assert!(!local);
    // Umarshal.to_string (prod2 bool (list string)) (true, ["batch"])
    assert_eq!(
        value,
        &[
            0, 0, 0, 0, 0, 0, 0, 10, 1, 1, 1, 1, 5, b'b', b'a', b't', b'c', b'h'
        ]
    );
    assert!(p.dump(2).is_err());
}

#[test]
fn dump_and_load_round_trip() {
    let mut client = prefs();
    client
        .parse_cmd_line(&args(&[
            "-retry=7",
            "-root=a",
            "-root=b",
            "-ignorecase=false",
            "-sshcmd=x",
        ]))
        .unwrap();
    let bytes = umarshal::to_bytes(|w| marshal_dumped(w, &client.dump(1).unwrap()));
    let dumped = umarshal::from_bytes(&bytes, unmarshal_dumped).unwrap();

    let mut server = prefs();
    server.load(&dumped, 1).unwrap();
    assert_eq!(*server.read(&RETRY), 7);
    assert_eq!(server.read(&ROOT), &["b", "a"]);
    assert_eq!(*server.read(&IGNORECASE), BoolWithDefault::False);
    assert_eq!(server.read(&SSHCMD), "x");

    // A server without a preference fails, unless it is local or removed.
    let mut bare = Prefs::new();
    let err = bare.load(&dumped, 1).unwrap_err();
    assert!(
        err.to_string()
            .ends_with("not found: inconsistent Unison versions??")
    );
    let mut tolerant = Prefs::new();
    for name in [
        "upper",
        "old",
        "ignorecase",
        "root",
        "sshcmd",
        "retry",
        "times",
        "batch",
        "someHostIsInsensitive",
    ] {
        tolerant.mark_removed(name);
    }
    tolerant.load(&dumped, 1).unwrap();
}

#[test]
fn profiles_with_includes() {
    let dir = TempDir::new("includes");
    dir.write(
        "default.prf",
        "\u{feff}# comment\r\n\nroot = /a\r\n  batch = true  \ninclude common\nsource? missing\nretry=5",
    );
    dir.write("common.prf", "root = /b\nsshcmd = ssh -x = y\n");
    let mut p = prefs();
    p.set_unison_dir(&dir.0);
    p.set_profile_name(Some("default".into()));
    p.load_the_file().unwrap();
    assert_eq!(p.read(&ROOT), &["/b", "/a"]);
    assert!(*p.read(&BATCH));
    assert_eq!(p.read(&SSHCMD), "ssh -x = y");
    assert_eq!(*p.read(&RETRY), 5);
    assert!(p.profile_unchanged());
    dir.write("common.prf", "root = /c\nsshcmd = changed, and longer\n");
    assert!(!p.profile_unchanged());
}

#[test]
fn profile_errors() {
    let dir = TempDir::new("errors");
    let load = |contents: &str| {
        dir.write("p.prf", contents);
        let mut p = prefs();
        p.set_unison_dir(&dir.0);
        p.set_profile_name(Some("p".into()));
        p.load_the_file().unwrap_err().to_string()
    };
    let loc = format!("Profile \"p\" (file \"{}\")", dir.0.join("p.prf").display());
    assert_eq!(
        load("nonsense"),
        format!("{loc}, line 1:\nGarbled line (no '='): nonsense")
    );
    assert_eq!(
        load("\nnope = 1"),
        format!("{loc}, line 2: `nope' is not a valid option")
    );
    assert_eq!(
        load("someHostIsInsensitive = true"),
        format!("{loc}, line 1: `someHostIsInsensitive' is not a valid option")
    );
    assert_eq!(
        load("testserver = true"),
        format!(
            "{loc}, line 1: \"testserver\" is a command line-only option; it must not be present in a profile."
        )
    );
    assert_eq!(
        load("batch = yes"),
        "batch expects a boolean value, but \nyes is not a boolean"
    );
    assert_eq!(
        load("retry = x"),
        "retry expects an integer value, but\nx is not an integer"
    );
    assert_eq!(
        load("include a b"),
        format!("{loc}, line 1:\nGarbled 'include' directive: include a b")
    );
    let missing = dir.0.join("gone.prf");
    assert_eq!(
        load("include gone"),
        format!(
            "Included from {}, line 1:\nProfile gone not found (looking for file {})",
            uncapitalize(&loc),
            missing.display()
        )
    );
}

#[test]
fn load_strings_and_add_line() {
    let dir = TempDir::new("add");
    dir.write("p.prf", "batch = true");
    let mut p = prefs();
    p.set_unison_dir(&dir.0);
    p.set_profile_name(Some("p".into()));
    p.load_strings(&["retry = 9".into()]).unwrap();
    assert_eq!(*p.read(&RETRY), 9);
    let msg = p.add("root", "/x").unwrap();
    assert!(msg.starts_with("root = /x' added to profile "));
    assert_eq!(
        fs::read_to_string(dir.0.join("p.prf")).unwrap(),
        "batch = true\nroot = /x"
    );
}

#[test]
fn scan_does_not_apply() {
    let p = prefs();
    let found = p
        .scan_cmd_line(&args(&["-root", "a", "-root=b", "-batch"]))
        .unwrap();
    assert_eq!(found["root"], ["b", "a"]);
    assert_eq!(found["batch"], ["true"]);
    assert!(p.read(&ROOT).is_empty());
}

#[test]
fn source_on_the_command_line() {
    let dir = TempDir::new("source");
    dir.write("extra", "retry = 4\n");
    let mut p = prefs();
    p.set_unison_dir(&dir.0);
    p.parse_cmd_line(&args(&["-source", "extra"])).unwrap();
    assert_eq!(*p.read(&RETRY), 4);
}
