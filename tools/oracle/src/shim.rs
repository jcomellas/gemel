//! The `-sshcmd` shim, which stands in for ssh: a copy of the harness,
//! as `ssh-shim/ssh` beside the work directory, so that it runs on Windows
//! too, where a client can only start a program. (A copy, not a link: a
//! running program's file can't be removed on Windows.)
//!
//! It records its arguments in `remote/argv`, skips what precedes the
//! server command (`[-l user] [-p port] host -e none`), and runs that
//! command as the server, with the server's environment and its standard
//! error in `remote/stderr`. With `old-server` beside it, it drops
//! `__new-rpc-mode`, so that a 2.52 or 2.53 server starts in its
//! compatibility mode.
//!
//! OCaml kills ssh as it exits (`SIGTERM`, then `SIGKILL`; on Windows,
//! `TerminateProcess`), and a server it reached through ssh then ends on
//! the end of its input. So the server runs under a keeper, another run of
//! the shim that the kill doesn't reach, which writes the server's exit
//! code to `remote/exit` once it has exited: the harness waits for that
//! before it reads the server's standard error.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use crate::run::SERVER_HOST;

/// The shim's name.
pub const NAME: &str = "ssh";

/// What the shim runs as its keeper with, before the server's command.
const KEEPER: &str = "__oracle-keeper";

/// The file beside the shim that names the work directory.
const WORK: &str = "work-directory";

/// The shim for `work`, as `-sshcmd`, copied there first if need be;
/// `old_server` says to drop `__new-rpc-mode`.
pub fn install(work: &Path, old_server: bool) -> io::Result<PathBuf> {
    let dir = work.with_file_name("ssh-shim");
    let shim = dir
        .join(NAME)
        .with_extension(std::env::consts::EXE_EXTENSION);
    if !shim.exists() {
        fs::create_dir_all(&dir)?;
        fs::copy(std::env::current_exe()?, &shim)?;
    }
    fs::write(dir.join(WORK), work.as_os_str().as_encoded_bytes())?;
    let marker = dir.join("old-server");
    if old_server {
        fs::write(marker, "")?;
    } else if marker.exists() {
        fs::remove_file(marker)?;
    }
    Ok(shim)
}

/// Where this program runs as the shim: the shim's directory and the
/// work directory.
pub fn running_as() -> Option<(PathBuf, PathBuf)> {
    let me = std::env::current_exe().ok()?;
    if me.file_stem()? != NAME {
        return None;
    }
    let dir = me.parent()?;
    let work = fs::read_to_string(dir.join(WORK)).ok()?;
    Some((dir.to_owned(), PathBuf::from(work)))
}

/// Runs as the shim, or as its keeper, from `dir` for `work`.
pub fn main(dir: &Path, work: &Path) -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.split_first() {
        Some((k, server)) if k == KEEPER => keep(work, server),
        _ => shim(dir, work, &args),
    };
    match result {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(e) => {
            eprintln!("oracle's ssh shim: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Records `args`, then runs their server command under a keeper, and
/// ends as it does.
fn shim(dir: &Path, work: &Path, args: &[String]) -> io::Result<i32> {
    let remote = work.join("remote");
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(remote.join("argv"))?;
    writeln!(log, "{}", args.join(" "))?;
    drop(log);
    let start = args
        .iter()
        .position(|a| a == "-e")
        .map_or(args.len(), |i| i + 2);
    let old_server = dir.join("old-server").exists();
    let server: Vec<&String> = args[start.min(args.len())..]
        .iter()
        .filter(|a| !(old_server && *a == "__new-rpc-mode"))
        .collect();
    let status = Command::new(std::env::current_exe()?)
        .arg(KEEPER)
        .args(server)
        .status()?;
    Ok(status.code().unwrap_or(1))
}

/// Runs `server` with the server's environment, then writes its exit code
/// to `remote/exit`.
fn keep(work: &Path, server: &[String]) -> io::Result<i32> {
    let remote = work.join("remote");
    let Some((program, args)) = server.split_first() else {
        return Err(io::Error::other("no server command"));
    };
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .open(remote.join("stderr"))?;
    let code = Command::new(program)
        .args(args)
        .env("UNISON", work.join("unison-server"))
        .env("HOME", work)
        .env("UNISONLOCALHOSTNAME", SERVER_HOST)
        .stderr(Stdio::from(stderr))
        .status()
        .map_or(127, |s| s.code().unwrap_or(1));
    fs::write(remote.join("exit"), format!("{code}\n"))?;
    Ok(code)
}
