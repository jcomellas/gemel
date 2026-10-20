//! Commits and the other changes to archives after update detection,
//! directly and through the loopback; and OCaml Unison reading what gemel
//! committed.

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use gemel_base::case::{self, Mode};
use gemel_base::name::Name;
use gemel_base::path::{Local, Path};
use gemel_engine::archive::{self, Archive, ArchiveVersion};
use gemel_engine::common::{UpdateContent, UpdateItem};
use gemel_engine::globals;
use gemel_engine::name_map::NameMap;
use gemel_engine::tree::Tree;
use gemel_engine::update;
use gemel_fs::fileinfo::Stamp;
use gemel_fs::osx;
use gemel_fs::props::{Perm, Props, PropsConfig};
use gemel_fs::system;
use support::sync::*;
use support::*;

fn local(s: &str) -> Path<Local> {
    Path::from_bytes(s.as_bytes()).unwrap()
}

fn name(s: &str) -> Name {
    Name::from_bytes(s).unwrap()
}

/// Two roots with the same tree, scanned and committed.
fn committed(dir: &TempDir, loopback: bool, mode: Mode) -> Setup {
    let s = setup_in(dir, loopback, mode);
    build_tree(&s.root("a"));
    build_tree(&s.root("b"));
    let found = s.find();
    s.commit(&found);
    s
}

impl Setup {
    fn file(&self, i: usize, v: ArchiveVersion) -> std::path::PathBuf {
        let (name, _) =
            archive::archive_name(self.host(), &self.roots.list()[i].fspath, v).unwrap();
        system::os_path(&self.host().file_in_unison_dir(&name))
    }

    fn fspath(&self, i: usize) -> &gemel_fs::fspath::Fspath {
        &self.roots.list()[i].fspath
    }
}

#[test]
fn commits_write_the_same_archive_on_both_roots() {
    for loopback in [false, true] {
        let dir = TempDir::new("commit");
        let s = committed(&dir, loopback, Mode::Sensitive);
        let mut hashes = Vec::new();
        for i in 0..2 {
            assert!(!s.file(i, ArchiveVersion::ScratchArch).exists());
            assert!(!s.file(i, ArchiveVersion::NewArch).exists());
            assert!(!s.file(i, ArchiveVersion::Lock).exists(), "unlocked");
            let f = system::name_bytes(s.file(i, ArchiveVersion::MainArch).as_os_str());
            let data = archive::load_archive_local(s.host(), &f, &s.this_root(i))
                .unwrap()
                .unwrap();
            let c = PropsConfig::default().with_features(&s.host().features().borrow());
            assert_eq!(
                archive::check_archive(&data.archive, Mode::Sensitive, &c).unwrap(),
                data.hash
            );
            assert!(data.magic.starts_with(b"case sensitive\0"));
            assert!(data.props.props.mem(&archive::DIR_STAMP_KEY));
            hashes.push((data.hash, data.magic));
        }
        assert_eq!(hashes[0], hashes[1]);
        // Nothing changed since.
        for (a, b) in s.find() {
            assert_eq!((a.1, b.1), (UpdateItem::NoUpdates, UpdateItem::NoUpdates));
        }
        // Sizes, by the local root's archive: 4 directories, 3 files of 36
        // bytes, and on Unix a link.
        let size =
            update::update_size(&s.rpc, &s.roots, &Path::empty(), &UpdateItem::NoUpdates).unwrap();
        assert_eq!(size, (if cfg!(unix) { 8 } else { 7 }, 36));
    }
}

#[test]
fn different_archives_are_not_committed() {
    let dir = TempDir::new("mismatch");
    let s = committed(&dir, true, Mode::Sensitive);
    let before = std::fs::read(s.file(0, ArchiveVersion::MainArch)).unwrap();
    // Root b no longer has README.
    update::replace_archive_local(s.host(), s.fspath(1), &local("README"), Archive::NoArchive)
        .unwrap();
    let e = block_on(update::commit_updates(&s.rpc, &s.roots))
        .unwrap_err()
        .to_string();
    assert_eq!(
        e,
        "Internal error: New archives are not identical.\nRetaining original archives.  \
         Please run Unison again to bring them up to date.\n"
    );
    assert_eq!(
        std::fs::read(s.file(0, ArchiveVersion::MainArch)).unwrap(),
        before
    );
    assert!(!s.file(0, ArchiveVersion::NewArch).exists());
    assert!(!s.file(0, ArchiveVersion::Lock).exists(), "unlocked");
}

#[test]
fn destinations_are_checked_before_writing() {
    for loopback in [false, true] {
        let dir = TempDir::new("check");
        let s = committed(&dir, loopback, Mode::Sensitive);
        let (host, fspath) = (s.host(), s.fspath(0));
        let readme = local("README");
        let check = |fast| {
            block_on(update::check_no_updates(
                host,
                fspath,
                &readme,
                &UpdateItem::NoUpdates,
                fast,
            ))
        };

        // Unchanged: the archive as it is.
        assert_eq!(check(false).unwrap(), s.archived(0, "README"));

        // Changed contents.
        write(&s.root("a"), "README", "read me twice", T0 + 50.0);
        assert_eq!(
            check(false).unwrap_err().to_string(),
            "Destination updated during synchronization\nThe contents of file README have \
             been modified\n"
        );

        // Changed contents that fast checking can't see (same time and
        // length): reported with a hint, and marked to be read again.
        write(&s.root("a"), "README", "read us", T0);
        let e = check(false).unwrap_err().to_string();
        assert!(
            e.ends_with("try running once with 'fastcheck' set to false)"),
            "{e}"
        );
        let Archive::File(_, _, stamp, _) = s.archived(0, "README") else {
            panic!()
        };
        assert_eq!(stamp, Stamp::Rescan);
    }
}

#[test]
fn archives_are_changed_after_propagation() {
    let dir = TempDir::new("props");
    let s = committed(&dir, true, Mode::Sensitive);
    let (host, fspath) = (s.host(), s.fspath(0));
    // Propagated permissions.
    let mut desc = Props::template(Perm {
        bits: 0o600,
        mask: 0o777,
    });
    desc.length = 7;
    update::update_props(
        host,
        fspath,
        &local("README"),
        Some(&desc),
        &UpdateItem::NoUpdates,
    )
    .unwrap();
    let Archive::File(p, ..) = s.archived(0, "README") else {
        panic!()
    };
    assert_eq!(p.perm.extract(), 0o600);

    // `updateArchive`: the stripped archive with an update applied.
    let a = update::update_archive(host, fspath, &local("src"), &UpdateItem::NoUpdates).unwrap();
    let Archive::Dir(_, children) = &a else {
        panic!("{a:?}")
    };
    assert_eq!(children.len(), 2);
    let gone = update::update_archive(
        host,
        fspath,
        &local("src"),
        &UpdateItem::Updates(UpdateContent::Absent, gemel_engine::common::PrevState::New),
    )
    .unwrap();
    assert_eq!(gone, Archive::NoArchive);

    // `iterFiles`: every file, with its path.
    let mut files = Vec::new();
    update::iter_files(fspath, &local("src"), &a, &mut |_, p, _| {
        files.push(String::from_utf8_lossy(p.as_bytes()).into_owned())
    });
    assert_eq!(files, ["src/deep/x", "src/main.c"]);
}

#[test]
fn equal_changes_are_marked_on_both_roots() {
    let dir = TempDir::new("equal");
    let s = committed(&dir, false, Mode::Sensitive);
    let stashed = Rc::new(RefCell::new(Vec::new()));
    let st = stashed.clone();
    update::set_stasher_fun(
        s.host(),
        Rc::new(move |_, fspath, path| {
            st.borrow_mut().push((
                fspath.clone(),
                String::from_utf8_lossy(path.as_bytes()).into_owned(),
            ));
            Ok(())
        }),
    );
    // README and src/main.c changed the same way on both roots.
    write(&s.root("a"), "README", "same change", T0 + 60.0);
    write(&s.root("b"), "README", "same change", T0 + 60.0);
    write(&s.root("a"), "src/main.c", "int x;", T0 + 60.0);
    write(&s.root("b"), "src/main.c", "int x;", T0 + 60.0);
    let found = s.find();
    let [(fa, fb)] = &found[..] else { panic!() };
    let content = |ui: &UpdateItem, p: &str| -> UpdateContent {
        let mut ui = ui;
        for n in p.split('/') {
            ui = child(ui, n);
        }
        let UpdateItem::Updates(uc, _) = ui else {
            panic!("{ui:?}")
        };
        uc.clone()
    };
    let leaf = |p: &str| Tree::Leaf((content(&fa.1, p), content(&fb.1, p)));
    let tree = Tree::Node(
        vec![
            ((name("README"), name("README")), leaf("README")),
            (
                (name("src"), name("src")),
                Tree::Node(
                    vec![((name("main.c"), name("main.c")), leaf("src/main.c"))],
                    None,
                ),
            ),
        ],
        None,
    );
    block_on(update::mark_equal(&s.rpc, &s.roots, &tree)).unwrap();
    let paths: Vec<_> = stashed
        .borrow()
        .iter()
        .map(|(f, p)| (f == s.fspath(0), p.clone()))
        .collect();
    assert_eq!(
        paths,
        [
            (true, "README".to_owned()),
            (true, "src/main.c".to_owned()),
            (false, "README".to_owned()),
            (false, "src/main.c".to_owned()),
        ]
    );
    // Now in the archives: no more updates.
    for (a, b) in s.find() {
        assert_eq!((a.1, b.1), (UpdateItem::NoUpdates, UpdateItem::NoUpdates));
    }

    // A failed backup leaves the archive as it was.
    let before = s.archived(0, "");
    update::set_stasher_fun(
        s.host(),
        Rc::new(|_, _, _| Err(gemel_base::util::Error::Transient("no backup".into()))),
    );
    let e = block_on(update::mark_equal(&s.rpc, &s.roots, &tree)).unwrap_err();
    assert_eq!(e.to_string(), "no backup");
    assert_eq!(s.archived(0, ""), before);
}

#[test]
fn replaced_archives_are_sorted_by_the_hosts_mode() {
    let dir = TempDir::new("replace");
    let s = committed(&dir, true, Mode::UnicodeInsensitive);
    let leaf = || Archive::Symlink(b"t".to_vec());
    // Built in case-sensitive order: "B" before "a".
    let new = Archive::Dir(
        Props::dir_default(),
        NameMap::from_bindings(Mode::Sensitive, [(name("B"), leaf()), (name("a"), leaf())]),
    );
    let root = s.roots.list()[0].clone();
    block_on(update::replace_archive(&s.rpc, &root, global("src"), new)).unwrap();
    let Archive::Dir(_, children) = s.archived(0, "src") else {
        panic!()
    };
    assert_eq!(children.mode(), Mode::UnicodeInsensitive);
    let order: Vec<_> = children
        .iter()
        .map(|(n, _)| n.as_bytes().to_vec())
        .collect();
    assert_eq!(order, [b"a".to_vec(), b"B".to_vec()]);
}

#[test]
fn paths_are_updated_in_the_archive() {
    let mode = Mode::Sensitive;
    let leaf = || Archive::Symlink(b"t".to_vec());
    let mut a = Archive::Dir(
        Props::dir_default(),
        NameMap::from_bindings(mode, [(name("f"), leaf())]),
    );
    let set = |a: &mut Archive, p: &str, new: Archive| {
        let mut new = Some(new);
        update::update_path_in_archive(a, &Path::empty(), &local(p), mode, &mut |x, _| {
            *x = new.take().unwrap();
            Ok(())
        })
        .unwrap();
    };
    // Under a symlink: a made-up directory, which goes when emptied.
    set(&mut a, "f/g", leaf());
    assert!(matches!(
        update::get_path_in_archive(&a, &local("f")).1,
        Archive::Dir(p, _) if *p == Props::dummy()
    ));
    set(&mut a, "f/g", Archive::NoArchive);
    assert_eq!(
        *update::get_path_in_archive(&a, &local("f")).1,
        Archive::NoArchive
    );
    // A real directory stays when emptied.
    set(
        &mut a,
        "d",
        Archive::Dir(Props::dir_default(), NameMap::new(mode)),
    );
    set(&mut a, "d/x", leaf());
    set(&mut a, "d/x", Archive::NoArchive);
    assert!(matches!(
        update::get_path_in_archive(&a, &local("d")).1,
        Archive::Dir(..)
    ));
    // A failed action leaves the archive as it was.
    let before = a.clone();
    let r =
        update::update_path_in_archive(&mut a, &Path::empty(), &local("d/x"), mode, &mut |_, _| {
            Err(gemel_base::util::Error::Transient("no".into()))
        });
    assert!(r.is_err());
    assert_eq!(a, before);
}

/// The done criterion of phase 4: OCaml Unison accepts what gemel
/// committed, and has nothing to do. Needs OCaml Unison: set
/// `GEMEL_REFERENCE_UNISON` to its path (the CI `oracle` job does).
#[test]
fn ocaml_accepts_what_gemel_commits() {
    let Some(unison) = std::env::var_os("GEMEL_REFERENCE_UNISON") else {
        eprintln!("skipped: set GEMEL_REFERENCE_UNISON to an OCaml unison");
        return;
    };
    for loopback in [false, true] {
        let dir = TempDir::new("accepted");
        let s = setup_in(&dir, loopback, Mode::Sensitive);
        // What OCaml's startup sets for these hosts.
        s.set_prefs(|p| {
            let osx_host = cfg!(target_os = "macos");
            case::init(p, osx_host || cfg!(windows), osx_host);
            osx::init(p, osx_host);
            for _ in 0..2 {
                globals::add_regexp_to_ignore(p, "Name *.unison.bak");
            }
        });
        build_tree(&s.root("a"));
        build_tree(&s.root("b"));
        let found = s.find();
        s.commit(&found);
        // The hash each archive was committed with.
        let hash = |i: usize| {
            let f = system::name_bytes(s.file(i, ArchiveVersion::MainArch).as_os_str());
            archive::load_archive_local(s.host(), &f, &s.this_root(i))
                .unwrap()
                .unwrap()
                .hash
        };
        let committed = [hash(0), hash(1)];
        let ocaml = |flags: &[&str]| {
            let out = std::process::Command::new(&unison)
                .arg(s.base.join("a"))
                .arg(s.base.join("b"))
                .args([
                    "-batch",
                    "-ui",
                    "text",
                    "-log=false",
                    "-contactquietly",
                    "-times",
                ])
                .args(flags)
                .env("UNISON", s.base.join("u"))
                .env("HOME", &s.base)
                .output()
                .unwrap();
            let all = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(out.status.success(), "{all}");
            for bad in ["Warning", "mismatch", "not identical", "No archive files"] {
                assert!(!all.contains(bad), "{all}");
            }
            all
        };
        let nothing = "Nothing to do: replicas have not changed since last sync.";
        // With fast checking, OCaml trusts gemel's times, lengths and
        // inodes. It commits even with nothing to do; the checkArchive hash
        // it stores (of the names, fingerprints and synchronised props) is
        // gemel's, so it kept what gemel recorded.
        assert!(ocaml(&[]).contains(nothing));
        assert_eq!([hash(0), hash(1)], committed);
        // Without, it checks every fingerprint and the props.
        assert!(ocaml(&["-fastcheck", "false"]).contains(nothing));
        // And it syncs on from gemel's archives.
        write(&s.root("a"), "README", "read me again", T0 + 100.0);
        ocaml(&[]);
        assert_eq!(
            std::fs::read_to_string(s.root("b").join("README")).unwrap(),
            "read me again"
        );
    }
}
