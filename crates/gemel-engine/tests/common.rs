//! Update items and fingerprint-cache chunks against OCaml's bytes
//! (`common.txt`, from `gen_fixtures.ml`), and update items through the
//! loopback with negotiated features.

use std::cell::RefCell;
use std::rc::Rc;

use gemel_base::features::Features;
use gemel_base::name::Name;
use gemel_base::path::Path;
use gemel_base::prefs::Prefs;
use gemel_compat::umarshal;
use gemel_engine::common::{
    Codec, ContentsChange, FEATURE_PREV_STATE2, PermChange, PrevState, UpdateContent, UpdateItem,
};
use gemel_engine::fpcache::{self, FpCache, MAGIC};
use gemel_engine::globals::{self, Roots};
use gemel_fs::fileinfo::Stamp;
use gemel_fs::fingerprint::Fingerprint;
use gemel_fs::os::FullFingerprint;
use gemel_fs::osx::RessInfo;
use gemel_fs::props::{self, AttrValue, Id, Perm, Props, PropsConfig, Time};
use gemel_rpc::host::Host;
use gemel_rpc::registry::{Dispatch, HostCmd, RootCmd, Rpc};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

const FIXTURES: &str = include_str!("../../gemel-compat/tests/fixtures/common.txt");

// The values of gen_fixtures.ml.

fn p1() -> Props {
    Props {
        perm: Perm {
            bits: 0o644,
            mask: 0o1777,
        },
        uid: Id::Named(b"alice".to_vec()),
        gid: Id::Numeric(20),
        time: Time::Synced(1_600_000_000.5),
        type_creator: Some(b"TEXTMACS".to_vec()),
        length: 123,
        ctime: 1_600_000_001.25,
        xattr: Some((
            vec![
                (b"user.a".to_vec(), AttrValue::String(b"v".to_vec())),
                (b"user.b".to_vec(), AttrValue::Hash(vec![1, 2, 3])),
                (
                    b"com.apple.FinderInfo".to_vec(),
                    AttrValue::Loaded(b"full value".to_vec(), b"hash".to_vec()),
                ),
            ],
            1234,
        )),
        acl: Some(b"acl text".to_vec()),
    }
}

fn p2() -> Props {
    Props {
        perm: Perm {
            bits: 0o755,
            mask: 0o777,
        },
        uid: Id::Ignored,
        gid: Id::Ignored,
        time: Time::NotSynced(12.0),
        type_creator: None,
        length: 0,
        ctime: -1.0,
        xattr: None,
        acl: None,
    }
}

fn fp(data: &[u8], ress: &[u8]) -> FullFingerprint {
    FullFingerprint {
        data: Fingerprint(data.to_vec()),
        ress: Fingerprint(ress.to_vec()),
    }
}

fn fp1() -> FullFingerprint {
    fp(b"0123456789abcdef", b"")
}

fn fp2() -> FullFingerprint {
    fp(b"fedcba9876543210", b"ressfingerprint!")
}

fn name(s: &str) -> Name {
    Name::from_bytes(s.as_bytes()).unwrap()
}

fn update_items() -> Vec<(&'static str, UpdateItem)> {
    use UpdateContent as C;
    use UpdateItem::{Error, NoUpdates, Updates};
    vec![
        ("noupdates", NoUpdates),
        ("error", Error("oops".into())),
        ("absent", Updates(C::Absent, PrevState::New)),
        (
            "file",
            Updates(
                C::File(
                    p1(),
                    ContentsChange::Updated(fp1(), Stamp::Inode(42), RessInfo::NoRess),
                ),
                PrevState::File(p2(), fp2(), Stamp::NoStamp, RessInfo::HfsRess(7)),
            ),
        ),
        (
            "file_same",
            Updates(C::File(p1(), ContentsChange::Same), PrevState::Dir(p2())),
        ),
        (
            "file_rescan",
            Updates(
                C::File(
                    p2(),
                    ContentsChange::Updated(
                        fp2(),
                        Stamp::Rescan,
                        RessInfo::AppleDoubleRess(5, 2.5, 3.5, 9, ()),
                    ),
                ),
                PrevState::File(p1(), fp1(), Stamp::Inode(7), RessInfo::NoRess),
            ),
        ),
        (
            "dir",
            Updates(
                C::Dir(
                    p1(),
                    vec![
                        (name("a"), NoUpdates),
                        (
                            name("b"),
                            Updates(C::Symlink(b"t".to_vec()), PrevState::Symlink),
                        ),
                        (name("c"), Error("deep".into())),
                    ],
                    PermChange::Updated,
                    false,
                ),
                PrevState::Dir(p2()),
            ),
        ),
        (
            "dir_empty",
            Updates(C::Dir(p2(), vec![], PermChange::Same, true), PrevState::New),
        ),
        (
            "symlink",
            Updates(C::Symlink(b"target".to_vec()), PrevState::Symlink),
        ),
    ]
}

fn codec(config: &str) -> Codec {
    let flag = |c: char| config.contains(&format!("{c}1"));
    Codec {
        props: PropsConfig {
            xattr_enabled: flag('x'),
            acl_enabled: flag('a'),
            ..PropsConfig::default()
        },
        prev_state2: flag('p'),
    }
}

/// What a value is after a round trip through the encoding: the props
/// lose what their features don't carry, and the old `prevState` format
/// loses a file's stamp.
fn through(ui: &UpdateItem, c: &Codec) -> UpdateItem {
    let props = |p: &Props| Props {
        ctime: if c.props.xattr_enabled || c.props.acl_enabled {
            p.ctime
        } else {
            props::CTIME_DUMMY
        },
        xattr: if c.props.xattr_enabled {
            p.xattr.clone()
        } else {
            None
        },
        acl: if c.props.acl_enabled {
            p.acl.clone()
        } else {
            None
        },
        ..p.clone()
    };
    let prev = |s: &PrevState| match s {
        PrevState::Dir(p) => PrevState::Dir(props(p)),
        PrevState::File(p, fp, stamp, ress) => PrevState::File(
            props(p),
            fp.clone(),
            if c.prev_state2 {
                *stamp
            } else {
                Stamp::NoStamp
            },
            ress.clone(),
        ),
        other => other.clone(),
    };
    match ui {
        UpdateItem::Updates(uc, s) => UpdateItem::Updates(
            match uc {
                UpdateContent::File(p, ch) => UpdateContent::File(props(p), ch.clone()),
                UpdateContent::Dir(p, children, perm, empty) => UpdateContent::Dir(
                    props(p),
                    children
                        .iter()
                        .map(|(n, ui)| (n.clone(), through(ui, c)))
                        .collect(),
                    *perm,
                    *empty,
                ),
                other => other.clone(),
            },
            prev(s),
        ),
        other => other.clone(),
    }
}

#[test]
fn update_items_match_ocaml() {
    let items = update_items();
    let mut n = 0;
    for line in FIXTURES.lines().filter(|l| l.starts_with("ui ")) {
        let f: Vec<&str> = line.split(' ').collect();
        let (config, which, bytes) = (f[1], f[2], unhex(f[3]));
        let c = codec(config);
        let ui = &items.iter().find(|(n, _)| *n == which).unwrap().1;
        assert_eq!(
            umarshal::to_bytes(|w| ui.marshal(w, &c)),
            bytes,
            "{config} {which}"
        );
        let back = umarshal::from_bytes(&bytes, |r| UpdateItem::unmarshal(r, &c)).unwrap();
        assert_eq!(back, through(ui, &c), "{config} {which}");
        n += 1;
    }
    assert_eq!(n, 5 * items.len());
}

fn entries() -> Vec<(&'static str, fpcache::Entry)> {
    vec![
        ("a/b", (p1(), fp1(), Stamp::Inode(42), RessInfo::NoRess)),
        ("a/c", (p2(), fp2(), Stamp::NoStamp, RessInfo::HfsRess(7))),
        (
            "",
            (
                p1(),
                fp2(),
                Stamp::Rescan,
                RessInfo::AppleDoubleRess(5, 2.5, 3.5, 9, ()),
            ),
        ),
    ]
}

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let d = std::env::temp_dir().join(format!("gemel-engine-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_fingerprint_cache_file_matches_ocaml() {
    let chunk = FIXTURES
        .lines()
        .find_map(|l| l.strip_prefix("fpcache_chunk "))
        .map(unhex)
        .unwrap();
    let ocaml = [format!("{MAGIC}\n").as_bytes(), &chunk].concat();
    let config = PropsConfig {
        xattr_enabled: true,
        acl_enabled: true,
        ..PropsConfig::default()
    };
    let dir = TempDir::new("fpcache");
    let file = dir.0.join("fp");

    // Writing: the paths compress to OCaml's deltas.
    let mut cache = FpCache::init(true, false, &file, config);
    for (p, e) in entries() {
        cache.save(&Path::from_bytes(p.as_bytes()).unwrap(), e);
    }
    cache.finish();
    assert_eq!(std::fs::read(&file).unwrap(), ocaml);

    // Reading.
    let chunks = fpcache::read_file(&file, &config).unwrap();
    let expected: Vec<(Vec<u8>, fpcache::Entry)> = entries()
        .into_iter()
        .map(|(p, e)| (p.as_bytes().to_vec(), e))
        .collect();
    assert_eq!(chunks, std::slice::from_ref(&expected));
    let cache = FpCache::init(true, false, &file, config);
    assert_eq!(
        cache.get(&Path::from_bytes(b"a/c").unwrap()),
        Some(&expected[1].1)
    );

    // A scan that saves nothing leaves the file alone; a disabled cache
    // reads nothing.
    cache.finish();
    assert_eq!(std::fs::read(&file).unwrap(), ocaml);
    let off = FpCache::init(false, false, &file, config);
    assert!(off.get(&Path::from_bytes(b"a/c").unwrap()).is_none());

    // A damaged chunk ends the reading quietly: here, after a good one.
    let mut damaged = [&ocaml[..], &chunk].concat();
    let last = damaged.len() - 1;
    damaged[last] ^= 1;
    std::fs::write(&file, &damaged).unwrap();
    assert_eq!(fpcache::read_file(&file, &config).unwrap().len(), 1);
    std::fs::write(&file, &ocaml[..ocaml.len() - 3]).unwrap();
    assert!(fpcache::read_file(&file, &config).unwrap().is_empty());
    std::fs::write(&file, b"Unison fingerprint cache format 2\n").unwrap();
    assert!(fpcache::read_file(&file, &config).is_err());
}

#[test]
fn the_cache_is_used_only_for_clearly_unchanged_files() {
    use gemel_fs::fileinfo::{self, FsConfig};
    use gemel_fs::fspath::Fspath;
    use gemel_fs::system;

    let dir = TempDir::new("fphit");
    let root = dir.0.join("r");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("f"), b"contents").unwrap();
    let fspath = Fspath::canonize(Some(&system::name_bytes(root.as_os_str()))).unwrap();
    let file = dir.0.join("fp");
    let c = FsConfig {
        props: PropsConfig {
            sync_times: true,
            ..PropsConfig::default()
        },
        allow_symlinks: gemel_base::prefs::BoolWithDefault::Default,
        symlinks_allowed: true,
        ignore_inode_numbers: false,
        dont_chmod: false,
    };
    let path = Path::from_bytes(b"f").unwrap();
    let none: &dyn Fn(&Path<_>) -> bool = &|_| false;
    let info = fileinfo::get(true, &fspath, &path, none, &c).unwrap();
    let real = FullFingerprint {
        data: Fingerprint::file(&fspath, &path).unwrap(),
        ress: Fingerprint::DUMMY,
    };

    // Record a fake fingerprint for the file as it is now.
    let fake = fp(b"not the real one", b"");
    let mut cache = FpCache::init(true, false, &file, c.props);
    cache.save(
        &path,
        (
            info.desc.clone(),
            fake.clone(),
            info.stamp(&c),
            info.ress_stamp(),
        ),
    );
    cache.finish();

    let mut cache = FpCache::init(true, false, &file, c.props);
    let (_, got, ..) = cache
        .fingerprint(false, true, false, &fspath, &path, &info, None, none, &c)
        .unwrap();
    assert_eq!(got, fake, "a clearly unchanged file uses the cache");
    // Without fast checking, it is fingerprinted.
    let (_, got, ..) = cache
        .fingerprint(false, false, false, &fspath, &path, &info, None, none, &c)
        .unwrap();
    assert_eq!(got, real);
    // So is a file whose length changed.
    std::fs::write(root.join("f"), b"longer contents").unwrap();
    let info2 = fileinfo::get(true, &fspath, &path, none, &c).unwrap();
    let mut cache = FpCache::init(true, false, &file, c.props);
    let (desc, got, ..) = cache
        .fingerprint(false, true, false, &fspath, &path, &info2, None, none, &c)
        .unwrap();
    assert_eq!(desc.length, 15);
    assert_ne!(got, fake);
    // fastercheckUNSAFE: a new file gets a pseudo-fingerprint.
    let (_, got, ..) = cache
        .fingerprint(true, true, true, &fspath, &path, &info2, None, none, &c)
        .unwrap();
    assert!(got.is_pseudo());
    // Its length is the scan's (`info`, 8 bytes), not the new stat's.
    let mut empty = FpCache::disabled(c.props);
    let (desc, got, ..) = empty
        .fingerprint(true, true, true, &fspath, &path, &info, None, none, &c)
        .unwrap();
    assert_eq!(got, FullFingerprint::pseudo(b"f", 8));
    assert_eq!(desc.length, 15);
    assert!(fpcache::excel_file(&Path::from_bytes(b"d/x.xls").unwrap()));
}

// ---- Through the loopback ----

/// A root command that returns the update item it is given, to see what
/// arrives with the negotiated features.
static ECHO: RootCmd<UpdateItem, UpdateItem> = RootCmd::new(
    "echoUpdateItem",
    |ui, w, f| ui.marshal(w, &Codec::new(f)),
    |r, f| UpdateItem::unmarshal(r, &Codec::new(f)),
    |ui, w, f| ui.marshal(w, &Codec::new(f)),
    |r, f| UpdateItem::unmarshal(r, &Codec::new(f)),
    |_, _, ui| Box::pin(async move { Ok(ui) }),
);

fn features(prev_state2: bool) -> Features {
    let mut f = Features::new();
    props::register_features(&mut f).unwrap();
    if prev_state2 {
        gemel_engine::common::register_features(&mut f).unwrap();
    }
    f
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, f)
}

#[test]
fn update_items_travel_with_the_negotiated_features() {
    let dir = TempDir::new("loopback");
    let (a, b) = (dir.0.join("a"), dir.0.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let raw = [a, b].map(|p| p.to_str().unwrap().to_owned());
    // The server lacks prevState2 in the second case, so the old format is
    // negotiated and the stamp is lost.
    for (server_prev_state2, stamp) in [(true, Stamp::Inode(7)), (false, Stamp::NoStamp)] {
        let mut prefs = Prefs::new();
        globals::register_prefs(&mut prefs).unwrap();
        gemel_fs::os::register_prefs(&mut prefs).unwrap();
        globals::set_raw_roots(&mut prefs, &raw);
        let server_prefs = {
            let mut p = Prefs::new();
            globals::register_prefs(&mut p).unwrap();
            gemel_fs::os::register_prefs(&mut p).unwrap();
            p
        };
        let client_features = features(true);
        let server = Host::loopback_server(
            &prefs,
            &client_features,
            server_prefs,
            features(server_prev_state2),
        )
        .unwrap();
        let client = Rc::new(Host::client(
            Rc::new(RefCell::new(prefs)),
            Rc::new(RefCell::new(client_features)),
        ));
        let mut rpc = Rpc::new(Dispatch::Loopback {
            client: client.clone(),
            server: Rc::new(server),
        });
        gemel_engine::register_commands(&mut rpc).unwrap();
        rpc.registry_mut().register_root(&ECHO).unwrap();
        block_on(async {
            let raw = globals::raw_roots(&client.prefs().borrow());
            let roots = Roots::install(&rpc, &raw).await.unwrap();
            let enabled = client.features().borrow().get_enabled();
            assert_eq!(
                enabled.contains(&FEATURE_PREV_STATE2.to_owned()),
                server_prev_state2
            );
            assert!(enabled.contains(&props::FEATURE_XATTR.to_owned()));
            assert_eq!(
                rpc.local_host().features().borrow().get_enabled(),
                enabled,
                "the server has the same features enabled"
            );
            let ui = UpdateItem::Updates(
                UpdateContent::Absent,
                PrevState::File(p1(), fp1(), Stamp::Inode(7), RessInfo::NoRess),
            );
            let (r1, _) = roots.pair();
            let back = rpc.call_root(&ECHO, r1, ui).await.unwrap();
            let UpdateItem::Updates(_, PrevState::File(desc, _, got, _)) = back else {
                panic!("{back:?}")
            };
            assert_eq!(got, stamp);
            assert_eq!(desc, p1(), "props with xattrs and ACL arrive whole");
        });
    }
}

/// A host command that reads `batch` as the host sees it.
static READ_BATCH: HostCmd<(), bool> = HostCmd::new(
    "readBatch",
    |(), _, _| {},
    |_, _| Ok(()),
    |b, w, _| w.bool(*b),
    |r, _| r.bool(),
    |host, ()| Box::pin(async move { Ok(*host.prefs().borrow().read(&globals::BATCH)) }),
);

#[test]
fn preferences_set_after_the_loopback_host_reach_it() {
    let dir = TempDir::new("propagate");
    let raw = ["a", "b"].map(|n| {
        let p = dir.0.join(n);
        std::fs::create_dir_all(&p).unwrap();
        p.to_str().unwrap().to_owned()
    });
    let fresh = || {
        let mut p = Prefs::new();
        globals::register_prefs(&mut p).unwrap();
        gemel_fs::os::register_prefs(&mut p).unwrap();
        p
    };
    let prefs = fresh();
    let server = Host::loopback_server(&prefs, &features(true), fresh(), features(true)).unwrap();
    let client = Rc::new(Host::client(
        Rc::new(RefCell::new(prefs)),
        Rc::new(RefCell::new(features(true))),
    ));
    let mut rpc = Rpc::new(Dispatch::Loopback {
        client: client.clone(),
        server: Rc::new(server),
    });
    gemel_engine::register_commands(&mut rpc).unwrap();
    rpc.registry_mut().register_host(&READ_BATCH).unwrap();
    block_on(async {
        let roots = Roots::install(&rpc, &raw).await.unwrap();
        client.prefs().borrow_mut().set(&globals::BATCH, true);
        let local = gemel_rpc::root::Host::Local;
        assert!(!rpc.call_host(&READ_BATCH, &local, ()).await.unwrap());
        globals::propagate_prefs(&rpc, &roots).await.unwrap();
        assert!(rpc.call_host(&READ_BATCH, &local, ()).await.unwrap());
    });
}

#[test]
fn roots_are_checked_and_ordered() {
    use gemel_fs::fspath::Fspath;

    let mut prefs = Prefs::new();
    globals::register_prefs(&mut prefs).unwrap();
    gemel_fs::os::register_prefs(&mut prefs).unwrap();
    let client = Rc::new(Host::client(
        Rc::new(RefCell::new(prefs)),
        Rc::new(RefCell::new(features(true))),
    ));
    let mut rpc = Rpc::new(Dispatch::Direct(client));
    gemel_engine::register_commands(&mut rpc).unwrap();
    let dir = TempDir::new("roots");
    let canonize = |p: &std::path::Path| {
        Fspath::canonize(Some(&gemel_fs::system::name_bytes(p.as_os_str()))).unwrap()
    };
    // Given as b, then a; canonically a comes first.
    let (b, a) = (dir.0.join("b"), dir.0.join("a"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let (ca, cb) = (canonize(&a), canonize(&b));
    let s = |p: &std::path::Path| p.to_str().unwrap().to_owned();
    block_on(async {
        let e = Roots::install(&rpc, &[s(&b)])
            .await
            .unwrap_err()
            .to_string();
        assert!(
            e.starts_with("Wrong number of roots: 2 expected, but 1 provided ("),
            "{e}"
        );
        let e = Roots::install(&rpc, &[s(&b), s(&b)])
            .await
            .unwrap_err()
            .to_string();
        assert!(
            e.starts_with("That's no good, the roots appear to be the same!"),
            "{e}"
        );
        let roots = Roots::install(&rpc, &[s(&b), s(&a)]).await.unwrap();
        let fspaths =
            |l: &[gemel_rpc::root::Root]| l.iter().map(|r| r.fspath.clone()).collect::<Vec<_>>();
        assert_eq!(
            fspaths(&roots.in_canonical_order()),
            [ca.clone(), cb.clone()]
        );
        assert_eq!(fspaths(roots.list()), [cb.clone(), ca.clone()]);
        // Results come back in the user's order.
        let got = roots
            .map(|r| async move { Ok(r.fspath.clone()) })
            .await
            .unwrap();
        assert_eq!(got, [cb.clone(), ca.clone()]);
        let e = roots
            .map(|r| {
                let fail = r.fspath == ca;
                async move {
                    if fail {
                        Err::<(), _>(gemel_base::util::Error::Transient("on a".into()))
                    } else {
                        Ok(())
                    }
                }
            })
            .await;
        assert_eq!(e, Err(gemel_base::util::Error::Transient("on a".into())));
        // Without remote roots, all features are enabled.
        let f = rpc.client_host().features().borrow().get_enabled();
        assert_eq!(f.len(), 3, "{f:?}");
    });
}
