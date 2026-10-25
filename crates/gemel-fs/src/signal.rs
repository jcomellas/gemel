//! Signals, as the text interface catches them: Ctrl-C (`Sys.catch_break`
//! and its own handler during a sync), termination during a sync,
//! suspending and resuming in raw mode, and the request to stop between
//! syncs.
//!
//! OCaml's handlers run in the program itself; here a handler only notes
//! the signal, and a thread of [`listen`]'s hands it to the frontend,
//! which does what OCaml's handler does. Reads and other system calls are
//! restarted after a handler (`SA_RESTART`), so the engine's aren't
//! disturbed.
//!
//! On Windows only Ctrl-C (and Ctrl-Break) can be caught, as OCaml catches
//! it there: the console calls a handler on a thread of its own.

/// A signal the text interface catches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// `SIGINT`: Ctrl-C.
    Interrupt,
    /// `SIGTERM`.
    Terminate,
    /// `SIGTSTP`: Ctrl-Z.
    Suspend,
    /// `SIGCONT`: resumed after Ctrl-Z.
    Resume,
    /// `SIGUSR2`: stop between syncs.
    SafeStop,
}

/// How a signal was handled before [`catch`], to [`restore`] it.
pub struct Previous {
    #[cfg(unix)]
    action: libc::sigaction,
    #[cfg(windows)]
    caught: bool,
}

#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;

#[cfg(unix)]
#[allow(unsafe_code)]
mod unix {
    use std::io;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI32, Ordering};

    use super::{Previous, Signal};

    /// Where the handler writes the signals caught.
    static PIPE: AtomicI32 = AtomicI32::new(-1);
    static LISTENING: Mutex<bool> = Mutex::new(false);

    fn number(s: Signal) -> libc::c_int {
        match s {
            Signal::Interrupt => libc::SIGINT,
            Signal::Terminate => libc::SIGTERM,
            Signal::Suspend => libc::SIGTSTP,
            Signal::Resume => libc::SIGCONT,
            Signal::SafeStop => libc::SIGUSR2,
        }
    }

    fn of_number(n: libc::c_int) -> Option<Signal> {
        [
            Signal::Interrupt,
            Signal::Terminate,
            Signal::Suspend,
            Signal::Resume,
            Signal::SafeStop,
        ]
        .into_iter()
        .find(|s| number(*s) == n)
    }

    extern "C" fn handler(n: libc::c_int) {
        let b = n as u8;
        let fd = PIPE.load(Ordering::Relaxed);
        if fd >= 0 {
            // SAFETY: `write` is async-signal-safe and `b` is one valid
            // byte; errno is saved and restored around it.
            unsafe {
                let errno = *errno_location();
                libc::write(fd, (&b as *const u8).cast(), 1);
                *errno_location() = errno;
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe fn errno_location() -> *mut libc::c_int {
        // SAFETY: always valid for the calling thread.
        unsafe { libc::__errno_location() }
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    unsafe fn errno_location() -> *mut libc::c_int {
        // SAFETY: always valid for the calling thread.
        unsafe { libc::__error() }
    }

    /// Hands each signal caught to `f`, on a thread of its own, from now
    /// on. Once per process; later calls fail.
    pub fn listen(mut f: impl FnMut(Signal) + Send + 'static) -> io::Result<()> {
        let mut listening = LISTENING.lock().unwrap_or_else(|e| e.into_inner());
        if *listening {
            return Err(io::Error::other("already listening for signals"));
        }
        let mut fds = [0; 2];
        // SAFETY: `fds` has room for the two descriptors.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        for fd in fds {
            // SAFETY: `fd` was just opened.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        let [r, w] = fds;
        std::thread::Builder::new()
            .name("gemel-signals".into())
            .spawn(move || {
                loop {
                    let mut b = 0u8;
                    // SAFETY: `b` is one writable byte.
                    let n = unsafe { libc::read(r, (&mut b as *mut u8).cast(), 1) };
                    if n == 1 {
                        if let Some(s) = of_number(b as libc::c_int) {
                            f(s);
                        }
                    } else if n == 0
                        || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
                    {
                        return;
                    }
                }
            })?;
        PIPE.store(w, Ordering::Relaxed);
        *listening = true;
        Ok(())
    }

    fn sigaction(s: Signal, handler: libc::sighandler_t) -> io::Result<Previous> {
        // SAFETY: both structures are valid for the call; `handler` is
        // `SIG_DFL` or a function that only writes to a pipe.
        unsafe {
            let mut new: libc::sigaction = std::mem::zeroed();
            new.sa_sigaction = handler;
            new.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut new.sa_mask);
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(number(s), &new, &mut old) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Previous { action: old })
        }
    }

    /// Catches `s` from now on, for [`listen`]'s thread; how it was handled
    /// before.
    pub fn catch(s: Signal) -> io::Result<Previous> {
        sigaction(
            s,
            handler as extern "C" fn(libc::c_int) as libc::sighandler_t,
        )
    }

    /// `Sys.Signal_default`.
    pub fn set_default(s: Signal) -> io::Result<()> {
        sigaction(s, libc::SIG_DFL).map(drop)
    }

    /// Handles `s` as it was before [`catch`].
    pub fn restore(s: Signal, p: &Previous) {
        // SAFETY: `p.action` was returned by `sigaction`.
        unsafe { libc::sigaction(number(s), &p.action, std::ptr::null_mut()) };
    }

    /// `Unix.kill (Unix.getpid ()) s`.
    pub fn raise(s: Signal) {
        // SAFETY: plain values.
        unsafe { libc::kill(libc::getpid(), number(s)) };
    }

    /// A child process started, for [`end_child`].
    pub struct Started(libc::pid_t);

    impl Started {
        /// The child `pid`.
        pub fn new(pid: u32) -> Option<Started> {
            libc::pid_t::try_from(pid).ok().map(Started)
        }
    }

    /// `end_ssh`: if the child is still running, asks it to stop
    /// (`SIGTERM`), gives it a moment to restore the terminal's settings,
    /// then kills it (`SIGKILL`). Nothing if it has ended, or isn't a child.
    pub fn end_child(child: Started) {
        let pid = child.0;
        let mut status = 0;
        // SAFETY: `status` is writable; WNOHANG doesn't wait.
        if unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } != 0 {
            return;
        }
        // SAFETY: plain values.
        unsafe { libc::kill(pid, libc::SIGTERM) };
        std::thread::sleep(std::time::Duration::from_millis(10));
        // SAFETY: plain values.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }

    /// Asks the child `pid` to stop (`SIGTERM`), as `safe_waitpid` does
    /// before it kills.
    pub fn terminate(pid: u32) {
        if let Ok(pid) = libc::pid_t::try_from(pid) {
            // SAFETY: plain values.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }

    /// `Util.blockSignals [Sys.sigint]` around starting a process: `cmd`
    /// starts with `SIGINT` blocked, so that Ctrl-C at the terminal, which
    /// reaches the whole foreground process group, doesn't reach it.
    pub fn block_interrupt_in_child(cmd: &mut std::process::Command) {
        use std::os::unix::process::CommandExt;
        // SAFETY: only async-signal-safe calls, between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                let mut set = std::mem::zeroed::<libc::sigset_t>();
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, libc::SIGINT);
                if libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows {
    use std::io;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE, TRUE, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    };
    use windows_sys::core::BOOL;

    use super::{Previous, Signal};

    type Listener = Box<dyn FnMut(Signal) + Send>;

    static LISTENER: Mutex<Option<Listener>> = Mutex::new(None);
    static CAUGHT: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn handler(event: u32) -> BOOL {
        if (event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT) && CAUGHT.load(Ordering::SeqCst) {
            if let Some(f) = LISTENER.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                f(Signal::Interrupt);
            }
            return TRUE;
        }
        FALSE
    }

    /// Hands each signal caught to `f`, from now on. Once per process;
    /// later calls fail.
    pub fn listen(f: impl FnMut(Signal) + Send + 'static) -> io::Result<()> {
        let mut l = LISTENER.lock().unwrap_or_else(|e| e.into_inner());
        if l.is_some() {
            return Err(io::Error::other("already listening for signals"));
        }
        // SAFETY: `handler` stays valid for the life of the process.
        if unsafe { SetConsoleCtrlHandler(Some(handler), TRUE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        *l = Some(Box::new(f));
        Ok(())
    }

    /// Catches `s` from now on, for [`listen`]'s handler; how it was
    /// handled before. Only Ctrl-C can be.
    pub fn catch(s: Signal) -> io::Result<Previous> {
        if s != Signal::Interrupt {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        Ok(Previous {
            caught: CAUGHT.swap(true, Ordering::SeqCst),
        })
    }

    /// `Sys.Signal_default`.
    pub fn set_default(s: Signal) -> io::Result<()> {
        if s != Signal::Interrupt {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        CAUGHT.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Handles `s` as it was before [`catch`].
    pub fn restore(s: Signal, p: &Previous) {
        if s == Signal::Interrupt {
            CAUGHT.store(p.caught, Ordering::SeqCst);
        }
    }

    /// Not on Windows.
    pub fn raise(_: Signal) {}

    /// A child process started, for [`end_child`]: a handle of its own,
    /// so that its process id can't be another's by then.
    pub struct Started(HANDLE);

    // SAFETY: a process handle may be used and closed from any thread.
    unsafe impl Send for Started {}

    impl Started {
        /// The child `pid`, if it can be opened.
        pub fn new(pid: u32) -> Option<Started> {
            // SAFETY: plain values; a null handle is a failure.
            let h = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, FALSE, pid) };
            (!h.is_null()).then_some(Started(h))
        }
    }

    impl Drop for Started {
        fn drop(&mut self) {
            // SAFETY: the handle is ours, closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// `end_ssh`, as OCaml's runs on Windows: if the child is still
    /// running, a moment later it is terminated. (`Unix.kill` there only
    /// takes `SIGKILL`, so the `SIGTERM` before it does nothing.)
    pub fn end_child(child: Started) {
        // SAFETY: the handle is open; a zero timeout doesn't wait.
        if unsafe { WaitForSingleObject(child.0, 0) } != WAIT_TIMEOUT {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        // SAFETY: the handle is open, with the right to terminate.
        unsafe { TerminateProcess(child.0, 0) };
    }

    /// Nothing on Windows, as in OCaml, where `Unix.sigprocmask` doesn't
    /// exist there.
    pub fn block_interrupt_in_child(_: &mut std::process::Command) {}
}

/// Whether signals other than Ctrl-C can be caught (`supportSignals`).
pub fn supported() -> bool {
    cfg!(unix)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child still running is ended at once; one that has ended is left.
    #[test]
    fn a_running_child_is_ended() {
        let mut sleeper = if cfg!(windows) {
            let mut c = std::process::Command::new("ping");
            c.args(["-n", "30", "127.0.0.1"]);
            c
        } else {
            let mut c = std::process::Command::new("sleep");
            c.arg("30");
            c
        };
        let mut child = sleeper.stdout(std::process::Stdio::null()).spawn().unwrap();
        let start = std::time::Instant::now();
        end_child(Started::new(child.id()).unwrap());
        let status = child.wait().unwrap();
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        // `TerminateProcess` with 0, as OCaml's `Unix.kill` calls it.
        #[cfg(windows)]
        assert_eq!(status.code(), Some(0));
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(libc::SIGTERM));
        }
        // Reaped: nothing to do.
        if let Some(gone) = Started::new(child.id()) {
            end_child(gone);
        }
    }
}
