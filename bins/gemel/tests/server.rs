//! `gemel -server` as ssh starts it: what it writes for what a client
//! sends, and how it fails. Each case is checked against what OCaml's
//! server writes (recorded from 2.54), and, with `GEMEL_REFERENCE_UNISON`
//! naming OCaml Unison, against that server too: the same output, byte
//! for byte, the same errors, the same exit code and the same Unison
//! directory.
//!
//! Whole syncs with a gemel server are the oracle harness's (`tools/oracle`,
//! `--remote ssh --candidate-server`).

#[path = "../../../crates/gemel-engine/tests/support/mod.rs"]
mod support;

use std::ffi::OsStr;
use std::io::Write;
use std::path::Path as StdPath;
use std::process::{Command, Stdio};

use support::TempDir;

/// The write token: id 0.
const TOKEN: &[u8] = b"\0\0\0\0\x11";

/// How a server run ended.
#[derive(Debug, PartialEq, Eq)]
struct Served {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: String,
    /// Whether the Unison directory was made, and its permissions.
    unison_dir: Option<u32>,
}

/// Runs `server -server __new-rpc-mode` in `dir` with `input` on stdin;
/// `home` is its home (none unset), and its Unison directory is `dir/u`.
fn serve(server: &OsStr, dir: &StdPath, home: Option<&str>, unison: &str, input: &[u8]) -> Served {
    let u = dir.join(unison);
    let _ = std::fs::remove_dir_all(dir.join("u"));
    let mut c = Command::new(server);
    c.args(["-server", "__new-rpc-mode"])
        .current_dir(dir)
        .env("UNISON", &u)
        .env_remove("HOME")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(h) = home {
        c.env("HOME", dir.join(h));
    }
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    let unison_dir = std::fs::metadata(&u).ok().map(|m| mode(&m));
    Served {
        code: out.status.code(),
        stdout: out.stdout,
        stderr: String::from_utf8_lossy(&out.stderr).replace(&*dir.to_string_lossy(), "<D>"),
        unison_dir,
    }
}

#[cfg(unix)]
fn mode(m: &std::fs::Metadata) -> u32 {
    std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o777
}

#[cfg(not(unix))]
fn mode(_: &std::fs::Metadata) -> u32 {
    0
}

fn header() -> Vec<u8> {
    b"Unison RPC\nVERSIONS 1\n".to_vec()
}

/// A server that answered with `reply`, then handed out the token, until
/// the client hung up.
fn answered(reply: &str) -> Served {
    Served {
        code: Some(0),
        stdout: [&header()[..], reply.as_bytes(), TOKEN].concat(),
        stderr: String::new(),
        unison_dir: Some(if cfg!(unix) { 0o700 } else { 0 }),
    }
}

/// A case: its name, the server's home (none unset), its Unison
/// directory, what the client sends, and how the server ends.
type Case = (
    &'static str,
    Option<&'static str>,
    &'static str,
    &'static [u8],
    Served,
);

/// What the client sends, and what OCaml's server (2.54) does with it.
fn cases() -> Vec<Case> {
    let failed = |stderr: &str| Served {
        code: Some(1),
        stdout: Vec::new(),
        stderr: stderr.to_owned(),
        unison_dir: None,
    };
    vec![
        ("accepted", Some("."), "u", b"VERSION 1\n", answered("OK\n")),
        (
            "unsupported",
            Some("."),
            "u",
            b"VERSION 2\n",
            answered(
                "NOK Client RPC version not supported. Version received from client: \"2\". \
                 Supported RPC versions: \"1\"\n",
            ),
        ),
        (
            "an error",
            Some("."),
            "u",
            b"NOK oops\n",
            answered(
                "NOK Could not negotiate RPC version. Received unexpected error from the \
                 client: \"oops\"\n",
            ),
        ),
        (
            "something else",
            Some("."),
            "u",
            b"HELLO\nmore",
            answered(
                "NOK Could not negotiate RPC version. Received unexpected header from the \
                 client: \"HELLO\\nmore\"\n",
            ),
        ),
        (
            "no home",
            None,
            "u",
            b"VERSION 1\n",
            Served {
                stderr: "Environment variable HOME unbound: executing server in current \
                         directory\n"
                    .to_owned(),
                ..answered("OK\n")
            },
        ),
        (
            "a home that isn't there",
            Some("nowhere"),
            "u",
            b"VERSION 1\n",
            Served {
                unison_dir: Some(if cfg!(unix) { 0o700 } else { 0 }),
                ..failed(
                    "Unison server failed: Error in changing working directory:\n<D>/nowhere: \
                     No such file or directory\n",
                )
            },
        ),
        (
            "no Unison directory",
            Some("."),
            "no/such/u",
            b"VERSION 1\n",
            failed(
                "Unison server failed: Error in creating unison directory <D>/no/such/u:\n\
                 No such file or directory [mkdir(<D>/no/such/u)]\n",
            ),
        ),
    ]
}

#[test]
fn the_server_answers_and_fails_as_ocamls_does() {
    let dir = TempDir::new("server");
    // On macOS the temporary directory is reached through a link, which
    // OCaml's messages resolve.
    let d = std::fs::canonicalize(&dir.0).unwrap();
    for (case, home, unison, input, expected) in cases() {
        // Windows' messages for the failures are its own.
        if cfg!(windows) && expected.code != Some(0) {
            continue;
        }
        let gemel = serve(
            OsStr::new(env!("CARGO_BIN_EXE_gemel")),
            &d,
            home,
            unison,
            input,
        );
        assert_eq!(gemel, expected, "{case}");
        if let Some(ocaml) = std::env::var_os("GEMEL_REFERENCE_UNISON") {
            let ocaml = serve(&ocaml, &d, home, unison, input);
            assert_eq!(gemel, ocaml, "{case}: against OCaml");
        }
    }
}

/// [`serve`] with standard input and output one end of a socket pair, as
/// sshd may give them (OpenSSH unless built with `USE_PIPES`): the client
/// sends `input`, then shuts its side, and gets what the server wrote.
#[cfg(unix)]
fn serve_on_socket(server: &OsStr, dir: &StdPath, input: &[u8]) -> Served {
    use std::io::Read;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    let u = dir.join("u");
    let _ = std::fs::remove_dir_all(&u);
    let (mut ours, theirs) = UnixStream::pair().unwrap();
    let child = Command::new(server)
        .args(["-server", "__new-rpc-mode"])
        .current_dir(dir)
        .env("UNISON", &u)
        .env("HOME", dir)
        .stdin(Stdio::from(OwnedFd::from(theirs.try_clone().unwrap())))
        .stdout(Stdio::from(OwnedFd::from(theirs)))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    ours.write_all(input).unwrap();
    ours.shutdown(std::net::Shutdown::Write).unwrap();
    let mut stdout = Vec::new();
    ours.read_to_end(&mut stdout).unwrap();
    let out = child.wait_with_output().unwrap();
    Served {
        code: out.status.code(),
        stdout,
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        unison_dir: std::fs::metadata(&u).ok().map(|m| mode(&m)),
    }
}

/// On a socket the server answers as on pipes.
#[cfg(unix)]
#[test]
fn the_server_answers_on_a_socket_as_on_pipes() {
    let dir = TempDir::new("server-socket");
    let d = std::fs::canonicalize(&dir.0).unwrap();
    let gemel = serve_on_socket(OsStr::new(env!("CARGO_BIN_EXE_gemel")), &d, b"VERSION 1\n");
    assert_eq!(gemel, answered("OK\n"));
    if let Some(ocaml) = std::env::var_os("GEMEL_REFERENCE_UNISON") {
        assert_eq!(
            gemel,
            serve_on_socket(&ocaml, &d, b"VERSION 1\n"),
            "against OCaml"
        );
    }
}
