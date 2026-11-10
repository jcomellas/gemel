//! The filesystem operations on real directories: setting and checking
//! props, deleting, renaming, temporary names, listing children and
//! fingerprinting.

use gemel_base::name::Name;
use gemel_base::path::{Local, Path};
use gemel_base::prefs::BoolWithDefault;
use gemel_fs::fileinfo::{self, FsConfig, Typ};
use gemel_fs::fingerprint::Fingerprint;
use gemel_fs::fspath::Fspath;
use gemel_fs::os::{self, TempNames};
use gemel_fs::props::{Props, PropsConfig, Time, XattrCtx};
use gemel_fs::system;

struct Dir(std::path::PathBuf, Fspath);

impl Dir {
    fn new(name: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("gemel-ops-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = Fspath::canonize(Some(&system::name_bytes(d.as_os_str()))).unwrap();
        Dir(d, f)
    }

    fn write(&self, rel: &str, content: &[u8]) {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn path(s: &str) -> Path<Local> {
    Path::from_bytes(s.as_bytes()).unwrap()
}

fn config() -> FsConfig {
    FsConfig {
        props: PropsConfig {
            sync_times: true,
            ..PropsConfig::default()
        },
        allow_symlinks: BoolWithDefault::Default,
        symlinks_allowed: true,
        ignore_inode_numbers: false,
        dont_chmod: false,
    }
}

const NONE: &dyn Fn(&Path<Local>) -> bool = &|_| false;

#[test]
fn get_types_and_absent_paths() {
    let d = Dir::new("types");
    d.write("f", b"abc");
    std::fs::create_dir(d.0.join("sub")).unwrap();
    let c = config();
    let f = fileinfo::get(
        true,
        &d.1,
        &path("f"),
        NONE,
        &c,
        &Props::dummy(),
        &XattrCtx::standalone(),
    )
    .unwrap();
    assert_eq!((f.typ, f.desc.length), (Typ::File, 3));
    assert!(matches!(f.desc.time, Time::Synced(_)));
    assert_eq!(
        fileinfo::get_type(true, &d.1, &path("sub"), NONE, &c).unwrap(),
        Typ::Directory
    );
    let missing = fileinfo::get(
        true,
        &d.1,
        &path("nope/x"),
        NONE,
        &c,
        &Props::dummy(),
        &XattrCtx::standalone(),
    )
    .unwrap();
    assert_eq!(missing.typ, Typ::Absent);
    assert_eq!(missing.desc, Props::dummy());
}

#[cfg(unix)]
#[test]
fn symlinks_and_follow() {
    let d = Dir::new("links");
    d.write("target", b"t");
    std::os::unix::fs::symlink("target", d.0.join("l")).unwrap();
    std::os::unix::fs::symlink("missing", d.0.join("dangling")).unwrap();
    let mut c = config();
    assert_eq!(
        fileinfo::get_type(true, &d.1, &path("l"), NONE, &c).unwrap(),
        Typ::Symlink
    );
    let follow: &dyn Fn(&Path<Local>) -> bool = &|_| true;
    assert_eq!(
        fileinfo::get_type(true, &d.1, &path("l"), follow, &c).unwrap(),
        Typ::File
    );
    let e = fileinfo::get_type(true, &d.1, &path("dangling"), follow, &c).unwrap_err();
    assert!(
        e.to_string()
            .ends_with("is marked 'follow' but its target is missing"),
        "{e}"
    );
    assert_eq!(os::read_link(&d.1, &path("l")).unwrap(), b"target");
    c.symlinks_allowed = false;
    let e = fileinfo::get_type(true, &d.1, &path("l"), NONE, &c)
        .unwrap_err()
        .to_string();
    assert!(e.ends_with(fileinfo::SYMLINK_ERR), "{e}");
    c.allow_symlinks = BoolWithDefault::False;
    assert!(fileinfo::should_ignore(&e, &c));
}

#[cfg(unix)]
#[test]
fn set_and_check_props() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, UNIX_EPOCH};

    use gemel_fs::fileinfo::SetAction;
    use gemel_fs::props::Perm;
    let d = Dir::new("props");
    d.write("f", b"x");
    let c = config();
    let p = path("f");
    let new = Props {
        perm: Perm {
            bits: 0o640,
            mask: 0o1777,
        },
        time: Time::Synced(1_600_000_123.0),
        ..Props::dummy()
    };
    fileinfo::set(
        &d.1,
        &p,
        SetAction::Set(&Props::file_default()),
        &new,
        &c,
        &XattrCtx::standalone(),
    )
    .unwrap();
    let m = std::fs::metadata(d.0.join("f")).unwrap();
    assert_eq!(m.permissions().mode() & 0o7777, 0o640);
    assert_eq!(
        m.modified().unwrap(),
        UNIX_EPOCH + Duration::from_secs(1_600_000_123)
    );
    // Update changes only what differs.
    let old = fileinfo::get(
        false,
        &d.1,
        &p,
        NONE,
        &c,
        &Props::dummy(),
        &XattrCtx::standalone(),
    )
    .unwrap()
    .desc;
    let newer = Props {
        perm: Perm {
            bits: 0o600,
            mask: 0o1777,
        },
        ..old.clone()
    };
    fileinfo::set(
        &d.1,
        &p,
        SetAction::Update(&old),
        &newer,
        &c,
        &XattrCtx::standalone(),
    )
    .unwrap();
    assert_eq!(
        std::fs::metadata(d.0.join("f"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o600
    );
    // check notices a mismatch.
    let st = system::stat(d.1.concat(&p).as_bytes()).unwrap();
    let wrong = Props {
        perm: Perm {
            bits: 0o644,
            mask: 0o1777,
        },
        ..newer
    };
    let e = wrong
        .check(&d.1, &p, &st, &c.props, &XattrCtx::standalone())
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("Failed to set permissions of file "), "{e}");
}

#[test]
fn delete_trees_and_rename() {
    let d = Dir::new("delete");
    d.write("t/a/b/c", b"1");
    d.write("t/x", b"2");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(d.0.join("t/a/b"), std::fs::Permissions::from_mode(0o500))
            .unwrap();
    }
    let c = config();
    os::delete(&d.1, &path("t"), &c).unwrap();
    assert!(!d.0.join("t").exists());
    os::delete(&d.1, &path("never"), &c).unwrap();

    d.write("from", b"r");
    os::rename("test", &d.1, &path("from"), &d.1, &path("to"), None, &c).unwrap();
    assert_eq!(std::fs::read(d.0.join("to")).unwrap(), b"r");
    let e = os::rename("test", &d.1, &path("to"), &d.1, &path("to"), None, &c).unwrap_err();
    assert!(
        e.to_string()
            .starts_with("Rename (test): identical source and target ")
    );
    let e = os::rename("test", &d.1, &path("missing"), &d.1, &path("x"), None, &c).unwrap_err();
    assert!(e.to_string().starts_with("Error in renaming "), "{e}");
}

#[test]
fn temp_names_and_children() {
    let d = Dir::new("temp");
    let c = config();
    let names = TempNames::default();
    let t = os::temp_path(true, &d.1, &path("dir/file.txt"), &names, &c).unwrap();
    assert_eq!(t.as_bytes(), b"dir/.unison.file.txt.unison.tmp");
    // An existing temp file makes the next one numbered.
    d.write(".unison.f.unison.tmp", b"");
    let t = os::temp_path(true, &d.1, &path("f"), &names, &c).unwrap();
    assert_eq!(t.as_bytes(), b".unison.f..001..unison.tmp");
    // Long names are shortened with their MD5.
    let long = "é".repeat(100);
    let t = os::temp_path(false, &d.1, &path(&long), &names, &c).unwrap();
    assert!(t.as_bytes().len() <= 143, "{}", t.as_bytes().len());
    assert!(
        std::str::from_utf8(t.as_bytes()).is_ok(),
        "cut inside a character"
    );
    let mut tagged = TempNames::default();
    tagged.include("host-1");
    assert_eq!(tagged.suffix(), ".host-1.unison.tmp");

    d.write("keep", b"");
    d.write("._apple", b"");
    let mut children: Vec<_> = os::children_of(&d.1, &Path::empty(), &names, &c)
        .unwrap()
        .into_iter()
        .map(|n: Name| String::from_utf8(n.as_bytes().to_vec()).unwrap())
        .collect();
    children.sort();
    // Temp files are hidden; AppleDouble files only when rsrc is on.
    assert_eq!(children, ["._apple", "keep"]);
    let rsrc = FsConfig {
        props: PropsConfig {
            rsrc: true,
            ..c.props
        },
        ..c
    };
    let children = os::children_of(&d.1, &Path::empty(), &names, &rsrc).unwrap();
    assert_eq!(children.len(), 1);
    assert!(os::all_children_of(&d.1, &path("nope")).unwrap().is_empty());
}

#[test]
fn fingerprints() {
    let d = Dir::new("fp");
    d.write("hi", b"hi\n");
    d.write("big", &vec![7u8; 300_000]);
    let fp = Fingerprint::file(&d.1, &path("hi")).unwrap();
    assert_eq!(fp.to_string(), "764efa883dda1e11db47671c4a3bbd9e");
    let sub = Fingerprint::subfile(&d.1.concat(&path("big")), 100, 10).unwrap();
    {
        use md5::{Digest, Md5};
        assert_eq!(sub.0, Md5::digest([7u8; 10]).to_vec());
    }
    let e = Fingerprint::subfile(&d.1.concat(&path("big")), 299_995, 10).unwrap_err();
    assert!(e.to_string().ends_with("truncated file"), "{e}");
    let e = Fingerprint::file(&d.1, &path("missing"))
        .unwrap_err()
        .to_string();
    assert!(
        e.starts_with(&format!(
            "Error in digesting {}",
            d.1.concat(&path("missing"))
        )),
        "{e}"
    );
    let c = config();
    let info = fileinfo::get(
        true,
        &d.1,
        &path("hi"),
        NONE,
        &c,
        &Props::dummy(),
        &XattrCtx::standalone(),
    )
    .unwrap();
    let (_, full) = os::safe_fingerprint(
        &d.1,
        &path("hi"),
        &info,
        None,
        NONE,
        &c,
        &XattrCtx::standalone(),
    )
    .unwrap();
    assert_eq!(full.data, fp);
}
