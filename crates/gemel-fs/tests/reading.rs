//! Checks that gemel reads a replica the way OCaml Unison does: builds a
//! tree, has OCaml Unison synchronise it, then compares what gemel reads for
//! every path with what OCaml stored in its archive (props, fingerprints,
//! inode stamps, resource-fork stamps, link targets).
//!
//! Needs OCaml Unison: set `GEMEL_REFERENCE_UNISON` to its path (the CI
//! `oracle` job does). Without it the test is skipped.

mod common;

use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use common::*;
use gemel_base::path::{Local, Path};
use gemel_base::prefs::BoolWithDefault;
use gemel_fs::fileinfo::{self, FsConfig, Typ};
use gemel_fs::fspath::Fspath;
use gemel_fs::os;
use gemel_fs::osx::InfoKind;
use gemel_fs::props::PropsConfig;
use gemel_fs::system;

fn reference() -> Option<PathBuf> {
    std::env::var_os("GEMEL_REFERENCE_UNISON").map(PathBuf::from)
}

fn write(root: &std::path::Path, rel: &str, content: &str, mtime: u64) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, content).unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(mtime))
        .unwrap();
}

fn build_tree(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("empty")).unwrap();
    write(root, "README", "read me", 1_600_000_000);
    write(root, "alpha", "", 1_600_000_010);
    write(root, "Zeta", "zeta zeta", 1_600_000_020);
    write(
        root,
        "src/main.c",
        "int main(void) { return 0; }",
        1_600_000_030,
    );
    write(root, "src/deep/er/x", "x", 1_600_000_040);
    write(root, "café", "unicode", 1_600_000_050);
    write(root, "日本/語", "japanese", 1_600_000_060);
    write(root, "with space", "space", 1_600_000_070);
    write(root, "big", &"0123456789".repeat(20_000), 1_600_000_080);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let chmod = |rel: &str, mode| {
            std::fs::set_permissions(root.join(rel), std::fs::Permissions::from_mode(mode)).unwrap()
        };
        chmod("alpha", 0o600);
        chmod("src/main.c", 0o755);
        chmod("README", 0o444);
        std::os::unix::fs::symlink("README", root.join("link")).unwrap();
    }
}

fn config(archive: &ArchiveFile) -> FsConfig {
    FsConfig {
        props: PropsConfig {
            sync_times: true,
            // rsrc defaults to true when a host runs macOS.
            rsrc: cfg!(target_os = "macos"),
            ..archive.config
        },
        allow_symlinks: BoolWithDefault::Default,
        symlinks_allowed: true,
        ignore_inode_numbers: false,
        dont_chmod: false,
    }
}

/// Compares gemel's reading of `root/path` with an archive entry; returns
/// the differences. For the replica that received the changes (`target`),
/// the archive's props come from propagation rather than a scan: its change
/// times are unknown (dummy) and its directory times predate the end of
/// propagation, so those aren't compared.
fn compare(
    root: &Fspath,
    path: &Path<Local>,
    a: &Archive,
    target: bool,
    c: &FsConfig,
    out: &mut Vec<String>,
) {
    let mut info = fileinfo::get(true, root, path, &|_| false, c).unwrap();
    if target {
        match a {
            Archive::Dir(p, _) => {
                info.desc.ctime = p.ctime;
                info.desc.time = p.time;
            }
            Archive::File(p, ..) => info.desc.ctime = p.ctime,
            _ => {}
        }
    }
    let here = String::from_utf8_lossy(path.as_bytes()).into_owned();
    let mut diff = |what: &str, ours: String, theirs: String| {
        if ours != theirs {
            out.push(format!(
                "{here:?} {what}:\n    gemel: {ours}\n    OCaml: {theirs}"
            ));
        }
    };
    match a {
        Archive::Dir(props, children) => {
            diff("type", format!("{:?}", info.typ), "Directory".into());
            diff(
                "props",
                format!("{:?}", info.desc.strip(&c.props)),
                format!("{props:?}"),
            );
            for (name, child) in children {
                compare(root, &path.child(name), child, target, c, out);
            }
        }
        Archive::File(props, fp, stamp, ress) => {
            diff("type", format!("{:?}", info.typ), "File".into());
            diff(
                "props",
                format!("{:?}", info.desc.strip(&c.props)),
                format!("{props:?}"),
            );
            diff(
                "stamp",
                format!("{:?}", info.stamp(c)),
                format!("{stamp:?}"),
            );
            diff(
                "resource fork",
                format!("{:?}", info.ress_stamp()),
                format!("{ress:?}"),
            );
            let ours = os::fingerprint(root, path, InfoKind::File, c.props.rsrc).unwrap();
            diff("fingerprint", ours.to_string(), fp.to_string());
        }
        Archive::Symlink(target) => {
            diff("type", format!("{:?}", info.typ), "Symlink".into());
            let ours = os::read_link(root, path).unwrap();
            diff("target", format!("{ours:?}"), format!("{target:?}"));
        }
        Archive::Absent => diff(
            "type",
            format!("{:?}", info.typ),
            format!("{:?}", Typ::Absent),
        ),
    }
}

#[test]
fn gemel_reads_replicas_as_ocaml_does() {
    let Some(unison) = reference() else {
        eprintln!("skipped: set GEMEL_REFERENCE_UNISON to an OCaml unison");
        return;
    };
    let base = std::env::temp_dir().join(format!("gemel-reading-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("u")).unwrap();
    std::fs::create_dir_all(base.join("b")).unwrap();
    let base = std::fs::canonicalize(&base).unwrap();
    build_tree(&base.join("a"));
    let out = std::process::Command::new(&unison)
        .arg(base.join("a"))
        .arg(base.join("b"))
        .args([
            "-batch",
            "-ui",
            "text",
            "-log=false",
            "-contactquietly",
            "-times",
        ])
        .env("UNISON", base.join("u"))
        .env("HOME", &base)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "unison failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut checked = 0;
    for side in ["a", "b"] {
        let root_path = system::name_bytes(base.join(side).as_os_str());
        let archive = std::fs::read_dir(base.join("u"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("ar"))
            .map(|p| parse_archive_file(&std::fs::read(p).unwrap()))
            .find(|f| {
                // "Archive for root //host//<root> synchronizing roots ..."
                let needle = [&b"/"[..], &root_path, b" synchronizing"].concat();
                f.roots_line.windows(needle.len()).any(|w| w == needle)
            })
            .unwrap_or_else(|| panic!("no archive for root {side}"));
        let c = config(&archive);
        let root = Fspath::canonize(Some(&root_path)).unwrap();
        let mut diffs = Vec::new();
        compare(
            &root,
            &Path::empty(),
            &archive.archive,
            side == "b",
            &c,
            &mut diffs,
        );
        assert!(diffs.is_empty(), "replica {side}:\n{}", diffs.join("\n"));
        checked += 1;
    }
    assert_eq!(checked, 2);
    let _ = std::fs::remove_dir_all(&base);
}
