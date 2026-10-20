//! Decodes real archives written by OCaml Unison (`tests/archives/`, see
//! `scripts/gen-archive-fixtures.sh`), down to the props, and checks three
//! things for each:
//!
//! - every directory's children are in the order of the archive's case mode;
//! - recomputing `Update.checkArchive` gives the hash stored in the archive
//!   (this exercises `Props.hash`, `Name.hash` and the fingerprint hash);
//! - re-encoding the payload gives the same bytes.
//!
//! The archive tree and `checkArchive` here are a test-only sketch of what
//! `update.ml` does; the real port is phase 4a of milestone 2.

mod common;

use std::path::Path;

use common::*;
use gemel_base::case::Mode;
use gemel_compat::umarshal::Writer;

/// Checks one archive file; returns its mode and entry counts.
fn check_file(path: &Path) -> (Mode, (usize, usize, usize)) {
    let f = parse_archive_file(&std::fs::read(path).unwrap());
    let mode = mode_of(&f.magic);
    assert_eq!(
        check_archive(&f.archive, 0, mode, &f.config),
        f.stored_hash,
        "checkArchive"
    );
    let mut w = Writer::new();
    write_archive(&f.archive, &mut w, &f.config);
    w.int(f.stored_hash);
    w.string(&f.magic);
    f.props.marshal(&mut w);
    assert!(w.into_payload() == f.payload, "re-encoded payload differs");
    (mode, count(&f.archive))
}

#[test]
fn real_archives_decode_hash_and_round_trip() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/archives");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().is_some_and(|n| n != "README"))
        .collect();
    files.sort();
    assert!(files.len() >= 10, "{} archives", files.len());
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let (mode, (dirs, regular, links)) = check_file(&path);
        // Each case's tree, after the second sync: 6 directories (including
        // the root), 8 files (README, alpha, café, with space, new,
        // src/main.c, src/deep/er/x, 日本/語) and a symlink.
        assert_eq!((dirs, regular, links), (6, 8, 1), "{name}");
        let expected = if name.starts_with("macos") || name.contains("insensitive") {
            Mode::UnicodeInsensitive
        } else {
            Mode::Sensitive
        };
        assert_eq!(mode, expected, "{name}");
    }
}
