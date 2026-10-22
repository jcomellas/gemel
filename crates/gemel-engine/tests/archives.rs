//! Archives: real ones written by OCaml Unison (`tests/archives/`, see
//! `scripts/gen-archive-fixtures.sh`), and the archive commands run on both
//! roots, directly and through the loopback.
//!
//! For each real archive, the test checks that:
//! - its name is the hash OCaml computed;
//! - it loads, with every directory's children in the case mode's order;
//! - `checkArchive` gives the hash stored in it (this exercises
//!   `Props.hash`, `Name.hash` and the fingerprint hash);
//! - storing it again gives the same bytes, apart from the time it was
//!   written.

mod support;

use std::cell::RefCell;
use std::path::{Path as StdPath, PathBuf};
use std::rc::Rc;

use gemel_base::case::Mode;
use gemel_base::name::Name;
use gemel_base::proplist::Proplist;
use gemel_base::util::Error;
use gemel_engine::archive::{self, Archive, ArchiveData, ArchiveProps, ArchiveVersion};
use gemel_engine::globals::{self, Roots};
use gemel_engine::name_map::NameMap;
use gemel_fs::fileinfo::Stamp;
use gemel_fs::fingerprint::Fingerprint;
use gemel_fs::fspath::Fspath;
use gemel_fs::os::{self, FullFingerprint};
use gemel_fs::osx::RessStamp;
use gemel_fs::props::{Perm, Props, PropsConfig};
use gemel_fs::system;
use gemel_rpc::host::Host;
use gemel_rpc::registry::{Dispatch, Rpc};
use support::*;

fn fixtures() -> PathBuf {
    StdPath::new(env!("CARGO_MANIFEST_DIR")).join("tests/archives")
}

fn lines(bytes: &[u8]) -> [String; 3] {
    let mut it = bytes.split(|&b| b == b'\n');
    [(); 3].map(|()| String::from_utf8(it.next().unwrap().to_vec()).unwrap())
}

/// An archive file without the time it was written at (in "Written at
/// <time> - <mode> mode" on the third line).
fn without_time(bytes: &[u8]) -> Vec<u8> {
    let mut at = 0;
    for _ in 0..2 {
        at += bytes[at..].iter().position(|&b| b == b'\n').unwrap() + 1;
    }
    let line = &bytes[at..];
    let dash = line.windows(3).position(|w| w == b" - ").unwrap();
    [&bytes[..at], b"Written at", &line[dash..]].concat()
}

/// (directories, files, symbolic links).
fn count(a: &Archive) -> (usize, usize, usize) {
    match a {
        Archive::Dir(_, children) => children.iter().fold((1, 0, 0), |(d, f, l), (_, c)| {
            let (d2, f2, l2) = count(c);
            (d + d2, f + f2, l + l2)
        }),
        Archive::File(..) => (0, 1, 0),
        Archive::Symlink(_) => (0, 0, 1),
        Archive::NoArchive => (0, 0, 0),
    }
}

#[test]
fn real_archives_load_check_and_round_trip() {
    let dir = fixtures();
    let mut names = Vec::new();
    for list in ["linux.names", "macos.names"] {
        let text = std::fs::read_to_string(dir.join(list)).unwrap();
        for line in text.lines() {
            let (fixture, name) = line.split_once(' ').unwrap();
            names.push((fixture.to_owned(), name.to_owned()));
        }
    }
    assert!(names.len() >= 32, "{} archives", names.len());
    let mut modes = std::collections::HashSet::new();
    for (fixture, recorded) in names {
        let file = dir.join(&fixture);
        let bytes = std::fs::read(&file).unwrap();
        let [_, roots_line, written_line] = lines(&bytes);
        let (this_root, roots_name) = roots_line
            .strip_prefix("Archive for root ")
            .and_then(|s| s.split_once(" synchronizing roots "))
            .unwrap();
        let (written, _) = written_line.split_once('\x1e').unwrap();
        let mode = mode_of_desc(
            written
                .rsplit_once(" - ")
                .and_then(|(_, m)| m.strip_suffix(" mode"))
                .unwrap(),
        );
        modes.insert(mode);

        // The host that wrote it: "//<host>/<path>".
        let host_name = this_root[2..].split('/').next().unwrap();
        let fspath = Fspath::from_canonical(this_root.as_bytes()[3 + host_name.len()..].to_vec());
        let mut prefs = new_prefs();
        prefs.set(&os::CLIENT_HOST_NAME, host_name.to_owned());
        prefs.set(&archive::ROOTS_NAME, roots_name.to_owned());
        settings(mode).store(&mut prefs);
        let host = Host::client(
            Rc::new(RefCell::new(prefs)),
            Rc::new(RefCell::new(enabled_features())),
        );
        let (name, root) = archive::archive_name(&host, &fspath, ArchiveVersion::MainArch).unwrap();
        assert_eq!(
            (name.as_str(), root.as_str()),
            (recorded.as_str(), this_root),
            "{fixture}"
        );

        let f = system::name_bytes(file.as_os_str());
        let data = archive::load_archive_local(&host, &f, this_root)
            .unwrap()
            .unwrap_or_else(|| panic!("{fixture} was not loaded"));
        let c = PropsConfig::default().with_features(&host.features().borrow());
        assert_eq!(
            archive::check_archive(&data.archive, mode, &c).unwrap(),
            data.hash,
            "{fixture}: checkArchive"
        );
        assert_eq!(
            archive::archive_mode(&data.magic, mode),
            mode.desc().as_bytes()
        );
        assert!(data.props.props.mem(&archive::CASE_KEY), "{fixture}");
        assert!(!data.props.paths.is_empty(), "{fixture}: paths");

        // OCaml writes the features in this order; so does gemel.
        let features = host.features().borrow().changing_archive_format();
        assert_eq!(features, ["Sync: ACL", "Sync: xattr"]);
        let again = archive::archive_file_bytes(&data, &roots_line, written, &features, &c);
        assert!(again == bytes, "{fixture}: stored again, the bytes differ");
        // And through the file, with the header derived from the host.
        let out = std::env::temp_dir().join(format!(
            "gemel-archive-store-{}-{fixture}",
            std::process::id()
        ));
        let o = system::name_bytes(out.as_os_str());
        archive::store_archive_local(&host, &o, this_root, &data).unwrap();
        let stored = std::fs::read(&out).unwrap();
        let _ = std::fs::remove_file(&out);
        assert!(
            without_time(&stored) == without_time(&bytes),
            "{fixture}: stored to a file, the bytes differ"
        );

        let expected = if fixture.contains("-paths-") {
            // Only src and README were synchronised.
            (4, 3, 0)
        } else {
            // After the second sync: 6 directories (including the root), 8
            // files (README, alpha, café, with space, new, src/main.c,
            // src/deep/er/x, 日本/語) and a symlink.
            (6, 8, 1)
        };
        assert_eq!(count(&data.archive), expected, "{fixture}");
    }
    assert_eq!(modes.len(), 4, "every case mode is covered: {modes:?}");
}

// ---- Headers ----

fn sample_data(mode: Mode) -> ArchiveData {
    let file = |len, data: &[u8]| {
        let mut p = Props::template(Perm::DUMMY);
        p.length = len;
        Archive::File(
            p,
            FullFingerprint {
                data: Fingerprint(data.to_vec()),
                ress: Fingerprint::DUMMY,
            },
            Stamp::Inode(42),
            RessStamp::NoRess,
        )
    };
    let name = |s: &str| Name::from_bytes(s).unwrap();
    let sub = NameMap::from_bindings(mode, [(name("x"), file(1, b"0123456789abcdef"))]);
    let children = NameMap::from_bindings(
        mode,
        [
            (name("b"), Archive::Symlink(b"target".to_vec())),
            (name("a"), file(3, b"fedcba9876543210")),
            (name("d"), Archive::Dir(Props::dir_default(), sub)),
        ],
    );
    let archive = Archive::Dir(Props::dir_default(), children);
    let c = PropsConfig::default().with_features(&enabled_features());
    let hash = archive::check_archive(&archive, mode, &c).unwrap();
    let mut props = ArchiveProps::empty(mode);
    props
        .props
        .add(&archive::CASE_KEY, mode.desc().as_bytes().to_vec());
    let mut path_props = Proplist::new();
    path_props.add(&archive::RSRC_KEY, true);
    props.paths.add(
        gemel_base::path::Path::from_bytes(b"d").unwrap(),
        path_props,
    );
    ArchiveData {
        archive,
        hash,
        magic: [mode.desc().as_bytes(), b"\x001.42"].concat(),
        props,
    }
}

fn parse(bytes: &[u8], warnings: &RefCell<Vec<String>>) -> Result<Option<ArchiveData>, Error> {
    let all = new_features().all();
    archive::parse_archive_file(
        bytes,
        "the-file",
        "Archive for root R synchronizing roots R, S",
        &all,
        Mode::Sensitive,
        &|s| warnings.borrow_mut().push(s.to_owned()),
    )
}

#[test]
fn headers_are_checked() {
    let data = sample_data(Mode::Sensitive);
    let c = PropsConfig::default().with_features(&enabled_features());
    let feats = ["Sync: ACL".to_owned(), "Sync: xattr".to_owned()];
    let roots = "Archive for root R synchronizing roots R, S";
    let good = archive::archive_file_bytes(&data, roots, "Written at now", &feats, &c);
    let warnings = RefCell::new(Vec::new());

    let back = parse(&good, &warnings).unwrap().unwrap();
    assert_eq!(back.archive, data.archive);
    assert_eq!((back.hash, &back.magic), (data.hash, &data.magic));
    assert_eq!(back.props.paths.len(), 1);
    assert!(warnings.borrow().is_empty());

    let wrong_format = [b"Unison archive format 22".as_slice(), &good[24..]].concat();
    assert!(parse(&wrong_format, &warnings).unwrap().is_none());
    let wrong_roots = archive::archive_file_bytes(
        &data,
        "Archive for root S synchronizing roots R, S",
        "Written at now",
        &feats,
        &c,
    );
    assert!(parse(&wrong_roots, &warnings).unwrap().is_none());
    assert_eq!(
        *warnings.borrow(),
        [
            "Archive format mismatch: found\n 'Unison archive format 22'\nbut expected\n \
             'Unison archive format 23'.\nI will delete the old archive and start from scratch.\n",
            "Archive mismatch: found\n 'Archive for root S synchronizing roots R, S'\nbut \
             expected\n 'Archive for root R synchronizing roots R, S'.\nI will delete the old \
             archive and start from scratch.\n",
        ]
    );

    let unknown = ["Sync: ACL".to_owned(), "Sync: teleport".to_owned()];
    let e = parse(
        &archive::archive_file_bytes(&data, roots, "Written at now", &unknown, &c),
        &warnings,
    )
    .unwrap_err();
    assert_eq!(
        e.to_string(),
        "Archive format mismatch: the archive was stored with features that are currently \
         not available.\nMissing features: Sync: teleport\nArchive file: the-file\nYou should \
         either upgrade Unison or invoke Unison once with -ignorearchives flag and then try \
         again."
    );

    // Without the features, props have no change time, xattrs or ACL.
    let plain =
        archive::archive_file_bytes(&data, roots, "Written at now", &[], &PropsConfig::default());
    assert_eq!(
        parse(&plain, &warnings).unwrap().unwrap().archive,
        data.archive
    );

    // A truncated "paths" frame is no paths; a truncated payload is damage.
    let paths_frame = good.len() - 1;
    let short_paths = parse(&good[..paths_frame], &warnings).unwrap().unwrap();
    assert!(short_paths.props.paths.is_empty());
    let damaged = parse(&good[..good.len() / 2], &warnings)
        .unwrap_err()
        .to_string();
    assert!(
        damaged.starts_with("Archive file seems damaged (end of input): use the -ignorearchives"),
        "{damaged}"
    );
    assert!(parse(b"", &warnings).is_err());
    assert_eq!(warnings.borrow().len(), 2);
}

#[test]
fn check_archive_reports_corruption() {
    let name = |s: &str| Name::from_bytes(s).unwrap();
    let leaf = || Archive::Symlink(b"t".to_vec());
    // Made in case-sensitive mode, "A" and "a" are the same file once
    // names are compared without case.
    let inner = NameMap::from_bindings(Mode::Sensitive, [(name("A"), leaf()), (name("a"), leaf())]);
    let top = NameMap::from_bindings(
        Mode::Sensitive,
        [(name("d"), Archive::Dir(Props::dir_default(), inner))],
    );
    let a = Archive::Dir(Props::dir_default(), top);
    let c = PropsConfig::default();
    assert!(archive::check_archive(&a, Mode::Sensitive, &c).is_ok());
    assert_eq!(
        archive::check_archive(&a, Mode::Insensitive, &c)
            .unwrap_err()
            .to_string(),
        "Corrupted archive: the file A occurs twice in path d"
    );
    let unordered =
        NameMap::from_bindings(Mode::Sensitive, [(name("B"), leaf()), (name("a"), leaf())]);
    let a = Archive::Dir(Props::dir_default(), unordered);
    assert_eq!(
        archive::check_archive(&a, Mode::Insensitive, &c)
            .unwrap_err()
            .to_string(),
        "Corrupted archive: the files B and a are not correctly ordered in directory "
    );
    assert_eq!(
        archive::check_archive(&Archive::NoArchive, Mode::Sensitive, &c).unwrap(),
        135
    );
    assert!(archive::archives_identical(&[Some(1), Some(1)]));
    assert!(!archive::archives_identical(&[Some(1), Some(2)]));
    assert!(!archive::archives_identical::<i32>(&[None, None]));
}

#[test]
fn case_conversion_drops_names_that_collide() {
    let name = |s: &str| Name::from_bytes(s).unwrap();
    let leaf = |t: &[u8]| Archive::Symlink(t.to_vec());
    let children = NameMap::from_bindings(
        Mode::Sensitive,
        [
            (name("B"), leaf(b"1")),
            (name("a"), leaf(b"2")),
            (name("b"), leaf(b"3")),
            (Name::from_bytes(b"c\xff".to_vec()).unwrap(), leaf(b"4")),
        ],
    );
    let a = Archive::Dir(Props::dir_default(), children);
    let Archive::Dir(_, out) = archive::make_case_sensitive_rec(&a, Mode::UnicodeInsensitive)
    else {
        panic!()
    };
    // "B" and "b" collide; "c\xff" isn't UTF-8.
    let kept: Vec<_> = out.iter().map(|(n, _)| n.as_bytes().to_vec()).collect();
    assert_eq!(kept, [b"a".to_vec()]);
    assert_eq!(out.mode(), Mode::UnicodeInsensitive);

    assert_eq!(
        archive::archive_mode(b"", Mode::Insensitive),
        b"Latin-1 case insensitive"
    );
    assert_eq!(
        archive::archive_mode(b"case sensitive\x001.2", Mode::Insensitive),
        b"case sensitive"
    );
    assert_eq!(
        archive::archive_mode(b"old", Mode::UnicodeInsensitive),
        b"some non-Unicode"
    );
    assert_eq!(
        archive::archive_mode(b"old", Mode::Sensitive),
        b"case sensitive"
    );
}

#[test]
fn root_aliases_and_names() {
    use gemel_rpc::root::{Host as RootHost, Root};

    let mut prefs = new_prefs();
    let root = Root {
        host: RootHost::Remote("h".into()),
        fspath: Fspath::from_canonical(b"/x".to_vec()),
    };
    assert_eq!(
        archive::root_to_string_or_alias(&prefs, &root).unwrap(),
        "//h//x"
    );
    prefs
        .parse_cmd_line(&["-rootalias".into(), " //h//x  ->  //g//y ".into()])
        .unwrap();
    assert_eq!(
        archive::root_to_string_or_alias(&prefs, &root).unwrap(),
        "//g//y"
    );
    prefs
        .parse_cmd_line(&["-rootalias".into(), "nonsense".into()])
        .unwrap();
    assert_eq!(
        archive::root_to_string_or_alias(&prefs, &root)
            .unwrap_err()
            .to_string(),
        "rootalias nonsense should be two strings separated by ' -> '"
    );
}

// ---- Commands on both roots ----

/// A client with roots `a` and `b` in `dir` (`b` on another host with
/// [`Via::Remote`]), its archives in `dir/u` (the server's too), and
/// warnings collected.
struct Setup {
    rpc: Rpc,
    roots: Roots,
    warnings: Rc<RefCell<Vec<String>>>,
    unison_dir: PathBuf,
    server: Option<Rc<Host>>,
}

const HOST_NAME: &str = "gemel-test-host";

fn setup(dir: &TempDir, via: Via, mode: Mode) -> Setup {
    let mut raw = ["a", "b"].map(|n| {
        let p = dir.0.join(n);
        std::fs::create_dir_all(&p).unwrap();
        p.to_str().unwrap().to_owned()
    });
    if via == Via::Remote {
        raw[1] = format!("ssh://{SERVER}/{}", raw[1]);
    }
    let unison_dir = dir.0.join("u");
    std::fs::create_dir_all(&unison_dir).unwrap();
    let mut prefs = new_prefs();
    globals::set_raw_roots(&mut prefs, &raw);
    prefs.set(&os::CLIENT_HOST_NAME, HOST_NAME.to_owned());
    settings(mode).store(&mut prefs);
    let client = Rc::new(Host::client(
        Rc::new(RefCell::new(prefs)),
        Rc::new(RefCell::new(new_features())),
    ));
    client.set_unison_dir(&unison_dir);
    let warnings = Rc::new(RefCell::new(Vec::new()));
    let w = warnings.clone();
    client.set_warn_printer(Some(Rc::new(move |s| w.borrow_mut().push(s.to_owned()))));
    let dispatch = if via == Via::Loopback {
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
    let server = (via == Via::Remote).then(|| {
        let server = Rc::new(Host::server(new_prefs(), new_features()));
        server.set_host_name(SERVER);
        server.set_unison_dir(&unison_dir);
        let w = warnings.clone();
        server.set_warn_printer(Some(Rc::new(move |s| w.borrow_mut().push(s.to_owned()))));
        server
    });
    let roots = block_on(async {
        let raw = globals::raw_roots(&rpc.client_host().prefs().borrow());
        if let Some(server) = &server {
            let clroot = gemel_base::clroot::parse_root(&raw[1]).unwrap();
            let provisional = gemel_rpc::root::Root {
                host: gemel_rpc::root::Host::Remote(SERVER.into()),
                fspath: Fspath::from_canonical(b"/".to_vec()),
            };
            rpc.connect_in_memory(clroot, provisional, server.clone());
        }
        let roots = Roots::install(&rpc, &raw).await.unwrap();
        archive::store_roots_name(&rpc, &roots).unwrap();
        globals::propagate_prefs(&rpc, &roots).await.unwrap();
        roots
    });
    Setup {
        rpc,
        roots,
        warnings,
        unison_dir,
        server,
    }
}

impl Setup {
    fn host(&self) -> &Host {
        self.rpc.local_host()
    }

    /// The host of root `i` (in the user's order).
    fn host_of(&self, i: usize) -> &Host {
        match (&self.roots.list()[i].host, &self.server) {
            (gemel_rpc::root::Host::Remote(_), Some(server)) => server,
            _ => self.host(),
        }
    }

    /// The file of `v` for root `i` (0 or 1, in the user's order).
    fn file(&self, i: usize, v: ArchiveVersion) -> PathBuf {
        let (name, _) =
            archive::archive_name(self.host_of(i), &self.roots.list()[i].fspath, v).unwrap();
        self.unison_dir.join(name)
    }

    fn this_root(&self, i: usize) -> String {
        archive::this_roots_global_name(self.host_of(i), &self.roots.list()[i].fspath).unwrap()
    }

    /// Stores `data` as root `i`'s main archive.
    fn store(&self, i: usize, data: &ArchiveData) {
        let f = system::name_bytes(self.file(i, ArchiveVersion::MainArch).as_os_str());
        archive::store_archive_local(self.host_of(i), &f, &self.this_root(i), data).unwrap();
    }

    fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut *self.warnings.borrow_mut())
    }
}

#[test]
fn archive_names_use_the_clients_host_name() {
    for via in Via::ALL {
        let dir = TempDir::new("names");
        let s = setup(&dir, via, Mode::Sensitive);
        let roots_name = s
            .rpc
            .client_host()
            .prefs()
            .borrow()
            .read(&archive::ROOTS_NAME)
            .clone();
        let expected: Vec<String> = s
            .roots
            .in_canonical_order()
            .iter()
            .map(|r| match &r.host {
                // A remote root is named after its host.
                gemel_rpc::root::Host::Remote(h) => format!("//{h}/{}", r.fspath),
                gemel_rpc::root::Host::Local => format!("//{HOST_NAME}/{}", r.fspath),
            })
            .collect();
        assert_eq!(roots_name, expected.join(", "));
        assert_eq!(
            s.host().prefs().borrow().read(&archive::ROOTS_NAME),
            &roots_name,
            "the host has rootsName"
        );
        let this_root = s.this_root(0);
        assert_eq!(
            this_root,
            format!("//{HOST_NAME}/{}", s.roots.list()[0].fspath)
        );
        let n = format!("{this_root};{roots_name};23");
        let hex: String = <md5::Md5 as md5::Digest>::digest(n.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let (name, host, exists) = block_on(s.rpc.call_root(
            &archive::ARCHIVE_NAME,
            &s.roots.list()[0],
            ArchiveVersion::Lock,
        ))
        .unwrap();
        assert_eq!(
            (name, host, exists),
            (format!("lk{hex}"), HOST_NAME.to_owned(), false)
        );
    }
}

#[test]
fn archives_load_and_recover() {
    for via in Via::ALL {
        let dir = TempDir::new("load");
        let s = setup(&dir, via, Mode::Sensitive);
        let (rpc, roots) = (&s.rpc, &s.roots);
        block_on(async {
            // No archives yet: the optimistic load fails, recovery finds
            // none, and both roots start empty.
            let sums = archive::load_archives_safely(rpc, roots).await.unwrap();
            assert_eq!(sums, [Some((0, Vec::new())), Some((0, Vec::new()))]);
            let w = s.take_warnings();
            assert_eq!(w.len(), 1, "{w:?}");
            assert!(w[0].starts_with(
                "No archive files were found for these roots, whose canonical names are:\n\t"
            ));
            assert!(!archive::found_archives(rpc));
            assert_eq!(
                archive::with_archive(s.host(), &s.this_root(0), Clone::clone),
                Some(Archive::NoArchive)
            );
            assert!(!s.file(0, ArchiveVersion::Lock).exists(), "unlocked");

            // Both roots stored: the optimistic load succeeds.
            let data = sample_data(Mode::Sensitive);
            s.store(0, &data);
            s.store(1, &data);
            let sums = archive::load_archives_safely(rpc, roots).await.unwrap();
            assert_eq!(sums, vec![Some((data.hash, data.magic.clone())); 2]);
            assert!(s.take_warnings().is_empty());
            assert_eq!(
                archive::with_archive(s.host_of(1), &s.this_root(1), Clone::clone),
                Some(data.archive.clone())
            );
            let props = archive::archive_props(s.host_of(1), &s.this_root(1)).unwrap();
            assert_eq!(props.paths.len(), 1);
            // Unchanged on disk, so nothing is loaded again.
            let (ok, sums) = archive::load_archives(rpc, roots, true).await.unwrap();
            assert!(ok);
            assert_eq!(sums, [Some((0, Vec::new())), Some((0, Vec::new()))]);

            // A new archive on one root only is dropped.
            std::fs::copy(
                s.file(0, ArchiveVersion::MainArch),
                s.file(0, ArchiveVersion::NewArch),
            )
            .unwrap();
            archive::lock_archives(rpc, roots).await.unwrap();
            archive::do_archive_crash_recovery(rpc, roots)
                .await
                .unwrap();
            archive::unlock_archives(rpc, roots).await.unwrap();
            assert!(!s.file(0, ArchiveVersion::NewArch).exists());
            let w = s.take_warnings();
            assert!(
                w[0].ends_with("A new archive exists on some hosts only; it will be ignored.\n"),
                "{w:?}"
            );
            assert!(archive::found_archives(rpc));

            // New archives on both roots replace the main ones.
            let mut newer = sample_data(Mode::Sensitive);
            newer.magic = b"case sensitive\x002.0".to_vec();
            for i in 0..2 {
                let f = system::name_bytes(s.file(i, ArchiveVersion::NewArch).as_os_str());
                archive::store_archive_local(s.host_of(i), &f, &s.this_root(i), &newer).unwrap();
            }
            let sums = archive::load_archives_safely(rpc, roots).await.unwrap();
            assert_eq!(sums, vec![Some((newer.hash, newer.magic.clone())); 2]);
            assert!(!s.file(1, ArchiveVersion::NewArch).exists());
            let w = s.take_warnings();
            assert!(
                w[0].ends_with("A new archive exists on all hosts: I'll use them.\n"),
                "{w:?}"
            );

            // An archive missing on one root is an inconsistent state.
            std::fs::remove_file(s.file(1, ArchiveVersion::MainArch)).unwrap();
            let e = archive::load_archives_safely(rpc, roots)
                .await
                .unwrap_err()
                .to_string();
            assert!(
                e.starts_with(
                    "Warning: inconsistent state.  \nThe archive file is missing on some hosts.\n"
                ),
                "{e}"
            );
            assert!(
                e.contains(" should be DELETED\n") && e.contains(" is MISSING\n"),
                "{e}"
            );
            assert!(
                !s.file(0, ArchiveVersion::Lock).exists(),
                "unlocked after the error"
            );
        });
    }
}

#[test]
fn archives_lock_on_both_roots() {
    for via in Via::ALL {
        let dir = TempDir::new("lock");
        let s = setup(&dir, via, Mode::Sensitive);
        let (rpc, roots) = (&s.rpc, &s.roots);
        block_on(async {
            archive::lock_archives(rpc, roots).await.unwrap();
            assert!(s.file(0, ArchiveVersion::Lock).exists());
            assert!(s.file(1, ArchiveVersion::Lock).exists());
            assert!(archive::lock_archives(rpc, roots).await.is_err());
            archive::unlock_archives(rpc, roots).await.unwrap();
            assert!(!s.file(1, ArchiveVersion::Lock).exists());

            // Someone else holds root b's lock, which its host reports.
            let b_host = s.roots.list()[1].hostname();
            let b_host = if via == Via::Remote {
                b_host
            } else {
                HOST_NAME
            };
            let held = s.file(1, ArchiveVersion::Lock);
            std::fs::write(&held, "").unwrap();
            let e = archive::lock_archives(rpc, roots)
                .await
                .unwrap_err()
                .to_string();
            assert_eq!(
                e,
                format!(
                    "Warning: the archives are locked.  \nIf no other instance of unison is \
                     running, the locks should be removed.\nThe file {} on host {b_host} \
                     should be deleted\nPlease delete lock files as appropriate and try again.",
                    held.display()
                )
            );
            assert!(
                !s.file(0, ArchiveVersion::Lock).exists(),
                "a's lock was released"
            );
            // An optimistic load fails while a lock is held.
            let (ok, _) = archive::load_archives(rpc, roots, true).await.unwrap();
            assert!(!ok);
            // With ignorelocks, the locks are taken anyway.
            rpc.client_host()
                .prefs()
                .borrow_mut()
                .set(&archive::IGNORE_LOCKS, true);
            archive::lock_archives(rpc, roots).await.unwrap();
            archive::unlock_archives(rpc, roots).await.unwrap();
        });
        // The host releases its locks when it goes away: a loopback host
        // once its connection's tasks have seen the connection closed.
        let a_lock = s.file(0, ArchiveVersion::Lock);
        archive::lock_archive_local(s.host(), &s.roots.list()[0].fspath).unwrap();
        assert!(a_lock.exists());
        drop(s);
        block_on(async {
            for _ in 0..100 {
                tokio::task::yield_now().await;
            }
        });
        assert!(!a_lock.exists());
    }
}

#[test]
fn archives_are_converted_to_the_case_mode() {
    for via in Via::ALL {
        let dir = TempDir::new("case");
        let s = setup(&dir, via, Mode::UnicodeInsensitive);
        let (rpc, roots) = (&s.rpc, &s.roots);
        // Archives made in case-sensitive mode (as the magic says) are
        // converted: re-sorted, and every directory to be rescanned.
        let data = sample_data(Mode::Sensitive);
        let mut sensitive = data.clone();
        sensitive.magic = b"case sensitive\x001.0".to_vec();
        s.store(0, &sensitive);
        s.store(1, &sensitive);
        block_on(async {
            archive::load_archives_safely(rpc, roots).await.unwrap();
        });
        for i in 0..2 {
            let props = archive::archive_props(s.host_of(i), &s.this_root(i)).unwrap();
            assert_eq!(
                props.props.find(&archive::CASE_KEY).unwrap(),
                b"Unicode case insensitive"
            );
            assert!(props.props.mem(&archive::DIR_STAMP_KEY));
            let mode = archive::with_archive(s.host_of(i), &s.this_root(i), |a| match a {
                Archive::Dir(_, c) => c.mode(),
                _ => panic!(),
            });
            assert_eq!(mode, Some(Mode::UnicodeInsensitive));
        }
        // The fingerprint cache was filled from the archive.
        assert!(s.file(0, ArchiveVersion::FpCache).exists());

        // Archives made in another mode can't be converted.
        let mut latin1 = data.clone();
        latin1.magic = b"Latin-1 case insensitive\x001.0".to_vec();
        s.store(0, &latin1);
        s.store(1, &latin1);
        let e = block_on(archive::load_archives_safely(rpc, roots))
            .unwrap_err()
            .to_string();
        let (n0, _) =
            archive::archive_name(s.host(), &roots.list()[0].fspath, ArchiveVersion::MainArch)
                .unwrap();
        assert!(
            e.starts_with(
                "Warning: incompatible case sensitivity settings.\nUnison is currently in \
                 Unicode case insensitive mode,\nwhile the archives were created in Latin-1 \
                 case insensitive mode.\nYou should either change Unison's setup or delete\n\
                 the following archives from the .unison directories:\n"
            ),
            "{e}"
        );
        assert!(
            e.contains(&format!("    archive {n0} on host {HOST_NAME}\n")),
            "{e}"
        );
        assert!(
            e.ends_with("(or invoke Unison once with -ignorearchives flag).\nThen, try again.")
        );
    }
}

#[test]
fn ignorearchives_starts_from_scratch() {
    let dir = TempDir::new("ignore");
    let s = setup(&dir, Via::Loopback, Mode::Sensitive);
    let data = sample_data(Mode::Sensitive);
    s.store(0, &data);
    s.store(1, &data);
    s.rpc
        .client_host()
        .prefs()
        .borrow_mut()
        .set(&archive::IGNORE_ARCHIVES, true);
    block_on(async { globals::propagate_prefs(&s.rpc, &s.roots).await.unwrap() });
    let sums = block_on(archive::load_archives_safely(&s.rpc, &s.roots)).unwrap();
    assert_eq!(sums, [Some((0, Vec::new())), Some((0, Vec::new()))]);
    assert!(!archive::found_archives(&s.rpc));
    assert_eq!(
        archive::with_archive(s.host(), &s.this_root(0), Clone::clone),
        Some(Archive::NoArchive)
    );
    // Once recovery finds the archives again, they count as found.
    s.rpc
        .client_host()
        .prefs()
        .borrow_mut()
        .set(&archive::IGNORE_ARCHIVES, false);
    block_on(async { globals::propagate_prefs(&s.rpc, &s.roots).await.unwrap() });
    std::fs::copy(
        s.file(0, ArchiveVersion::MainArch),
        s.file(0, ArchiveVersion::NewArch),
    )
    .unwrap();
    block_on(archive::load_archives_safely(&s.rpc, &s.roots)).unwrap();
    assert!(archive::found_archives(&s.rpc));
}

#[test]
fn commits_move_scratch_to_main() {
    let dir = TempDir::new("commit");
    let s = setup(&dir, Via::Loopback, Mode::Sensitive);
    let data = sample_data(Mode::Sensitive);
    let scratch = s.file(0, ArchiveVersion::ScratchArch);
    let f = system::name_bytes(scratch.as_os_str());
    archive::store_archive_local(s.host(), &f, &s.this_root(0), &data).unwrap();
    let r = &s.roots.list()[0];
    block_on(async {
        s.rpc
            .call_root(&archive::COMMIT_ARCHIVE, r, ())
            .await
            .unwrap();
        assert!(!scratch.exists());
        assert!(s.file(0, ArchiveVersion::NewArch).exists());
        s.rpc
            .call_root(&archive::POST_COMMIT_ARCHIVE, r, ())
            .await
            .unwrap();
        s.rpc
            .call_root(&archive::REMOVE_ARCHIVE, r, ArchiveVersion::NewArch)
            .await
            .unwrap();
        // Removing what isn't there is fine.
        s.rpc
            .call_root(&archive::REMOVE_ARCHIVE, r, ArchiveVersion::NewArch)
            .await
            .unwrap();
    });
    let main = s.file(0, ArchiveVersion::MainArch);
    assert!(!s.file(0, ArchiveVersion::NewArch).exists());
    let f = system::name_bytes(main.as_os_str());
    let back = archive::load_archive_local(s.host(), &f, &s.this_root(0))
        .unwrap()
        .unwrap();
    assert_eq!(back.archive, data.archive);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&main).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // The archive's status was recorded, so it counts as unchanged.
    let info = archive::get_archive_info(&f).unwrap();
    assert!(archive::archive_unchanged(
        s.host(),
        &s.this_root(0),
        info.as_ref()
    ));
}

/// With root b on a server on another host (an in-memory connection), each
/// host names its root's archives after itself: the server's lock is in its
/// own Unison directory, under the name only a host called "server" gives.
#[test]
fn a_server_names_its_archives_after_itself() {
    let dir = TempDir::new("two-hosts");
    let [a, b, u, v] = ["a", "b", "u", "v"].map(|n| {
        let p = dir.0.join(n);
        std::fs::create_dir_all(&p).unwrap();
        p
    });
    let fspath = |p: &StdPath| {
        let bytes = system::name_bytes(p.as_os_str());
        Fspath::canonize(Some(&bytes)).unwrap()
    };
    let mut prefs = new_prefs();
    prefs.set(&os::CLIENT_HOST_NAME, HOST_NAME.to_owned());
    let client = Rc::new(Host::client(
        Rc::new(RefCell::new(prefs)),
        Rc::new(RefCell::new(new_features())),
    ));
    client.set_unison_dir(&u);
    let mut rpc = Rpc::new(Dispatch::Direct(client));
    gemel_engine::register_commands(&mut rpc).unwrap();
    let server = Rc::new(Host::server(new_prefs(), new_features()));
    server.set_host_name("server");
    server.set_unison_dir(&v);
    let remote = gemel_rpc::root::Root {
        host: gemel_rpc::root::Host::Remote("server".into()),
        fspath: fspath(&b),
    };
    let clroot = gemel_base::clroot::Clroot::ConnectByShell {
        shell: "ssh".into(),
        host: "server".into(),
        user: None,
        port: None,
        path: Some(b.to_str().unwrap().into()),
    };
    let roots = Roots::from_roots(vec![gemel_rpc::root::Root::local(fspath(&a)), remote]);
    block_on(async {
        rpc.connect_in_memory(clroot, roots.list()[1].clone(), server.clone());
        archive::store_roots_name(&rpc, &roots).unwrap();
        globals::propagate_prefs(&rpc, &roots).await.unwrap();
        archive::lock_archives(&rpc, &roots).await.unwrap();
    });
    let lock = |host: &Host, root: &gemel_rpc::root::Root| {
        archive::archive_name(host, &root.fspath, ArchiveVersion::Lock)
            .unwrap()
            .0
    };
    let (ours, theirs) = (
        lock(rpc.client_host(), &roots.list()[0]),
        lock(&server, &roots.list()[1]),
    );
    assert!(u.join(&ours).exists(), "{ours}");
    assert!(v.join(&theirs).exists(), "{theirs}");
    // The server got the client's roots name, and named its root after
    // itself: the client would name the same path otherwise.
    assert_eq!(
        archive::this_roots_global_name(&server, &roots.list()[1].fspath).unwrap(),
        format!("//server/{}", roots.list()[1].fspath)
    );
    assert_ne!(theirs, lock(rpc.client_host(), &roots.list()[1]));
    block_on(archive::unlock_archives(&rpc, &roots)).unwrap();
    assert!(!v.join(&theirs).exists());
}
