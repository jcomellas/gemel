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
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows {
    use std::io;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::Foundation::{FALSE, TRUE};
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
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
}

/// Whether signals other than Ctrl-C can be caught (`supportSignals`).
pub fn supported() -> bool {
    cfg!(unix)
}
