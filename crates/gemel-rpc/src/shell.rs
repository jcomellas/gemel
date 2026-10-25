//! Connections to a server through a remote shell: ssh, running the
//! server's command on the other host.
//!
//! Ports Unison `src/remote.ml` (`buildShellConnection` and
//! `initConnection`).
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! The shell's standard input and output are the connection; its standard
//! error is the client's, so what ssh or the server print there shows. It
//! starts with `SIGINT` blocked, as in OCaml: Ctrl-C at the terminal
//! reaches the whole foreground process group, and would end the
//! connection before the client has cleaned up.
//!
//! Since ssh doesn't get Ctrl-C, the client ends it as it exits
//! ([`end_shells`]), should it still run: ssh ends when its input does,
//! but not while it waits at the terminal for a password.
//!
//! A graphical interface, which has no terminal for ssh to ask at, runs it
//! under a terminal of its own, whose prompts it answers ([`terminal`]):
//! then ssh's standard error is that terminal, and SIGINT isn't blocked.
//! Once the server has written something, the connection counts as up,
//! and what ssh prints from then on goes to the client's standard error.
//!
//! [`terminal`]: crate::terminal

use std::process::Stdio;
use std::rc::Rc;
use std::sync::Mutex;

use gemel_base::prefs::Prefs;
use gemel_base::util::{self, Error, MY_MAJOR_VERSION, MY_NAME};
use gemel_fs::signal::Started;
use gemel_fs::system::SysError;

use crate::connection::{ADD_VERSION_NO, Connection, Endpoint, SERVER_CMD, SSH_ARGS, SSH_CMD};
use crate::handshake::{self, RPC_SERVER_CMDLINE_OVERRIDE};
use crate::terminal::{self, Interact};

/// The server's command, as the shell runs it on the other host:
/// `servercmd` (or `gemel`), with the major version if `addversionno`.
fn remote_cmd(prefs: &Prefs) -> String {
    let cmd = prefs.read(&SERVER_CMD);
    let name = if cmd.is_empty() { MY_NAME } else { cmd };
    let version = if *prefs.read(&ADD_VERSION_NO) {
        format!("-{MY_MAJOR_VERSION}")
    } else {
        String::new()
    };
    format!("{name}{version} -server {RPC_SERVER_CMDLINE_OVERRIDE}")
}

/// The shell's program and arguments: for ssh, `sshcmd`, then `-l user`,
/// `-p port`, the host, `-e none`, `sshargs` and the server's command, each
/// split into words at spaces (a backslash escapes the next character).
/// The program itself isn't split.
pub fn command(
    prefs: &Prefs,
    shell: &str,
    host: &str,
    user: Option<&str>,
    port: Option<&str>,
) -> (String, Vec<String>) {
    let ssh = shell == "ssh";
    let mut pre: Vec<String> = Vec::new();
    if let Some(u) = user {
        pre.extend(["-l".to_owned(), u.to_owned()]);
    }
    if let Some(p) = port {
        pre.extend(["-p".to_owned(), p.to_owned()]);
    }
    pre.push(host.to_owned());
    if ssh {
        pre.push("-e none".to_owned());
    }
    pre.push(if ssh {
        prefs.read(&SSH_ARGS).clone()
    } else {
        String::new()
    });
    pre.push(remote_cmd(prefs));
    let program = if ssh {
        prefs.read(&SSH_CMD).clone()
    } else {
        shell.to_owned()
    };
    let args = pre
        .iter()
        .flat_map(|s| util::split_into_words(s, ' '))
        .collect();
    (program, args)
}

/// The shells started, for [`end_shells`].
static SHELLS: Mutex<Vec<Started>> = Mutex::new(Vec::new());

/// OCaml's `at_exit end_ssh`, for each shell started: ends those still
/// running. For the program to call as it exits.
pub fn end_shells() {
    let shells = std::mem::take(&mut *SHELLS.lock().unwrap_or_else(|e| e.into_inner()));
    for shell in shells {
        gemel_fs::signal::end_child(shell);
    }
}

/// `buildShellConnection`: starts the shell for `host` and, over its input
/// and output, the client's side of a connection served by `endpoint`,
/// once the handshake is done. With `interact`, the shell runs under a
/// terminal of its own, whose prompts `interact` answers, if one can be
/// had. When the connection closes, the shell goes (tokio waits for it in
/// the background, as OCaml's `safe_waitpid` does).
pub async fn connect(
    endpoint: Rc<Endpoint>,
    (program, args): (String, Vec<String>),
    half_duplex: bool,
    interact: Option<Interact>,
) -> Result<Connection, Error> {
    let mut cmd = std::process::Command::new(&program);
    cmd.args(&args).stdin(Stdio::piped()).stdout(Stdio::piped());
    let starting = |e| {
        SysError::new("create_process", program.as_bytes(), e).to_fatal("starting shell connection")
    };
    let host = endpoint.host().clone();
    let session = match &interact {
        Some(_) => terminal::create_session(cmd).map_err(starting)?,
        None => {
            cmd.stderr(Stdio::inherit());
            gemel_fs::signal::block_interrupt_in_child(&mut cmd);
            let mut child = tokio::process::Command::from(cmd)
                .spawn()
                .map_err(starting)?;
            let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
                return Err(Error::Fatal("Internal error: the shell's pipes".into()));
            };
            terminal::Session {
                term: None,
                stdin,
                stdout,
                child: terminal::Child::Spawned(Box::new(child)),
            }
        }
    };
    let terminal::Session {
        term,
        stdin,
        stdout,
        child,
    } = session;
    if let Some(shell) = child.id().and_then(Started::new) {
        SHELLS.lock().unwrap_or_else(|e| e.into_inner()).push(shell);
    }
    let requests = match (&term, interact) {
        (Some(term), Some(interact)) => {
            Some(terminal::handle_password_requests(term.clone(), interact))
        }
        _ => None,
    };
    let ready = || {
        if let Some(r) = &requests {
            use std::io::Write;
            let _ = std::io::stderr().write_all(r.ready().as_bytes());
        }
    };
    // If the handshake fails, closing the pipes ends the shell.
    match handshake::init_connection(
        endpoint,
        Box::new(stdout),
        Box::new(stdin),
        half_duplex,
        ready,
    )
    .await
    {
        Ok(conn) => {
            // Kept until the connection closes. (Its handlers may run as
            // the engine thread ends, so this one starts nothing: a shell
            // left running is ended as the program exits.)
            conn.at_close(move || {
                if let Some(term) = term {
                    term.close();
                }
                drop(child);
            });
            Ok(conn)
        }
        Err(e) => {
            // ssh's last words, before the terminal closes.
            if let Some(r) = &requests {
                let s = r.remaining().await;
                if !s.is_empty() {
                    host.warn(&s);
                }
            }
            if let Some(term) = term {
                term.close();
                terminal::end_session(child);
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection;

    fn prefs(set: &[&str]) -> Prefs {
        let mut p = Prefs::new();
        connection::register_prefs(&mut p).unwrap();
        p.parse_cmd_line(&set.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .unwrap();
        p
    }

    #[test]
    fn ssh_command_lines_are_ocamls() {
        let (program, args) = command(&prefs(&[]), "ssh", "host", None, None);
        assert_eq!(program, "ssh");
        assert_eq!(
            args,
            ["host", "-e", "none", "gemel", "-server", "__new-rpc-mode"]
        );
        let p = prefs(&[
            "-sshcmd",
            "/usr/bin/my ssh",
            "-sshargs",
            "-C -o Foo=a\\ b",
            "-servercmd",
            "/opt/unison",
            "-addversionno",
        ]);
        let (program, args) = command(&p, "ssh", "h", Some("me"), Some("2222"));
        assert_eq!(program, "/usr/bin/my ssh");
        assert_eq!(
            args,
            [
                "-l",
                "me",
                "-p",
                "2222",
                "h",
                "-e",
                "none",
                "-C",
                "-o",
                "Foo=a b",
                &format!("/opt/unison-{MY_MAJOR_VERSION}"),
                "-server",
                "__new-rpc-mode"
            ]
        );
        // Another shell: no `-e none`, no `sshargs`.
        let (program, args) = command(&p, "rsh", "h", None, None);
        assert_eq!(program, "rsh");
        assert_eq!(args[..1], ["h"]);
    }
}
