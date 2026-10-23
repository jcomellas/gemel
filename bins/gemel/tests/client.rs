//! The `gemel` program reaching root `b` over ssh, when the shell or the
//! server isn't what it should be: no shell, a shell that prints something
//! else or dies, a server too old, a server that refuses. A script stands
//! in for ssh (`-sshcmd`). With `GEMEL_REFERENCE_UNISON` naming OCaml
//! Unison, it is run the same way, and both must end the same; otherwise
//! gemel's error is checked.
//!
//! Whole syncs with a server are the oracle harness's (`tools/oracle`,
//! `--remote ssh --candidate`).

#![cfg(unix)]

mod common;
#[path = "../../../crates/gemel-engine/tests/support/mod.rs"]
mod support;
#[path = "../../../crates/gemel-ui-api/tests/ui_support/mod.rs"]
mod ui_support;

use std::path::PathBuf;

use common::{compare, gemel_path, ocaml_args, ocaml_path, run};
use ui_support::Pair;

/// Octal escapes for `printf`.
fn octal(b: &[u8]) -> String {
    b.iter().map(|c| format!("\\{c:03o}")).collect()
}

/// A script standing in for ssh in `pair`'s directory, with `body`; or
/// a path to nothing.
fn shell(pair: &Pair, body: Option<&str>) -> PathBuf {
    let p = pair.base().join("ssh");
    if let Some(body) = body {
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

#[test]
fn broken_shells_and_servers_end_as_with_ocaml() {
    // A packet id other than the magic one, after an old header.
    let not_magic = octal(&gemel_rpc::protocol::encode_int(2));
    let cases: Vec<(&str, Option<String>, &str)> = vec![
        ("no shell", None, "Error in starting shell connection:\nNo such file or directory"),
        (
            "a shell that prints something else",
            Some("echo hello there; cat > /dev/null".into()),
            "Received unexpected header from the server:\n expected \"Unison RPC\\n\" but \
             received \"hello there\\n\"",
        ),
        ("a shell that dies", Some("exit 3".into()), "Lost connection with the server"),
        (
            "an old server",
            Some(format!(
                "printf 'Unison 2.51 with OCaml >= 4.01.2\\n{not_magic}'; cat > /dev/null"
            )),
            "The server runs an old and unsupported Unison version.",
        ),
        (
            "an old server that goes",
            Some("printf 'Unison 2.48\\n'".into()),
            "Lost connection with the server",
        ),
        (
            "an error",
            Some("printf 'Unison RPC\\nNOK go away\\n'; cat > /dev/null".into()),
            "Received error from the server: \"go away\".",
        ),
        (
            "versions it doesn't speak",
            Some("printf 'Unison RPC\\nVERSIONS 7 8\\n'; cat > /dev/null".into()),
            "None of server's RPC versions are supported.",
        ),
        (
            "its version refused",
            Some("printf 'Unison RPC\\nVERSIONS 1\\n'; read v; printf 'NOK no thanks\\n'; cat > /dev/null".into()),
            "Received error from the server: \"no thanks\".",
        ),
        (
            "something else than OK",
            Some("printf 'Unison RPC\\nVERSIONS 1\\n'; read v; printf 'MAYBE\\n'; cat > /dev/null".into()),
            "Received unexpected header from the server: \"MAYBE\\n\".",
        ),
    ];
    for (case, body, expected) in cases {
        let (g, o) = (Pair::new("client-g"), Pair::new("client-o"));
        let args = |p: &Pair| {
            vec![
                p.a().to_string_lossy().into_owned(),
                "ssh://server/b".to_owned(),
                "-batch".to_owned(),
                "-sshcmd".to_owned(),
                shell(p, body.as_deref()).to_string_lossy().into_owned(),
            ]
        };
        let ga = args(&g);
        let ours = run(
            gemel_path(),
            &g,
            &ga.iter().map(String::as_str).collect::<Vec<_>>(),
            None,
        );
        assert_eq!(ours.code, Some(3), "{case}: {ours:#?}");
        assert!(
            ours.stderr.join("\n").contains(expected),
            "{case}: {ours:#?}"
        );
        if let Some(ocaml) = ocaml_path() {
            let oa = args(&o);
            let oa: Vec<&str> = oa.iter().map(String::as_str).collect();
            let theirs = run(&ocaml, &o, &ocaml_args(&oa), None);
            compare(&g, &o, case, &ours, &theirs);
        }
    }
}

/// Ssh starts with Ctrl-C blocked, so the client ends it as it exits,
/// should it still run, waiting at the terminal for a password say
/// (`end_ssh`). Gemel's only: OCaml's client (2.54) doesn't stop on
/// Ctrl-C while it waits for the server.
#[test]
fn a_shell_still_running_ends_with_the_client() {
    let pair = Pair::new("client-end-shell");
    let pid_file = pair.base().join("shell.pid");
    let shim = shell(
        &pair,
        Some(&format!(
            "echo $$ > {}\nexec sleep 300 < /dev/null",
            pid_file.display()
        )),
    );
    let mut child = common::command(
        gemel_path(),
        &pair,
        &[
            "a",
            "ssh://server/b",
            "-batch",
            "-sshcmd",
            shim.to_str().unwrap(),
        ],
    )
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .spawn()
    .unwrap();
    let pid = (0..400)
        .find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(25));
            std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
        })
        .expect("the shell started");
    // The shell's `kill`: some systems have no program of that name.
    let kill = |sig: &str, pid: u32| {
        std::process::Command::new("sh")
            .args(["-c", &format!("kill {sig} {pid}")])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    assert!(kill("-INT", child.id()));
    assert_eq!(child.wait().unwrap().code(), Some(3));
    // Gone once the client has exited (and no longer a zombie of its own).
    std::thread::sleep(std::time::Duration::from_millis(200));
    let alive = kill("-0", pid);
    if alive {
        kill("-KILL", pid);
    }
    assert!(!alive, "the shell outlived the client");
}
