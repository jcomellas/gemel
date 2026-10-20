//! Update detection, on real trees, directly and through the loopback; and
//! against the archives OCaml Unison writes.

mod support;

use std::cell::RefCell;
use std::path::{Path as StdPath, PathBuf};
use std::rc::Rc;

use gemel_base::case::{self, Mode};
use gemel_base::path::{Global, Path};
use gemel_base::prefs::Prefs;
use gemel_engine::archive::{self, Archive, ArchiveData, ArchiveVersion};
use gemel_engine::common::{ContentsChange, PermChange, PrevState, UpdateContent, UpdateItem};
use gemel_engine::globals::{self, Ignore, Roots};
use gemel_engine::name_map::NameMap;
use gemel_engine::update::{self, PathUpdates};
use gemel_engine::xferhint;
use gemel_fs::osx;
use gemel_fs::props::PropsConfig;
use gemel_rpc::host::Host;
use gemel_rpc::registry::{Dispatch, Rpc};
use support::*;

fn fixtures() -> PathBuf {
    StdPath::new(env!("CARGO_MANIFEST_DIR")).join("tests/archives")
}

fn global(s: &str) -> Path<Global> {
    Path::from_bytes(s.as_bytes()).unwrap()
}

/// What OCaml records about the predicates and preferences each scan
/// used is what gemel computes for the same settings, so gemel neither
/// rescans everything nor changes the archive.
#[test]
fn ocamls_recorded_predicates_are_unchanged() {
    let dir = fixtures();
    let mut checked = 0;
    for list in ["linux.names", "macos.names"] {
        for line in std::fs::read_to_string(dir.join(list)).unwrap().lines() {
            let (fixture, _) = line.split_once(' ').unwrap();
            let bytes = std::fs::read(dir.join(fixture)).unwrap();
            let mut it = bytes.split(|&b| b == b'\n');
            let roots_line = String::from_utf8(it.nth(1).unwrap().to_vec()).unwrap();
            let third = String::from_utf8_lossy(it.next().unwrap()).into_owned();
            let (written, _) = third.split_once('\x1e').unwrap();
            let mode = mode_of_desc(
                written
                    .rsplit_once(" - ")
                    .and_then(|(_, m)| m.strip_suffix(" mode"))
                    .unwrap(),
            );
            let (this_root, roots_name) = roots_line
                .strip_prefix("Archive for root ")
                .and_then(|s| s.split_once(" synchronizing roots "))
                .unwrap();

            let mut prefs = new_prefs();
            prefs.set(&archive::ROOTS_NAME, roots_name.to_owned());
            // Stasher.initBackups ignores old backup files, once on each
            // root's host: twice when both roots are local.
            for _ in 0..2 {
                globals::add_regexp_to_ignore(&mut prefs, "Name *.unison.bak");
            }
            settings(mode).store(&mut prefs);
            // Osx.init: resource forks are synchronised when a host runs
            // macOS.
            osx::init(&mut prefs, fixture.starts_with("macos"));
            let paths = if fixture.contains("-paths-") {
                prefs
                    .parse_cmd_line(&[
                        "-path".into(),
                        "src".into(),
                        "-path".into(),
                        "README".into(),
                    ])
                    .unwrap();
                let ignore = Ignore::new(&prefs, mode).unwrap();
                update::filter_paths(prefs.read(&globals::PATHS).clone(), mode, &ignore)
            } else {
                vec![Path::empty()]
            };
            let host = Host::client(
                Rc::new(RefCell::new(prefs)),
                Rc::new(RefCell::new(enabled_features())),
            );
            let all = host.features().borrow().all();
            let data =
                archive::parse_archive_file(&bytes, fixture, &roots_line, &all, mode, &|w| {
                    panic!("{w}")
                })
                .unwrap()
                .unwrap();
            let stamp = *data.props.props.find(&archive::DIR_STAMP_KEY).unwrap();
            archive::set_archive_props_local(&host, this_root, data.props.clone());

            let (rescan, dir_stamp) =
                update::check_no_update_predicate_change(&host, this_root, &paths).unwrap();
            assert!(!rescan, "{fixture}: props rescan");
            assert_eq!(dir_stamp, stamp, "{fixture}: directory stamp");
            let after = ArchiveData {
                props: archive::archive_props(&host, this_root).unwrap(),
                ..data
            };
            let features = host.features().borrow().changing_archive_format();
            let c = PropsConfig::default().with_features(&host.features().borrow());
            assert!(
                archive::archive_file_bytes(&after, &roots_line, written, &features, &c) == bytes,
                "{fixture}: the props changed"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 32);
}

#[test]
fn paths_under_others_and_ignored_ones_are_left_out() {
    let mut prefs = new_prefs();
    prefs
        .parse_cmd_line(&["-ignore".into(), "Name *.o".into()])
        .unwrap();
    let ignore = Ignore::new(&prefs, Mode::Sensitive).unwrap();
    let paths = ["a/b", "c", "a", "x.o", "c/d/e", "b"].map(global).to_vec();
    let kept = update::filter_paths(paths, Mode::Sensitive, &ignore);
    let kept: Vec<_> = kept
        .iter()
        .map(|p| String::from_utf8_lossy(p.as_bytes()).into_owned())
        .collect();
    assert_eq!(kept, ["a", "b", "c"]);
    let everything =
        update::filter_paths(vec![global("a"), Path::empty()], Mode::Sensitive, &ignore);
    assert_eq!(everything, [Path::empty()]);
}

// ---- Scans of real trees ----

const T0: f64 = 1_600_000_000.0;

/// A client with two local roots `a` and `b` in `dir`, archives in `dir/u`.
struct Setup {
    rpc: Rpc,
    roots: Roots,
    base: PathBuf,
}

fn setup(dir: &TempDir, loopback: bool) -> Setup {
    setup_in(dir, loopback, Mode::Sensitive)
}

/// [`setup`] in a case mode. (Roots are canonized when installed, so the
/// directory needn't be.)
fn setup_in(dir: &TempDir, loopback: bool, mode: Mode) -> Setup {
    let base = dir.0.clone();
    let raw = ["a", "b"].map(|n| {
        let p = base.join(n);
        std::fs::create_dir_all(&p).unwrap();
        p.to_str().unwrap().to_owned()
    });
    let unison_dir = base.join("u");
    std::fs::create_dir_all(&unison_dir).unwrap();
    let mut prefs = new_prefs();
    globals::set_raw_roots(&mut prefs, &raw);
    prefs.set(&globals::PATHS, vec![Path::empty()]);
    prefs.set(&gemel_fs::props::TIMES, true);
    settings(mode).store(&mut prefs);
    let client = Rc::new(Host::client(
        Rc::new(RefCell::new(prefs)),
        Rc::new(RefCell::new(new_features())),
    ));
    client.set_unison_dir(&unison_dir);
    client.set_warn_printer(None);
    let dispatch = if loopback {
        let server = Host::loopback_server(
            &client.prefs().borrow(),
            &client.features().borrow(),
            new_prefs(),
            new_features(),
        )
        .unwrap();
        server.set_unison_dir(&unison_dir);
        Dispatch::Loopback {
            client,
            server: Rc::new(server),
        }
    } else {
        Dispatch::Direct(client)
    };
    let mut rpc = Rpc::new(dispatch);
    gemel_engine::register_commands(&mut rpc).unwrap();
    let roots = block_on(async {
        let raw = globals::raw_roots(&rpc.client_host().prefs().borrow());
        let roots = Roots::install(&rpc, &raw).await.unwrap();
        archive::store_roots_name(&rpc, &roots).unwrap();
        globals::propagate_prefs(&rpc, &roots).await.unwrap();
        roots
    });
    Setup { rpc, roots, base }
}

impl Setup {
    fn host(&self) -> &Host {
        self.rpc.local_host()
    }

    fn root(&self, side: &str) -> PathBuf {
        self.base.join(side)
    }

    fn this_root(&self, i: usize) -> String {
        archive::this_roots_global_name(self.host(), &self.roots.list()[i].fspath).unwrap()
    }

    fn find(&self) -> Vec<PathUpdates> {
        block_on(update::find_updates(&self.rpc, &self.roots, false, None)).unwrap()
    }

    /// Changes the client's preferences and sends them to the hosts.
    fn set_prefs(&self, f: impl FnOnce(&mut Prefs)) {
        f(&mut self.rpc.client_host().prefs().borrow_mut());
        block_on(globals::propagate_prefs(&self.rpc, &self.roots)).unwrap();
    }

    /// Stores, for both roots, the archive the updates of the whole
    /// replica make (as a commit would), with the hosts' props.
    fn commit(&self, found: &[PathUpdates]) {
        let [(fa, fb)] = found else {
            panic!("one path")
        };
        let mode = case::mode(&self.host().prefs().borrow());
        for (i, ui) in [(0, &fa.1), (1, &fb.1)] {
            let archive = make_archive(ui, mode);
            let props = archive::archive_props(self.host(), &self.this_root(i)).unwrap();
            let data = ArchiveData {
                archive,
                hash: 7,
                magic: b"case sensitive\x001.0".to_vec(),
                props,
            };
            let (name, _) = archive::archive_name(
                self.host(),
                &self.roots.list()[i].fspath,
                ArchiveVersion::MainArch,
            )
            .unwrap();
            let f = self.host().file_in_unison_dir(&name);
            archive::store_archive_local(self.host(), &f, &self.this_root(i), &data).unwrap();
        }
    }

    /// The archive at `path` in root `i`, as cached on the host.
    fn archived(&self, i: usize, path: &str) -> Archive {
        archive::with_archive(self.host(), &self.this_root(i), |a| {
            update::get_path_in_archive(a, &global(path)).1.clone()
        })
        .unwrap()
    }
}

/// `makeArchive`: the archive a commit of these updates makes from
/// nothing.
fn make_archive(ui: &UpdateItem, mode: Mode) -> Archive {
    match ui {
        UpdateItem::NoUpdates | UpdateItem::Error(_) => Archive::NoArchive,
        UpdateItem::Updates(uc, _) => match uc {
            UpdateContent::Absent => Archive::NoArchive,
            UpdateContent::File(desc, ContentsChange::Updated(fp, stamp, ress)) => {
                Archive::File(desc.clone(), fp.clone(), *stamp, ress.clone())
            }
            UpdateContent::File(_, ContentsChange::Same) => panic!("no previous file"),
            UpdateContent::Symlink(l) => Archive::Symlink(l.clone()),
            UpdateContent::Dir(desc, children, _, _) => Archive::Dir(
                desc.clone(),
                NameMap::from_bindings(
                    mode,
                    children
                        .iter()
                        .map(|(n, ui)| (n.clone(), make_archive(ui, mode)))
                        .filter(|(_, a)| *a != Archive::NoArchive),
                ),
            ),
        },
    }
}

fn children(ui: &UpdateItem) -> Vec<(String, &UpdateItem)> {
    match ui {
        UpdateItem::Updates(UpdateContent::Dir(_, ch, _, _), _) => ch
            .iter()
            .map(|(n, ui)| (String::from_utf8_lossy(n.as_bytes()).into_owned(), ui))
            .collect(),
        other => panic!("not a directory: {other:?}"),
    }
}

fn child<'a>(ui: &'a UpdateItem, name: &str) -> &'a UpdateItem {
    children(ui)
        .into_iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("no update for {name} in {ui:?}"))
        .1
}

fn names(ui: &UpdateItem) -> Vec<String> {
    children(ui).into_iter().map(|(n, _)| n).collect()
}

fn md5(s: &str) -> Vec<u8> {
    <md5::Md5 as md5::Digest>::digest(s.as_bytes()).to_vec()
}

/// The replica: files, an empty directory and (on Unix) a symbolic link,
/// all with times in the past; directories' times are set last.
fn build_tree(root: &StdPath) {
    write(root, "README", "read me", T0);
    write(root, "src/main.c", "int main(void) { return 0; }", T0 + 1.0);
    write(root, "src/deep/x", "x", T0 + 2.0);
    std::fs::create_dir_all(root.join("empty")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("README", root.join("link")).unwrap();
    for d in ["src/deep", "src", "empty", ""] {
        set_mtime(&root.join(d), T0 + 10.0);
    }
}

#[test]
fn scans_find_changes_against_the_archive() {
    for loopback in [false, true] {
        let dir = TempDir::new("scan");
        let s = setup(&dir, loopback);
        build_tree(&s.root("a"));

        // No archive: everything in a is new; b is an empty directory.
        let found = s.find();
        let [(fa, fb)] = &found[..] else { panic!() };
        assert_eq!((fa.0.as_bytes(), fb.0.as_bytes()), (&b""[..], &b""[..]));
        let expected: &[&str] = if cfg!(unix) {
            &["README", "empty", "link", "src"]
        } else {
            &["README", "empty", "src"]
        };
        assert_eq!(names(&fa.1), expected);
        assert!(matches!(
            &fa.1,
            UpdateItem::Updates(
                UpdateContent::Dir(_, _, PermChange::Updated, false),
                PrevState::New
            )
        ));
        let UpdateItem::Updates(
            UpdateContent::File(desc, ContentsChange::Updated(fp, _, _)),
            PrevState::New,
        ) = child(&fa.1, "README")
        else {
            panic!("{:?}", child(&fa.1, "README"))
        };
        assert_eq!(fp.data.0, md5("read me"));
        assert_eq!(desc.length(), 7);
        assert_eq!(names(child(&fa.1, "src")), ["deep", "main.c"]);
        if cfg!(unix) {
            assert_eq!(
                child(&fa.1, "link"),
                &UpdateItem::Updates(UpdateContent::Symlink(b"README".to_vec()), PrevState::New)
            );
        }
        assert!(names(&fb.1).is_empty());

        // Both replicas the same, as after a sync.
        build_tree(&s.root("b"));
        let found = s.find();
        s.commit(&found);
        let found = s.find();
        for (fa, fb) in &found {
            assert_eq!(
                (&fa.1, &fb.1),
                (&UpdateItem::NoUpdates, &UpdateItem::NoUpdates)
            );
        }
        // Each fingerprint was last seen in b, the root scanned last in
        // OCaml's (canonical) order.
        let main_c = gemel_fs::os::FullFingerprint {
            data: gemel_fs::fingerprint::Fingerprint(md5("int main(void) { return 0; }")),
            ress: gemel_fs::fingerprint::Fingerprint::DUMMY,
        };
        let (hint_root, hint_path) = xferhint::lookup(s.host(), &main_c).unwrap();
        assert_eq!(hint_root, s.roots.list()[1].fspath);
        assert_eq!(hint_path.as_bytes(), b"src/main.c");

        // Changes in a.
        write(&s.root("a"), "README", "read me again", T0 + 100.0);
        write(
            &s.root("a"),
            "src/main.c",
            "int main(void) { return 0; }",
            T0 + 200.0,
        );
        write(&s.root("a"), "new", "new", T0 + 300.0);
        write(&s.root("a"), "junk.tmp", "junk", T0 + 300.0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                s.root("a").join("src/deep/x"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            std::fs::remove_file(s.root("a").join("link")).unwrap();
        }
        s.set_prefs(|p| {
            p.parse_cmd_line(&["-ignore".into(), "Name *.tmp".into()])
                .unwrap()
        });
        let found = s.find();
        let [(fa, fb)] = &found[..] else { panic!() };
        assert_eq!(fb.1, UpdateItem::NoUpdates);
        let expected: &[&str] = if cfg!(unix) {
            &["README", "link", "new", "src"]
        } else {
            &["README", "new"]
        };
        assert_eq!(names(&fa.1), expected, "{:?}", fa.1);
        assert!(matches!(
            &fa.1,
            UpdateItem::Updates(
                UpdateContent::Dir(_, _, PermChange::Same, false),
                PrevState::Dir(_)
            )
        ));
        let UpdateItem::Updates(
            UpdateContent::File(_, ContentsChange::Updated(fp, _, _)),
            PrevState::File(..),
        ) = child(&fa.1, "README")
        else {
            panic!()
        };
        assert_eq!(fp.data.0, md5("read me again"));
        assert!(matches!(
            child(&fa.1, "new"),
            UpdateItem::Updates(
                UpdateContent::File(_, ContentsChange::Updated(..)),
                PrevState::New
            )
        ));
        if cfg!(unix) {
            assert!(matches!(
                child(&fa.1, "link"),
                UpdateItem::Updates(UpdateContent::Absent, PrevState::Symlink)
            ));
            let deep = child(child(&fa.1, "src"), "deep");
            assert!(matches!(
                child(deep, "x"),
                UpdateItem::Updates(
                    UpdateContent::File(_, ContentsChange::Same),
                    PrevState::File(..)
                )
            ));
        }
        // Only main.c's time changed, which `times` synchronises.
        assert!(matches!(
            child(child(&fa.1, "src"), "main.c"),
            UpdateItem::Updates(
                UpdateContent::File(_, ContentsChange::Same),
                PrevState::File(..)
            )
        ));
    }
}

/// Without `times`, a file whose time alone changed has no update, and
/// the archive takes the new time.
#[test]
fn new_times_go_to_the_archive() {
    let dir = TempDir::new("times");
    let s = setup(&dir, true);
    build_tree(&s.root("a"));
    build_tree(&s.root("b"));
    let found = s.find();
    s.commit(&found);
    s.set_prefs(|p| p.set(&gemel_fs::props::TIMES, false));
    write(
        &s.root("a"),
        "src/main.c",
        "int main(void) { return 0; }",
        T0 + 200.0,
    );
    let found = s.find();
    assert_eq!(found[0].0.1, UpdateItem::NoUpdates);
    let Archive::File(desc, ..) = s.archived(0, "src/main.c") else {
        panic!()
    };
    assert_eq!(desc.time(), T0 + 200.0);
}

#[test]
fn paths_are_checked_on_the_way_down() {
    let dir = TempDir::new("paths");
    let s = setup(&dir, true);
    build_tree(&s.root("a"));
    s.set_prefs(|p| {
        p.set(
            &globals::PATHS,
            vec![global("README/x"), global("src/deep")],
        )
    });
    let found = s.find();
    // In order: README/x, then src/deep.
    let [(a2, b2), (a1, b1)] = &found[..] else {
        panic!("{found:?}")
    };
    assert_eq!(
        a2.1,
        UpdateItem::Error(
            "path README/x is not valid because README is not a directory in one of the \
             replicas"
                .into()
        )
    );
    assert_eq!(b2.1, UpdateItem::NoUpdates);
    // src/deep: new in a, with the props of a and src on the way.
    assert_eq!(a1.0.as_bytes(), b"src/deep");
    assert_eq!(names(&a1.1), ["x"]);
    assert_eq!(a1.2.len(), 2);
    // In b, src is missing: only the root's props.
    assert_eq!(b1.1, UpdateItem::NoUpdates);
    assert_eq!(b1.2.len(), 1);
}

/// On Unix, a directory marked unchanged by a scan isn't read again while
/// its time is the same: a file added without changing the time goes
/// unnoticed, unless fast checking is off.
#[cfg(unix)]
#[test]
fn unchanged_directories_are_not_read() {
    let dir = TempDir::new("dirfast");
    let s = setup(&dir, false);
    build_tree(&s.root("a"));
    build_tree(&s.root("b"));
    let found = s.find();
    s.commit(&found);
    // This scan marks the directories; the next one trusts the marks.
    assert!(
        s.find()
            .iter()
            .all(|(a, b)| a.1 == UpdateItem::NoUpdates && b.1 == UpdateItem::NoUpdates)
    );
    write(&s.root("a"), "empty/hidden", "h", T0);
    set_mtime(&s.root("a").join("empty"), T0 + 10.0);
    let found = s.find();
    assert_eq!(found[0].0.1, UpdateItem::NoUpdates);
    s.set_prefs(|p| {
        p.parse_cmd_line(&["-fastcheck".into(), "false".into()])
            .unwrap()
    });
    let found = s.find();
    assert_eq!(names(child(&found[0].0.1, "empty")), ["hidden"]);
}

/// OCaml Unison syncs a tree twice (so that its archives mark directories
/// unchanged); gemel then finds nothing to do in its archives, and finds
/// what changes after. Needs OCaml Unison: set `GEMEL_REFERENCE_UNISON`
/// to its path (the CI `oracle` job does).
#[test]
fn ocamls_archives_show_no_updates() {
    let Some(unison) = std::env::var_os("GEMEL_REFERENCE_UNISON") else {
        eprintln!("skipped: set GEMEL_REFERENCE_UNISON to an OCaml unison");
        return;
    };
    for loopback in [false, true] {
        let dir = TempDir::new("reference");
        let base = std::fs::canonicalize(&dir.0).unwrap();
        for side in ["a", "b"] {
            std::fs::create_dir_all(base.join(side)).unwrap();
        }
        build_tree(&base.join("a"));
        for _ in 0..2 {
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
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let s = setup(&dir, loopback);
        // What OCaml's startup sets for these hosts.
        s.set_prefs(|p| {
            let osx_host = cfg!(target_os = "macos");
            case::init(p, osx_host || cfg!(windows), osx_host);
            osx::init(p, osx_host);
            for _ in 0..2 {
                globals::add_regexp_to_ignore(p, "Name *.unison.bak");
            }
        });
        let found = s.find();
        assert!(archive::found_archives(&s.rpc));
        let [(fa, fb)] = &found[..] else { panic!() };
        assert_eq!(
            (&fa.1, &fb.1),
            (&UpdateItem::NoUpdates, &UpdateItem::NoUpdates)
        );

        write(&s.root("a"), "README", "read me again", T0 + 100.0);
        write(&s.root("b"), "src/new", "new", T0 + 100.0);
        let found = s.find();
        let [(fa, fb)] = &found[..] else { panic!() };
        assert_eq!(names(&fa.1), ["README"], "{:?}", fa.1);
        assert_eq!(names(&fb.1), ["src"], "{:?}", fb.1);
        assert_eq!(names(child(&fb.1, "src")), ["new"]);
    }
}

/// In a case-insensitive mode, a name respelled on disk is the same file,
/// and the archive takes the new spelling.
#[test]
fn respelled_names_are_the_same_file() {
    for loopback in [false, true] {
        let dir = TempDir::new("respell");
        let s = setup_in(&dir, loopback, Mode::UnicodeInsensitive);
        for side in ["a", "b"] {
            write(&s.root(side), "readme", "read me", T0);
            set_mtime(&s.root(side), T0 + 10.0);
        }
        let found = s.find();
        s.commit(&found);
        std::fs::rename(s.root("a").join("readme"), s.root("a").join("README")).unwrap();
        let found = s.find();
        assert_eq!(found[0].0.1, UpdateItem::NoUpdates);
        let (spelled, a) = archive::with_archive(s.host(), &s.this_root(0), |a| {
            let (p, a) = update::get_path_in_archive(a, &global("readme"));
            (p, a.clone())
        })
        .unwrap();
        assert_eq!(spelled.as_bytes(), b"README");
        assert!(matches!(a, Archive::File(..)));
    }
}

/// In a case-insensitive mode, two names differing only in case can't be
/// synchronised (where the filesystem can hold both).
#[test]
fn names_equal_but_for_case_are_errors() {
    let dir = TempDir::new("dup");
    let s = setup_in(&dir, false, Mode::UnicodeInsensitive);
    write(&s.root("a"), "A", "upper", T0);
    write(&s.root("a"), "a", "lower", T0);
    if std::fs::read_dir(s.root("a")).unwrap().count() < 2 {
        eprintln!("skipped: the filesystem is case-insensitive");
        return;
    }
    let found = s.find();
    let ch = children(&found[0].0.1);
    assert_eq!(ch.len(), 1, "{ch:?}");
    let UpdateItem::Error(e) = ch[0].1 else {
        panic!("{ch:?}")
    };
    assert!(e.starts_with("Two or more files on a case-sensitive system have names identical"));
}

/// `follow`: a followed link is scanned as what it points to.
#[cfg(unix)]
#[test]
fn followed_links_are_their_targets() {
    let dir = TempDir::new("follow");
    let s = setup(&dir, true);
    build_tree(&s.root("a"));
    s.set_prefs(|p| {
        p.parse_cmd_line(&["-follow".into(), "Path link".into()])
            .unwrap()
    });
    let found = s.find();
    let UpdateItem::Updates(
        UpdateContent::File(_, ContentsChange::Updated(fp, _, _)),
        PrevState::New,
    ) = child(&found[0].0.1, "link")
    else {
        panic!("{:?}", child(&found[0].0.1, "link"))
    };
    assert_eq!(fp.data.0, md5("read me"));
}
