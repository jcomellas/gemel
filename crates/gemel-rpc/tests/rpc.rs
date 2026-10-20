//! The RPC layer: OCaml's bytes, direct and loopback dispatch, errors, the
//! loopback host's separate state, and the engine thread.

use std::cell::RefCell;
use std::rc::Rc;

use gemel_base::clroot::Clroot;
use gemel_base::features::Features;
use gemel_base::prefs::{Category, PrefDef, Prefs};
use gemel_base::util::Error;
use gemel_compat::umarshal::{self, Reader};
use gemel_fs::fspath::Fspath;
use gemel_fs::os;
use gemel_rpc::host::Host;
use gemel_rpc::protocol::{self, Header};
use gemel_rpc::registry::{Dispatch, HostCmd, Registry, RootCmd, Rpc};
use gemel_rpc::root::{self, CANONIZE_ON_SERVER, Root};
use gemel_rpc::runtime::{self, EngineThread};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn fixture(name: &str) -> Vec<u8> {
    let text = include_str!("../../gemel-compat/tests/fixtures/rpc.txt");
    let line = text
        .lines()
        .find(|l| l.split(' ').next() == Some(name))
        .unwrap_or_else(|| panic!("{name}"));
    unhex(line.split(' ').nth(1).unwrap())
}

#[test]
fn framing_matches_ocaml() {
    let text = include_str!("../../gemel-compat/tests/fixtures/rpc.txt");
    let mut ints = 0;
    for line in text.lines().filter(|l| l.starts_with("int ")) {
        let f: Vec<&str> = line.split(' ').collect();
        let n: u32 = f[1].parse().unwrap();
        assert_eq!(
            protocol::encode_int(n).to_vec(),
            unhex(f[2]),
            "encodeInt {n}"
        );
        assert_eq!(protocol::decode_int(&unhex(f[2])), Ok(n));
        ints += 1;
    }
    assert!(ints >= 10);
    for (name, h) in [
        ("header_normal", Header::NormalResult),
        ("header_transient", Header::TransientExn("oops".into())),
        ("header_fatal", Header::FatalExn("bad".into())),
        ("header_request", Header::Request("canonizeOnServer".into())),
        ("header_stream", Header::Stream("x".into())),
        ("header_abort", Header::StreamAbort),
    ] {
        let bytes = fixture(name);
        assert_eq!(umarshal::to_bytes(|w| h.marshal(w)), bytes, "{name}");
        assert_eq!(umarshal::from_bytes(&bytes, Header::unmarshal), Ok(h));
    }
}

/// The argument and result sections of canonizeOnServer carry exactly
/// OCaml's frames.
#[test]
fn canonize_on_server_codecs_match_ocaml() {
    let section = |tag: &str, frame: Vec<u8>| -> Vec<u8> {
        let len = u32::try_from(tag.len() + frame.len()).unwrap();
        [&protocol::encode_int(len)[..], tag.as_bytes(), &frame].concat()
    };
    let read_args = |r: &mut Reader<'_>| Ok((r.option(Reader::string)?, r.bool()?));
    let args = protocol::decode_section(
        "canonizeOnServer-args",
        &section("canonizeOnServer-args", fixture("canonize_args_some")),
        read_args,
    );
    assert_eq!(args, Ok((Some(b"/tmp/x".to_vec()), true)));
    let res = protocol::decode_section(
        "canonizeOnServer-res",
        &section("canonizeOnServer-res", fixture("canonize_res")),
        |r| Ok((r.string()?, Fspath::unmarshal(r)?)),
    );
    assert_eq!(
        res,
        Ok((b"host".to_vec(), Fspath::from_canonical(b"/a/b".to_vec())))
    );
}

/// A root command that fails on request, and reports the root it ran on.
static PROBE: RootCmd<u8, Vec<u8>> = RootCmd::new(
    "probe",
    |a, w| w.char(*a),
    |r| r.char(),
    |s, w| w.string(s),
    |r| r.string(),
    |_, fspath, a| {
        Box::pin(async move {
            match a {
                b't' => Err(Error::Transient("transient failure".into())),
                b'f' => Err(Error::Fatal("fatal failure".into())),
                _ => Ok(fspath.as_bytes().to_vec()),
            }
        })
    },
);

/// A preference set on the client; whether the host sees it depends on
/// whether it is sent.
static SENT: PrefDef<String> = PrefDef::new("probe-sent", Category::Expert, String::new, "", "");
static NOT_SENT: PrefDef<String> =
    PrefDef::new("probe-local", Category::Expert, String::new, "", "").cli_only();

/// A host command that reports the two preferences as the host sees them.
static READ_PREFS: HostCmd<(), (String, String)> = HostCmd::new(
    "readPrefs",
    |(), _| {},
    |_| Ok(()),
    |(a, b), w| {
        w.string(a.as_bytes());
        w.string(b.as_bytes());
    },
    |r| {
        Ok((
            String::from_utf8(r.string()?).unwrap(),
            String::from_utf8(r.string()?).unwrap(),
        ))
    },
    |host, ()| {
        Box::pin(async move {
            let prefs = host.prefs().borrow();
            Ok((prefs.read(&SENT).clone(), prefs.read(&NOT_SENT).clone()))
        })
    },
);

fn fresh_prefs() -> Prefs {
    let mut p = Prefs::new();
    p.register(&SENT).unwrap();
    p.register(&NOT_SENT).unwrap();
    os::register_prefs(&mut p).unwrap();
    p
}

fn registered(dispatch: Dispatch) -> Rpc {
    let mut rpc = Rpc::new(dispatch);
    rpc.registry_mut()
        .register_host(&CANONIZE_ON_SERVER)
        .unwrap();
    rpc.registry_mut().register_root(&PROBE).unwrap();
    rpc.registry_mut().register_host(&READ_PREFS).unwrap();
    rpc
}

/// Both dispatch modes for the same client state.
fn both() -> [Rpc; 2] {
    let mut prefs = fresh_prefs();
    prefs.set(&SENT, "sent".into());
    prefs.set(&NOT_SENT, "not sent".into());
    let features = Features::new();
    let server = Host::loopback_server(&prefs, &features, fresh_prefs(), Features::new()).unwrap();
    let client = Host::client(
        Rc::new(RefCell::new(prefs)),
        Rc::new(RefCell::new(features)),
    );
    [
        registered(Dispatch::Direct(Rc::new(client))),
        registered(Dispatch::Loopback(Rc::new(server))),
    ]
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, f)
}

#[test]
fn direct_and_loopback_agree() {
    let dir = std::env::temp_dir();
    let dir_bytes = gemel_fs::system::name_bytes(dir.as_os_str());
    let expected = Fspath::canonize(Some(&dir_bytes)).unwrap();
    let [direct, loopback] = both();
    block_on(async {
        for rpc in [&direct, &loopback] {
            let clroot = Clroot::ConnectLocal(Some(String::from_utf8(dir_bytes.clone()).unwrap()));
            let root = root::canonize_root(rpc, &clroot).await.unwrap();
            assert_eq!(root, Root::local(expected.clone()));
            assert_eq!(
                rpc.call_root(&PROBE, &root, b'x').await.unwrap(),
                expected.as_bytes()
            );
            assert_eq!(
                rpc.call_root(&PROBE, &root, b't').await,
                Err(Error::Transient("transient failure".into()))
            );
            assert_eq!(
                rpc.call_root(&PROBE, &root, b'f').await,
                Err(Error::Fatal("fatal failure".into()))
            );
            let remote = Root {
                host: root::Host::Remote("elsewhere".into()),
                fspath: expected.clone(),
            };
            let e = rpc
                .call_root(&PROBE, &remote, b'x')
                .await
                .unwrap_err()
                .to_string();
            assert!(e.starts_with("Remote roots are not supported yet"), "{e}");
        }
    });
}

#[test]
fn the_loopback_host_has_only_what_a_server_would() {
    let [direct, loopback] = both();
    block_on(async {
        let local = root::Host::Local;
        assert_eq!(
            direct.call_host(&READ_PREFS, &local, ()).await.unwrap(),
            ("sent".to_owned(), "not sent".to_owned())
        );
        // cli_only preferences aren't sent to a server.
        assert_eq!(
            loopback.call_host(&READ_PREFS, &local, ()).await.unwrap(),
            ("sent".to_owned(), String::new())
        );
        assert!(loopback.local_host().is_server());
    });
}

#[test]
fn registration_errors() {
    let mut r = Registry::new();
    r.register_root(&PROBE).unwrap();
    assert_eq!(
        r.register_root(&PROBE),
        Err(Error::Fatal("probe already registered!".into()))
    );
    let rpc = Rpc::new(Dispatch::Direct(Rc::new(Host::server(
        Prefs::new(),
        Features::new(),
    ))));
    let root = Root::local(Fspath::from_canonical(b"/".to_vec()));
    let e = block_on(rpc.call_root(&PROBE, &root, b'x')).unwrap_err();
    assert_eq!(e, Error::Fatal("probe not registered!".into()));
}

#[test]
fn engine_thread_runs_commands_and_blocking_work() {
    let mut engine: EngineThread<u64, u64> =
        EngineThread::spawn(|mut commands, events| async move {
            // State that isn't Send lives on the engine thread.
            let seen = Rc::new(RefCell::new(0));
            while let Some(n) = commands.recv().await {
                *seen.borrow_mut() += 1;
                let sum = runtime::blocking(move || (1..=n).sum::<u64>()).await;
                let count = *seen.borrow();
                let _ = events.send(sum * 10 + count);
            }
        })
        .unwrap();
    assert!(engine.send(4));
    assert_eq!(engine.recv_blocking(), Some(101));
    assert!(engine.send(10));
    assert_eq!(engine.recv_blocking(), Some(552));
    engine.join().unwrap();
}
