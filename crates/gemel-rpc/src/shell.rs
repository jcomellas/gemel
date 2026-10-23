//! Connections to a server through a remote shell: ssh, running the
//! server's command on the other host.
//!
//! Ports Unison `src/remote.ml` (`buildShellConnection`, without a
//! terminal, and `initConnection`).
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! The shell's standard input and output are the connection; its standard
//! error is the client's, so what ssh or the server print there shows. It
//! starts with `SIGINT` blocked, as in OCaml: Ctrl-C at the terminal
//! reaches the whole foreground process group, and would end the
//! connection before the client has cleaned up.
//!
//! Not yet: a pseudo-terminal for ssh's password and passphrase prompts,
//! and killing ssh when the client exits while it talks to the terminal
//! (both phase 4). Without a terminal, ssh ends when its input does.

use std::process::Stdio;
use std::rc::Rc;
use std::time::Duration;

use gemel_base::prefs::Prefs;
use gemel_base::util::{self, Error, MY_MAJOR_VERSION, MY_NAME};
use gemel_fs::system::SysError;

use crate::connection::{
    self, ADD_VERSION_NO, Connection, Endpoint, Output, SERVER_CMD, SSH_ARGS, SSH_CMD,
};
use crate::handshake::{self, RPC_SERVER_CMDLINE_OVERRIDE};
use crate::runtime;

/// How long the handshake may take (`initConnection`'s timeout).
const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(120);

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

/// `buildShellConnection`: starts the shell for `host` and, over its input
/// and output, the client's side of a connection served by `endpoint`,
/// once the handshake is done. When the connection closes, the shell is
/// waited for.
pub async fn connect(
    endpoint: Rc<Endpoint>,
    (program, args): (String, Vec<String>),
    half_duplex: bool,
) -> Result<Connection, Error> {
    let mut cmd = std::process::Command::new(&program);
    cmd.args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    gemel_fs::signal::block_interrupt_in_child(&mut cmd);
    let mut child = tokio::process::Command::from(cmd).spawn().map_err(|e| {
        SysError::new("create_process", program.as_bytes(), e).to_fatal("starting shell connection")
    })?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(Error::Fatal("Internal error: the shell's pipes".into()));
    };
    let mut input = handshake::reader(Box::new(stdout));
    let mut output: Output = Box::new(stdin);
    let version = tokio::time::timeout(
        NEGOTIATION_TIMEOUT,
        handshake::client_handshake(&mut input, &mut output, || {}),
    )
    .await
    .unwrap_or_else(|_| {
        Err(Error::Fatal(
            "Timed out negotiating connection with the server".into(),
        ))
    });
    let version = match version {
        Ok(v) => v,
        Err(e) => {
            // Closing the pipes ends the shell; it is waited for.
            drop((input, output));
            runtime::spawn_local(async move {
                let _ = child.wait().await;
            });
            return Err(e);
        }
    };
    let conn = connection::start_client(endpoint, Box::new(input), output, half_duplex);
    conn.set_version(version);
    conn.at_close(move || {
        runtime::spawn_local(async move {
            let _ = child.wait().await;
        });
    });
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

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
