//! Transfers between the client and a server (root `b`, over ssh through
//! a shim) that resume from what an interrupted one left: the temporary
//! file is kept if it is at least 1 MiB long and the source starts with
//! it (`validFilePrefix`), whichever end is gemel's: onto a new file, and
//! onto an old version, which the rest comes as rsync deltas against. With
//! `GEMEL_REFERENCE_UNISON` naming OCaml Unison, each of gemel and OCaml
//! is the client, and each the server; OCaml at both ends shows what the
//! others must do.

#![cfg(unix)]

mod common;
#[path = "../../../crates/gemel-engine/tests/support/mod.rs"]
mod support;
#[path = "../../../crates/gemel-ui-api/tests/ui_support/mod.rs"]
mod ui_support;

use std::ffi::OsString;
use std::path::{Path as StdPath, PathBuf};

use common::{gemel_path, ocaml_args, ocaml_path, run};
use ui_support::Pair;

const MIB: usize = 1024 * 1024;

/// Gemel, and OCaml if there is one.
fn programs() -> Vec<OsString> {
    let mut v = vec![gemel_path().to_owned()];
    v.extend(ocaml_path());
    v
}

/// The source's contents: 3 MiB that don't repeat.
fn contents() -> Vec<u8> {
    let mut x: u32 = 0x1234_5678;
    (0..3 * MIB)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect()
}

/// Syncs `pair` with `client` in batch mode: its exit code.
fn sync(client: &OsString, pair: &Pair) -> Option<i32> {
    let args = ["a", "b", "-batch"];
    let args = if client == gemel_path() {
        args.to_vec()
    } else {
        ocaml_args(&args)
    };
    let r = run(client, pair, &args, None);
    assert!(r.stderr.iter().all(|l| !l.contains("rror")), "{r:#?}");
    r.code
}

/// The roots' archive hashes, from their archives' names in `u`.
fn archive_hashes(u: &StdPath) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(u)
        .unwrap()
        .filter_map(|e| {
            let name = e.unwrap().file_name().into_string().unwrap();
            let hash = name.strip_prefix("ar")?;
            (hash.len() == 32).then(|| hash.to_owned())
        })
        .collect();
    v.sort();
    v
}

/// What an interrupted transfer to `dest` left: `prefix`, as the
/// temporary file for `big`, under each root's archive hash (only the
/// destination's is used). Each is also linked from outside the replicas,
/// so that what becomes of it shows: kept and completed, or removed.
fn leave_temp_files(pair: &Pair, dest: &StdPath, prefix: &[u8]) -> Vec<PathBuf> {
    archive_hashes(&pair.u())
        .iter()
        .map(|hash| {
            let temp = dest.join(format!(".unison.big.{hash}.unison.tmp"));
            std::fs::write(&temp, prefix).unwrap();
            let kept = pair.base().join(format!("left-{hash}"));
            std::fs::hard_link(&temp, &kept).unwrap();
            kept
        })
        .collect()
}

/// An old version of the source: changed near the start and past where
/// the transfers resume, and shorter.
fn old_version(big: &[u8]) -> Vec<u8> {
    let mut old = big[..5 * MIB / 2].to_vec();
    old[100] ^= 1;
    old[9 * MIB / 4] ^= 1;
    old
}

#[test]
fn an_interrupted_transfer_resumes_from_what_it_left() {
    let big = contents();
    let old = old_version(&big);
    let mut other = big[..2 * MIB].to_vec();
    other[MIB] ^= 1;
    // What was left, and whether the transfer goes on from it.
    let cases: [(&str, &[u8], bool); 3] = [
        ("a prefix of the source", &big[..2 * MIB], true),
        ("not a prefix of the source", &other, false),
        ("too short to keep", &big[..MIB / 2], false),
    ];
    for client in programs() {
        for server in programs() {
            for (to_server, onto_old) in
                [(true, false), (false, false), (true, true), (false, true)]
            {
                for (what, prefix, resumed) in cases {
                    let what = format!(
                        "{what}, client {client:?}, server {server:?}, to the {}{}",
                        if to_server { "server" } else { "client" },
                        if onto_old {
                            ", onto an old version"
                        } else {
                            ""
                        }
                    );
                    let pair = Pair::remote("transfer-resume", &server);
                    let (src, dest) = if to_server {
                        (pair.a(), pair.b())
                    } else {
                        (pair.b(), pair.a())
                    };
                    if onto_old {
                        std::fs::write(src.join("big"), &old).unwrap();
                        std::fs::write(dest.join("big"), &old).unwrap();
                    }
                    // Archives first, which name the temporary files.
                    assert_eq!(sync(&client, &pair), Some(0), "{what}");
                    std::fs::write(src.join("big"), &big).unwrap();
                    let left = leave_temp_files(&pair, &dest, prefix);
                    assert_eq!(left.len(), 2, "{what}");
                    assert_eq!(sync(&client, &pair), Some(0), "{what}");
                    assert!(std::fs::read(dest.join("big")).unwrap() == big, "{what}");
                    let completed = left
                        .iter()
                        .filter(|l| std::fs::read(l).unwrap() == big)
                        .count();
                    assert_eq!(completed, usize::from(resumed), "{what}");
                }
            }
        }
    }
}
