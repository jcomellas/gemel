//! Running OCaml Unison on the same replicas, and the setup both need.

use std::ffi::OsStr;
use std::path::Path as StdPath;

use gemel_base::case;
use gemel_fs::osx;

use super::sync::*;
use super::*;

impl Setup {
    /// What OCaml's startup sets for these hosts, and `prefs`.
    pub fn init(&self, prefs: &[&str]) {
        self.set_prefs(|p| {
            let osx_host = cfg!(target_os = "macos");
            case::init(p, osx_host || cfg!(windows), osx_host);
            osx::init(p, osx_host);
            let lines: Vec<String> = prefs.iter().map(|l| l.to_string()).collect();
            p.load_strings(&lines).unwrap();
            gemel_fs::fileinfo::init(p, cfg!(windows));
        });
        self.init_backups();
    }
}

/// Both replicas of a sync, in step.
pub fn synced(dir: &TempDir, loopback: bool, prefs: &[&str], build: fn(&StdPath)) -> Setup {
    let s = setup(dir, loopback);
    s.init(prefs);
    build(&s.a());
    build(&s.b());
    s.commit(&s.find());
    s
}

/// A replica's files and contents.
pub type Listing = Vec<(String, String)>;

/// The listings of both replicas.
pub fn listings(s: &Setup) -> (Listing, Listing) {
    (listing(&s.a()), listing(&s.b()))
}

/// OCaml Unison finds nothing to do, with fast checking and without.
/// Needs OCaml Unison (`GEMEL_REFERENCE_UNISON`).
pub fn ocaml_agrees(s: &Setup, prefs: &[&str]) {
    let Some(unison) = std::env::var_os("GEMEL_REFERENCE_UNISON") else {
        return;
    };
    for fastcheck in ["true", "false"] {
        let mut flags = vec![format!("-fastcheck={fastcheck}")];
        flags.extend(flags_of(prefs));
        let all = ocaml_sync(&unison, s, &flags);
        assert!(
            all.contains("Nothing to do: replicas have not changed since last sync."),
            "fastcheck {fastcheck}: {all}"
        );
    }
}

/// OCaml Unison, run in batch mode on `s`'s replicas with `flags`, without
/// a log file: its output.
pub fn ocaml_sync(unison: &OsStr, s: &Setup, flags: &[String]) -> String {
    let mut all = vec!["-log=false".to_owned()];
    all.extend_from_slice(flags);
    ocaml_run(unison, s, &all)
}

/// OCaml Unison, run in batch mode on `s`'s replicas with `flags`: its
/// output.
pub fn ocaml_run(unison: &OsStr, s: &Setup, flags: &[String]) -> String {
    let out = std::process::Command::new(unison)
        .arg(s.a())
        .arg(s.b())
        .args(["-batch", "-ui", "text", "-contactquietly", "-times"])
        .args(flags)
        .env("UNISON", s.base.join("u"))
        .env("HOME", &s.base)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

/// Preference lines (`name = value`) as command-line flags.
pub fn flags_of(prefs: &[&str]) -> Vec<String> {
    prefs
        .iter()
        .map(|l| {
            let (k, v) = l.split_once(" = ").unwrap();
            format!("-{k}={v}")
        })
        .collect()
}
