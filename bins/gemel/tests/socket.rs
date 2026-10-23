//! Socket servers (`-socket`) and clients of `socket://` roots: a server
//! serving clients one after another until one sets `killserver`, or until
//! Ctrl-C, and how servers and clients fail. With `GEMEL_REFERENCE_UNISON`
//! naming OCaml Unison, its server serves the same clients, and its
//! failures must be gemel's, byte for byte; otherwise gemel's are checked
//! against what OCaml's are on each system.
//!
//! Whole syncs on a socket are the oracle harness's (`tools/oracle`,
//! `--remote socket`).

#![cfg(unix)]

mod common;
#[path = "../../../crates/gemel-engine/tests/support/mod.rs"]
mod support;
#[path = "../../../crates/gemel-ui-api/tests/ui_support/mod.rs"]
mod ui_support;

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader};
use std::path::Path as StdPath;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::{compare, gemel_path, ocaml_args, ocaml_path, run};
use ui_support::Pair;

/// The servers to check: gemel's, and OCaml's if there is one.
fn servers() -> Vec<OsString> {
    let mut v = vec![gemel_path().to_owned()];
    v.extend(ocaml_path());
    v
}

/// A free TCP port on localhost, as the system gives one.
fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A socket server running in `dir`, with its Unison directory there.
struct Server {
    child: Child,
    stderr: mpsc::Receiver<String>,
}

impl Server {
    /// Starts `prog -socket <args>` in `dir`, and waits until it says it
    /// has started.
    fn start(prog: &OsStr, dir: &StdPath, args: &[&str]) -> Server {
        let mut child = Command::new(prog)
            .arg("-socket")
            .args(args)
            .current_dir(dir)
            .env("UNISON", dir.join("su"))
            .env("HOME", dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, stderr) = mpsc::channel();
        let mut r = BufReader::new(child.stderr.take().unwrap());
        std::thread::spawn(move || {
            let mut line = String::new();
            while r.read_line(&mut line).unwrap_or(0) > 0 {
                let _ = tx.send(std::mem::take(&mut line));
            }
        });
        let server = Server { child, stderr };
        let first = server.stderr.recv_timeout(Duration::from_secs(20));
        assert_eq!(
            first.as_deref(),
            Ok("server started\n"),
            "{prog:?} {args:?}"
        );
        server
    }

    /// Waits for the server to end: its exit code, and what it printed
    /// besides that it had started.
    fn wait(mut self) -> (Option<i32>, String) {
        for _ in 0..400 {
            if let Some(status) = self.child.try_wait().unwrap() {
                let rest: String = self.stderr.iter().collect();
                return (status.code(), rest);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the server is still running");
    }

    fn interrupt(&self) {
        // The shell's `kill`: some systems have no program of that name.
        let ok = Command::new("sh")
            .args(["-c", &format!("kill -INT {}", self.child.id())])
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// How the server listens: on a port of localhost, or on a path in its
/// directory (short enough for a socket's address).
#[derive(Debug, Clone, Copy)]
enum On {
    Port(u16),
    Path,
}

impl On {
    fn args(&self) -> Vec<String> {
        match self {
            On::Port(p) => vec![p.to_string(), "-listen".into(), "127.0.0.1".into()],
            On::Path => vec!["s.sock".into()],
        }
    }

    /// Root `b` on this server, which is in the pair's directory.
    fn root(&self) -> String {
        match self {
            On::Port(p) => format!("socket://127.0.0.1:{p}/b"),
            On::Path => "socket://{s.sock}/b".into(),
        }
    }
}

/// Syncs `pair` with `prog`, root `b` on the server: the run.
fn sync(prog: &OsStr, pair: &Pair, on: On, extra: &[&str]) -> common::Run {
    let root = on.root();
    let mut args = vec!["a", &root, "-batch"];
    args.extend(extra);
    let args = if prog == gemel_path() {
        args
    } else {
        ocaml_args(&args)
    };
    run(prog, pair, &args, None)
}

#[test]
fn a_socket_server_serves_one_client_after_another() {
    for server in servers() {
        for on in [On::Port(free_port()), On::Path] {
            let what = format!("{server:?} on {on:?}");
            let pair = Pair::new("socket-serves");
            let args = on.args();
            let s = Server::start(
                &server,
                pair.base(),
                &args.iter().map(String::as_str).collect::<Vec<_>>(),
            );
            // Clients in turn, the last with `killserver`, which stops it.
            let mut clients = vec![gemel_path().to_owned()];
            clients.extend(ocaml_path());
            clients.push(gemel_path().to_owned());
            for (i, client) in clients.iter().enumerate() {
                let name = format!("f{i}");
                std::fs::write(pair.a().join(&name), &name).unwrap();
                let last = i + 1 == clients.len();
                let r = sync(client, &pair, on, if last { &["-killserver"] } else { &[] });
                assert_eq!(r.code, Some(0), "{what}, client {client:?}: {r:#?}");
                assert_eq!(
                    std::fs::read_to_string(pair.b().join(&name))
                        .ok()
                        .as_deref(),
                    Some(&*name),
                    "{what}, client {client:?}"
                );
            }
            assert_eq!(s.wait(), (Some(0), String::new()), "{what}");
            assert!(!pair.base().join("s.sock").exists(), "{what}");
        }
    }
}

/// Not OCaml's server on macOS: 2.54 there doesn't stop on Ctrl-C, though
/// it means to (`Sys.catch_break`), as it does on Linux.
#[test]
fn ctrl_c_stops_a_socket_server() {
    let mut servers = servers();
    if cfg!(target_os = "macos") {
        servers.truncate(1);
    }
    for server in servers {
        for on in [On::Port(free_port()), On::Path] {
            let what = format!("{server:?} on {on:?}");
            let pair = Pair::new("socket-ctrl-c");
            let args = on.args();
            let s = Server::start(
                &server,
                pair.base(),
                &args.iter().map(String::as_str).collect::<Vec<_>>(),
            );
            // After a client, which leaves it waiting for the next.
            std::fs::write(pair.a().join("f"), "f").unwrap();
            assert_eq!(sync(gemel_path(), &pair, on, &[]).code, Some(0), "{what}");
            s.interrupt();
            assert_eq!(s.wait(), (Some(0), String::new()), "{what}");
            assert!(!pair.base().join("s.sock").exists(), "{what}");
        }
    }
}

/// Runs `prog -socket <args>` in `dir`, where it fails: its exit code and
/// standard error, with `port` as `<P>`.
fn failed_server(prog: &OsStr, dir: &StdPath, args: &[&str], port: u16) -> (Option<i32>, String) {
    let out = Command::new(prog)
        .arg("-socket")
        .args(args)
        .current_dir(dir)
        .env("UNISON", dir.join("su"))
        .env("HOME", dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let mut stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    stderr = stderr.replace(&port.to_string(), "<P>");
    assert!(out.stdout.is_empty());
    (out.status.code(), stderr)
}

#[test]
fn socket_servers_fail_as_ocamls_do() {
    let pair = Pair::new("socket-server-fails");
    let dir = pair.base();
    // A port in use, a path that is there, one too long for an address,
    // a host to listen on that isn't found, and a port that isn't one.
    let taken = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = taken.local_addr().unwrap().port();
    let p = port.to_string();
    std::fs::write(dir.join("there.sock"), "").unwrap();
    let long = "x".repeat(120);
    let cases: Vec<(Vec<&str>, String)> = vec![
        (
            vec![&p, "-listen", "127.0.0.1"],
            "Can't bind socket to port <P> at address [127.0.0.1]: Address already in use\n\n"
                .into(),
        ),
        (
            vec!["there.sock"],
            "Can't bind socket to path 'there.sock': Address already in use\n\n".into(),
        ),
        (
            vec![&long],
            format!("Can't bind socket to path '{long}': File name too long\n\n"),
        ),
        (
            vec![&p, "-listen", "no-such-host.invalid"],
            "Can't bind socket to port <P>\n".into(),
        ),
        (vec!["0x10"], "Can't bind socket to port 0x10\n".into()),
    ];
    for (args, expected) in cases {
        let ours = failed_server(gemel_path(), dir, &args, port);
        assert_eq!(
            ours,
            (Some(1), format!("Unison server failed: {expected}")),
            "{args:?}"
        );
        if let Some(ocaml) = ocaml_path() {
            let theirs = failed_server(&ocaml, dir, &args, port);
            assert_eq!(ours, theirs, "{args:?}: against OCaml");
        }
    }
    drop(taken);
}

#[test]
fn socket_clients_fail_as_ocamls_do() {
    // Nothing listening on a port, a path that isn't there or isn't a
    // socket, and a host that isn't found.
    let port = free_port();
    let refused = if cfg!(target_os = "macos") {
        // Unison's Lwt checks a connection with `getpeername`, which fails
        // with EINVAL on macOS once it is refused: the address is skipped.
        format!("Failed to connect to the server on host 127.0.0.1:{port}")
    } else {
        format!(
            "Failed to connect to the server on host 127.0.0.1[127.0.0.1]:{port}: Connection \
             refused"
        )
    };
    let cases = [
        (format!("socket://127.0.0.1:{port}/b"), refused),
        (
            "socket://{none.sock}/b".to_owned(),
            "Can't connect to Unix domain socket on path none.sock: No such file or directory"
                .to_owned(),
        ),
        (
            "socket://{gone.sock}/b".to_owned(),
            "Can't connect to Unix domain socket on path gone.sock: Connection refused".to_owned(),
        ),
        (
            "socket://no-such-host.invalid:5000/b".to_owned(),
            "Failed to connect to the server on host no-such-host.invalid:5000".to_owned(),
        ),
    ];
    for (root, expected) in cases {
        let (g, o) = (Pair::new("socket-client-g"), Pair::new("socket-client-o"));
        // A socket no server listens on any more (its file is left).
        for p in [&g, &o] {
            drop(std::os::unix::net::UnixListener::bind(p.base().join("gone.sock")).unwrap());
        }
        let ours = run(gemel_path(), &g, &["a", &root, "-batch"], None);
        assert_eq!(ours.code, Some(3), "{root}: {ours:#?}");
        assert!(
            ours.stderr
                .iter()
                .any(|l| l == &format!("Error: {expected}")),
            "{root}: {ours:#?}"
        );
        if let Some(ocaml) = ocaml_path() {
            let theirs = run(&ocaml, &o, &ocaml_args(&["a", &root, "-batch"]), None);
            compare(&g, &o, &root, &ours, &theirs);
        }
    }
}
