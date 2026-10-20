//! Running external diff and merge programs.
//!
//! Ports Unison `src/external.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! On Unix the command runs through `/bin/sh -c`, and the log is its
//! output then its error output; on Windows it runs through `cmd /c`, with
//! only its output captured. One divergence: OCaml gives the program a
//! pipe for its input that it never writes to or closes until the program
//! ends, so a program that reads its input hangs; here the input is empty
//! on Unix. (On Windows, as in OCaml, it is inherited.)

use gemel_base::util::Error;
use gemel_rpc::runtime;

/// How a program ended (`Unix.process_status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    /// It exited with this status.
    Exited(i32),
    /// A signal killed it (OCaml's number: negative for the signals it
    /// knows).
    Signaled(i32),
    /// A signal stopped it.
    Stopped(i32),
}

impl ProcessStatus {
    /// Exited with 0.
    pub fn success(self) -> bool {
        self == ProcessStatus::Exited(0)
    }

    /// `Util.process_status_to_string`.
    pub fn describe(self) -> String {
        match self {
            ProcessStatus::Exited(i) => format!("Exited with status {i}"),
            ProcessStatus::Signaled(i) => format!("Killed by signal {i}"),
            ProcessStatus::Stopped(i) => format!("Stopped by signal {i}"),
        }
    }

    #[cfg(unix)]
    fn of(s: std::process::ExitStatus) -> ProcessStatus {
        use std::os::unix::process::ExitStatusExt;
        match (s.code(), s.signal(), s.stopped_signal()) {
            (Some(c), _, _) => ProcessStatus::Exited(c),
            (_, Some(sig), _) => ProcessStatus::Signaled(ocaml_signal(sig)),
            (_, _, Some(sig)) => ProcessStatus::Stopped(ocaml_signal(sig)),
            _ => ProcessStatus::Exited(-1),
        }
    }

    #[cfg(not(unix))]
    fn of(s: std::process::ExitStatus) -> ProcessStatus {
        ProcessStatus::Exited(s.code().unwrap_or(-1))
    }
}

/// OCaml's number for a POSIX signal (`caml_rev_convert_signal_number`):
/// the negative constants of `Sys` for those it knows.
#[cfg(unix)]
fn ocaml_signal(sig: i32) -> i32 {
    let known = [
        libc::SIGABRT,
        libc::SIGALRM,
        libc::SIGFPE,
        libc::SIGHUP,
        libc::SIGILL,
        libc::SIGINT,
        libc::SIGKILL,
        libc::SIGPIPE,
        libc::SIGQUIT,
        libc::SIGSEGV,
        libc::SIGTERM,
        libc::SIGUSR1,
        libc::SIGUSR2,
        libc::SIGCHLD,
        libc::SIGCONT,
        libc::SIGSTOP,
        libc::SIGTSTP,
        libc::SIGTTIN,
        libc::SIGTTOU,
        libc::SIGVTALRM,
        libc::SIGPROF,
        libc::SIGBUS,
        #[cfg(target_os = "linux")]
        libc::SIGPOLL,
        #[cfg(not(target_os = "linux"))]
        -1000,
        libc::SIGSYS,
        libc::SIGTRAP,
        libc::SIGURG,
        libc::SIGXCPU,
        libc::SIGXFSZ,
    ];
    match known.iter().position(|&s| s == sig) {
        Some(i) => -(i as i32) - 1,
        None => sig,
    }
}

/// `readChannelTillEof`: the lines, without their final newline.
fn lines(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    s.strip_suffix('\n').unwrap_or(&s).to_owned()
}

/// `runExternalProgram`: runs `cmd` through the shell, and returns how it
/// ended and its log (its output and error output, trimmed, and how it
/// ended if not with 0). Off the engine thread.
pub async fn run_external_program(cmd: &str) -> Result<(ProcessStatus, String), Error> {
    let cmd = cmd.to_owned();
    runtime::blocking(move || run(&cmd)).await
}

#[cfg(unix)]
fn run(cmd: &str) -> Result<(ProcessStatus, String), Error> {
    use std::process::{Command, Stdio};
    let out = Command::new("/bin/sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::Transient(format!("Error in running {cmd}:\n{e}")))?;
    let status = ProcessStatus::of(out.status);
    let out_log = lines(&out.stdout);
    let err_log = lines(&out.stderr);
    let (out_log, err_log) = (
        gemel_base::util::trim_whitespace(&out_log),
        gemel_base::util::trim_whitespace(&err_log),
    );
    let mut log = if out_log.is_empty() || err_log.is_empty() {
        format!("{out_log}{err_log}")
    } else {
        format!("{out_log}\n\nError Output:{err_log}")
    };
    if !status.success() {
        log.push_str(&format!("\n\n{}", status.describe()));
    }
    Ok((status, log))
}

#[cfg(windows)]
fn run(cmd: &str) -> Result<(ProcessStatus, String), Error> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    let out = Command::new("cmd")
        .raw_arg(format!("/c \"{cmd}\""))
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdout(Stdio::piped())
        .output()
        .map_err(|e| Error::Transient(format!("Error in running {cmd}:\n{e}")))?;
    let status = ProcessStatus::of(out.status);
    let mut log = gemel_base::util::trim_whitespace(&lines(&out.stdout)).to_owned();
    if !status.success() {
        log.push_str(&format!("\n\n{}", status.describe()));
    }
    Ok((status, log))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&rt, f)
    }

    #[test]
    fn logs_join_output_and_errors() {
        let run = |cmd| block_on(run_external_program(cmd)).unwrap();
        assert_eq!(run("echo hi"), (ProcessStatus::Exited(0), "hi".into()));
        assert_eq!(
            run("printf ' out \\n\\n'; echo err >&2; exit 3"),
            (
                ProcessStatus::Exited(3),
                "out\n\nError Output:err\n\nExited with status 3".into()
            )
        );
        assert_eq!(
            run("echo only-err >&2"),
            (ProcessStatus::Exited(0), "only-err".into())
        );
        // The input is empty, rather than a pipe nobody writes to.
        assert_eq!(run("cat"), (ProcessStatus::Exited(0), String::new()));
        // OCaml's signal numbers.
        assert_eq!(
            run("kill -KILL $$"),
            (
                ProcessStatus::Signaled(-7),
                "\n\nKilled by signal -7".into()
            )
        );
        assert_eq!(run("kill -TERM $$").0, ProcessStatus::Signaled(-11));
    }
}
