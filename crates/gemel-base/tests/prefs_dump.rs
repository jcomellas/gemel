//! Checks the preference dump against one produced by Unison's own
//! marshallers (gemel-compat's `tests/fixtures/prefs_dump.txt`).

use gemel_base::features::Features;
use gemel_base::prefs::{self, BoolWithDefault, Category, PrefDef, Prefs, Topic};
use gemel_compat::umarshal::{self, Writer};

static BATCH: PrefDef<bool> = PrefDef::new(
    "batch",
    Category::Basic(Topic::Syncprocess_CLI),
    || false,
    "",
    "",
);
static RETRY: PrefDef<i64> = PrefDef::new(
    "retry",
    Category::Advanced(Topic::Syncprocess),
    || 0,
    "",
    "",
);
static SSHCMD: PrefDef<String> = PrefDef::new(
    "sshcmd",
    Category::Advanced(Topic::Remote),
    || "ssh".into(),
    "",
    "",
)
.local();
static ROOT: PrefDef<Vec<String>> =
    PrefDef::new("root", Category::Basic(Topic::General), Vec::new, "", "");
static IGNORECASE: PrefDef<BoolWithDefault> = PrefDef::new(
    "ignorecase",
    Category::Advanced(Topic::Sync),
    BoolWithDefault::default,
    "",
    "",
);
static UNICODE: PrefDef<BoolWithDefault> = PrefDef::new(
    "unicode",
    Category::Advanced(Topic::General),
    BoolWithDefault::default,
    "",
    "",
);

const NAMES: [&str; 6] = ["batch", "retry", "sshcmd", "root", "ignorecase", "unicode"];

fn ocaml_dump() -> Vec<u8> {
    let line = include_str!("../../gemel-compat/tests/fixtures/prefs_dump.txt")
        .lines()
        .find_map(|l| l.strip_prefix("dump "))
        .expect("dump line");
    (0..line.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&line[i..i + 2], 16).unwrap())
        .collect()
}

/// The registry gen_fixtures.ml describes: registered in reverse, because
/// the dump lists the most recently registered preference first.
fn registry() -> Prefs {
    let mut p = Prefs::new();
    p.register(&UNICODE).unwrap();
    p.register(&IGNORECASE).unwrap();
    p.register(&ROOT).unwrap();
    p.register(&SSHCMD).unwrap();
    p.register(&RETRY).unwrap();
    p.register(&BATCH).unwrap();
    p.alias(&RETRY, "retries").unwrap();
    p
}

#[test]
fn dump_is_byte_identical_to_ocaml() {
    let mut p = registry();
    let args: Vec<String> = [
        "-batch",
        "-retry=7",
        "-sshcmd=ssh -x",
        "-root=a",
        "-root=b",
        "-ignorecase=no",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    p.parse_cmd_line(&args).unwrap();
    let dumped: Vec<_> = p
        .dump(1, &Features::new())
        .unwrap()
        .into_iter()
        .filter(|(name, ..)| NAMES.contains(&name.as_str()))
        .collect();
    let bytes = umarshal::to_bytes(|w: &mut Writer| prefs::marshal_dumped(w, &dumped));
    assert_eq!(bytes, ocaml_dump());
}

#[test]
fn a_gemel_server_loads_the_ocaml_dump() {
    let dumped = umarshal::from_bytes(&ocaml_dump(), prefs::unmarshal_dumped).unwrap();
    let mut server = registry();
    server.load(&dumped, 1).unwrap();
    assert!(*server.read(&BATCH));
    assert_eq!(*server.read(&RETRY), 7);
    assert_eq!(server.read(&SSHCMD), "ssh -x");
    assert_eq!(server.read(&ROOT), &["b", "a"]);
    assert_eq!(*server.read(&IGNORECASE), BoolWithDefault::False);
    assert_eq!(*server.read(&UNICODE), BoolWithDefault::Default);
}

#[test]
fn non_utf8_values_are_rejected_not_altered() {
    let value = umarshal::to_bytes(|w| {
        w.list(&[b"/caf\xe9".to_vec()], |w, s| w.string(s));
        w.list(&["root".to_owned()], |w, n| w.string(n.as_bytes()));
    });
    let mut server = registry();
    let err = server
        .load(&[("root".into(), false, value)], 1)
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Preference root could not be read: a string value is not valid UTF-8"
    );
    assert!(server.read(&ROOT).is_empty());
}
