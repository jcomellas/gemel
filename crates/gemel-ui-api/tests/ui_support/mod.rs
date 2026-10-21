//! Helpers shared by the user interface's tests: replica pairs with a
//! Unison directory, OCaml Unison runs, and whole syncs from a profile
//! checked against OCaml's.

#![allow(dead_code)]

use std::path::{Path as StdPath, PathBuf};

use crate::support::sync::T0;
use crate::support::{TempDir, listing, settle, write};

/// How a test runs `gemel <args>` in batch mode on a pair (in loopback
/// mode or not): the items' details.
pub type Gemel = fn(&Pair, &[&str], bool) -> Vec<String>;

/// Replicas `a` and `b` in a directory, with the Unison directory `u`.
pub struct Pair {
    pub dir: TempDir,
}

impl Pair {
    pub fn new(tag: &str) -> Pair {
        let dir = TempDir::new(tag);
        for d in ["a", "b", "u"] {
            std::fs::create_dir_all(dir.0.join(d)).unwrap();
        }
        Pair { dir }
    }

    pub fn base(&self) -> &StdPath {
        &self.dir.0
    }

    pub fn a(&self) -> PathBuf {
        self.base().join("a")
    }

    pub fn b(&self) -> PathBuf {
        self.base().join("b")
    }

    pub fn u(&self) -> PathBuf {
        self.base().join("u")
    }

    pub fn root(&self, side: &str) -> String {
        self.base().join(side).to_str().unwrap().to_owned()
    }

    /// Writes the profile `name`, with this pair's roots and `lines`.
    pub fn profile(&self, name: &str, lines: &[&str]) {
        let mut text = format!("root = {}\nroot = {}\n", self.root("a"), self.root("b"));
        for l in lines {
            text += l;
            text.push('\n');
        }
        std::fs::write(self.u().join(format!("{name}.prf")), text).unwrap();
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(self.u().join("unison.log")).unwrap_or_default()
    }
}

/// OCaml Unison, run as `unison <args> -batch -ui text` on `pair`: its
/// output. `None` without `GEMEL_REFERENCE_UNISON`.
pub fn ocaml(pair: &Pair, args: &[&str]) -> Option<String> {
    let unison = std::env::var_os("GEMEL_REFERENCE_UNISON")?;
    let out = std::process::Command::new(unison)
        .args(args)
        .args(["-batch", "-ui", "text"])
        .env("UNISON", pair.u())
        .env("HOME", pair.base())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    Some(String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr))
}

/// The log, with the pair's directory shown as `S`, the times left out,
/// and without what the text interface logs (its summaries, phase 8).
pub fn startup_log(pair: &Pair) -> Vec<String> {
    let canonical = std::fs::canonicalize(pair.base()).unwrap();
    let names = [
        canonical.to_str().unwrap().to_owned(),
        pair.base().to_str().unwrap().to_owned(),
    ];
    let mut out: Vec<String> = Vec::new();
    for line in pair.log().lines() {
        let mut line = line.to_owned();
        for n in &names {
            line = line.replace(n, "S");
        }
        if line.contains(" log started at ") {
            out.push("(log started)".to_owned());
        } else if line.contains(" started propagating changes at ") {
            out.push("(start)".to_owned());
        } else if line.contains(" finished propagating changes at ") {
            out.push("(finish)".to_owned());
        } else if line.contains(" items will be synced, ") {
            // And the blank line before it.
            assert_eq!(out.pop().as_deref(), Some(""));
        } else if !(line.contains(" to be synced from ")
            || line.starts_with("Synchronization complete at ")
            || line.starts_with("  skipped: "))
        {
            out.push(line);
        }
    }
    out
}

pub const NOTHING_TO_DO: &str = "Nothing to do: replicas have not changed since last sync.";

/// Syncs a pair with gemel, and with OCaml a twin pair, from the profile
/// `p` (with `lines`): after `build` on both replicas, then after
/// `change`. Both must leave the same replicas and logs, and each item's
/// details must be OCaml's. Then OCaml finds nothing to do on gemel's
/// pair, so its archives agree with the replicas.
pub fn sync_like_ocaml(
    gemel: Gemel,
    lines: &[&str],
    build: fn(&StdPath, &str),
    change: fn(&StdPath, &str),
) {
    for loopback in [false, true] {
        let (g, o) = (Pair::new("ui-gemel"), Pair::new("ui-ocaml"));
        for p in [&g, &o] {
            p.profile("p", lines);
            build(&p.a(), "a");
            build(&p.b(), "b");
        }
        for round in 0..2 {
            if round == 1 {
                for p in [&g, &o] {
                    change(&p.a(), "a");
                    change(&p.b(), "b");
                }
            }
            // The same directory times in both pairs.
            settle(&[&g.a(), &g.b(), &o.a(), &o.b()]);
            let details = gemel(&g, &["p", "-batch"], loopback);
            let Some(out) = ocaml(&o, &["p"]) else {
                continue;
            };
            let what = format!("loopback {loopback}, round {round}");
            for d in &details {
                assert!(out.contains(d), "{what}: {d:?} not in\n{out}");
            }
            assert_eq!(listing(&g.a()), listing(&o.a()), "{what}");
            assert_eq!(listing(&g.b()), listing(&o.b()), "{what}");
            assert_eq!(startup_log(&g), startup_log(&o), "{what}");
        }
        for fastcheck in ["true", "false"] {
            let flag = format!("-fastcheck={fastcheck}");
            if let Some(out) = ocaml(&g, &["p", &flag]) {
                assert!(out.contains(NOTHING_TO_DO), "{fastcheck}: {out}");
            }
        }
    }
}

/// A profile with a label, an ignore pattern and times.
pub fn a_profile_syncs_like_ocaml(gemel: Gemel) {
    sync_like_ocaml(
        gemel,
        &["label = a test", "ignore = Name *.tmp", "times = true"],
        |r, side| {
            write(r, "same", "same", T0);
            write(r, "x.tmp", side, T0);
            if side == "a" {
                write(r, "f", "from a", T0 + 1.0);
                write(r, "sub/g", "deep", T0 + 2.0);
            } else {
                write(r, "h", "from b", T0 + 3.0);
            }
        },
        |r, side| {
            if side == "a" {
                std::fs::remove_file(r.join("f")).unwrap();
                write(r, "sub/new", "new in a", T0 + 4.0);
            } else {
                write(r, "h", "changed in b", T0 + 5.0);
                write(r, "dir/k", "in a new dir", T0 + 6.0);
            }
        },
    );
}

/// A path ending in `*`.
pub fn wildcard_paths_sync_like_ocaml(gemel: Gemel) {
    // `top/*` is the children of `top` in the first root, when the paths
    // are expanded: `z`, only in `b`, isn't synced.
    sync_like_ocaml(
        gemel,
        &["path = top/*", "times = true"],
        |r, side| {
            write(r, "outside", side, T0);
            if side == "a" {
                write(r, "top/x", "x", T0 + 1.0);
                write(r, "top/y/deep", "y", T0 + 2.0);
            } else {
                write(r, "top/z", "z", T0 + 3.0);
            }
        },
        |r, side| {
            if side == "b" {
                write(r, "top/x", "x changed", T0 + 4.0);
            }
        },
    );
}
