//! Pseudo-terminals, so that ssh asks for passwords where a graphical
//! interface can answer.
//!
//! Ports Unison `src/pty.c` (`c_openpty`, `setControllingTerminal`), and
//! the child's side of `unix_create_session` in `src/terminal.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! On Unix only, for now: on Windows, ssh runs as a plain process.

use std::io;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// A pseudo-terminal's two ends.
pub struct Pty {
    /// What the controlling process reads and writes.
    pub master: OwnedFd,
    /// The terminal itself, as the child sees it.
    pub slave: OwnedFd,
}

/// Sets close-on-exec on `fd`: `openpty` doesn't, and a master that leaked
/// into the child would never see its terminal close.
#[allow(unsafe_code)]
fn set_cloexec(fd: &OwnedFd) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let fd = fd.as_raw_fd();
    // SAFETY: `fd` is open, and these calls only read and set its flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `c_openpty`: a new pseudo-terminal, both ends closed on exec.
#[allow(unsafe_code)]
pub fn open() -> io::Result<Pty> {
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: the out-pointers are valid; no name, settings or size.
    let r = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openpty` opened both, and nothing else owns them.
    let pty = unsafe {
        Pty {
            master: OwnedFd::from_raw_fd(master),
            slave: OwnedFd::from_raw_fd(slave),
        }
    };
    set_cloexec(&pty.master)?;
    set_cloexec(&pty.slave)?;
    Ok(pty)
}

/// The child's side of `unix_create_session`: `cmd`'s standard error is
/// the terminal `slave` (so that ssh's messages, and the server's, show
/// there), and the child leads a session of its own, whose controlling
/// terminal is `slave`, with echo off.
#[allow(unsafe_code)]
pub fn controlled_by(cmd: &mut Command, slave: &OwnedFd) -> io::Result<()> {
    cmd.stderr(Stdio::from(slave.try_clone()?));
    // SAFETY: only async-signal-safe calls, between fork and exec; the
    // standard streams are set up before this runs.
    unsafe {
        cmd.pre_exec(|| {
            let fail = || Err(io::Error::last_os_error());
            if libc::setsid() < 0 {
                return fail();
            }
            // `setControllingTerminal`, on what is now standard error.
            if libc::ioctl(2, libc::TIOCSCTTY as _, 0) < 0 {
                return fail();
            }
            let mut tio = std::mem::zeroed::<libc::termios>();
            if libc::tcgetattr(2, &mut tio) != 0 {
                return fail();
            }
            tio.c_lflag &= !libc::ECHO;
            if libc::tcsetattr(2, libc::TCSANOW, &tio) != 0 {
                return fail();
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// A child under a pseudo-terminal: its terminal is the slave, with
    /// echo off, and what it writes to `/dev/tty` comes out of the master.
    #[test]
    fn a_child_has_the_terminal_to_itself() {
        let pty = open().unwrap();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "stty -a < /dev/tty > /dev/tty; echo done > /dev/tty"])
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        controlled_by(&mut cmd, &pty.slave).unwrap();
        let mut child = cmd.spawn().unwrap();
        // Read as it comes: once the child has gone and the parent closes
        // its slave, what is left may be lost (or the read fail with EIO).
        // The master is kept until the child has gone: closing it hangs
        // the child up.
        let mut master = std::fs::File::from(pty.master);
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            let mut buf = [0; 4096];
            // How the reading ended, for the assertions.
            let mut end = "with the child's last words".to_owned();
            while !String::from_utf8_lossy(&out).contains("done") {
                match master.read(&mut buf) {
                    Ok(0) => {
                        end = "at the end of the file".to_owned();
                        break;
                    }
                    Ok(n) => out.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        end = format!("with {e}");
                        break;
                    }
                }
            }
            (out, end, master)
        });
        let status = child.wait().unwrap();
        let (out, end, master) = reader.join().unwrap();
        drop(master);
        drop(pty.slave);
        let out = String::from_utf8_lossy(&out);
        let what = format!("{status}, reading ended {end}:\n{out}");
        assert!(status.success(), "{what}");
        assert!(out.contains("-echo"), "{what}");
        assert!(out.contains("done"), "{what}");
    }
}
