//! The terminal, as the text interface uses it: its modes, and reading
//! keys as they are typed.
//!
//! Ports `System.terminalStateFunctions` and `System.termVtCapable`
//! (`system_generic.ml`, and `system_win.ml` with its stubs).
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Not ported: on Windows, allocating a console for a process started
//! without one (`win_init_console`); gemel is a console program.

use std::io;

/// The terminal's state when it was taken, to go back to, and how to
/// switch it to raw mode (`terminalStateFunctions`).
pub struct TerminalState {
    #[cfg(unix)]
    old: libc::termios,
    #[cfg(windows)]
    win: windows::State,
}

impl TerminalState {
    /// The terminal's state now. Fails when stdin isn't a terminal (on
    /// Windows, when there is no console).
    #[cfg(unix)]
    #[allow(unsafe_code)]
    pub fn new() -> io::Result<TerminalState> {
        // SAFETY: `old` is a valid out-pointer for the call.
        let mut old: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(0, &mut old) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(TerminalState { old })
    }

    /// The terminal's state now. Fails when stdin isn't a terminal (on
    /// Windows, when there is no console).
    #[cfg(windows)]
    pub fn new() -> io::Result<TerminalState> {
        Ok(TerminalState {
            win: windows::State::new()?,
        })
    }

    #[cfg(unix)]
    #[allow(unsafe_code)]
    fn set(&self, t: &libc::termios) -> io::Result<()> {
        // SAFETY: `t` is a valid termios.
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// `defaultTerminal`: back to the state taken.
    pub fn restore(&self) -> io::Result<()> {
        #[cfg(unix)]
        return self.set(&self.old);
        #[cfg(windows)]
        return self.win.restore();
    }

    /// `rawTerminal`: keys are read as they are typed, and not echoed.
    pub fn raw(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            let mut t = self.old;
            t.c_lflag &= !(libc::ICANON | libc::ECHO);
            t.c_cc[libc::VMIN] = 1;
            self.set(&t)
        }
        #[cfg(windows)]
        return self.win.raw();
    }

    /// `startReading`: before reading a key. (On Windows, Ctrl-C is then
    /// read as a key, since it doesn't interrupt a read without line
    /// input.)
    pub fn start_reading(&self) -> io::Result<()> {
        #[cfg(windows)]
        return self.win.start_reading();
        #[cfg(unix)]
        Ok(())
    }

    /// `stopReading`: after reading a key.
    pub fn stop_reading(&self) -> io::Result<()> {
        #[cfg(windows)]
        return self.win.raw();
        #[cfg(unix)]
        Ok(())
    }
}

/// Which of the standard outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// stdout.
    Stdout,
    /// stderr.
    Stderr,
}

/// `termVtCapable`: the output is a terminal that understands VT100
/// escape sequences (on Windows, a console in that mode).
pub fn vt_capable(o: Output) -> bool {
    #[cfg(unix)]
    {
        use std::io::IsTerminal;
        match o {
            Output::Stdout => io::stdout().is_terminal(),
            Output::Stderr => io::stderr().is_terminal(),
        }
    }
    #[cfg(windows)]
    return windows::vt_capable(o);
}

/// `Unix.read Unix.stdin`: at most `buf.len()` bytes, from one read,
/// without buffering. 0 at the end of the input.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn read_stdin(buf: &mut [u8]) -> io::Result<usize> {
    loop {
        // SAFETY: `buf` is valid for `buf.len()` bytes.
        let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// `Unix.read Unix.stdin`: at most `buf.len()` bytes, from one read,
/// without buffering. 0 at the end of the input.
#[cfg(windows)]
pub fn read_stdin(buf: &mut [u8]) -> io::Result<usize> {
    windows::read_stdin(buf)
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows {
    //! `win_get_console_mode`, `win_set_console_mode`, the output code
    //! page and `win_vt_capable` (system_win_stubs.c).

    use std::fs::File;
    use std::io::{self, Read};
    use std::mem::ManuallyDrop;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};

    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetConsoleOutputCP,
        GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        SetConsoleMode, SetConsoleOutputCP,
    };

    use super::Output;

    fn std_handle(which: STD_HANDLE) -> Option<HANDLE> {
        // SAFETY: a plain value.
        let h = unsafe { GetStdHandle(which) };
        (h != INVALID_HANDLE_VALUE && !h.is_null()).then_some(h)
    }

    fn mode(h: HANDLE) -> io::Result<CONSOLE_MODE> {
        let mut m = 0;
        // SAFETY: `m` is a valid out-pointer.
        if unsafe { GetConsoleMode(h, &mut m) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(m)
    }

    fn set_mode(h: HANDLE, m: CONSOLE_MODE) -> io::Result<()> {
        // SAFETY: plain values.
        if unsafe { SetConsoleMode(h, m) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn set_output_cp(cp: u32) -> io::Result<()> {
        // SAFETY: a plain value.
        if unsafe { SetConsoleOutputCP(cp) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub struct State {
        /// The console's input (`init_conin`): `CONIN$`, whatever stdin is.
        conin: File,
        old_mode: CONSOLE_MODE,
        old_cp: u32,
        vt_input: CONSOLE_MODE,
    }

    impl State {
        pub fn new() -> io::Result<State> {
            let conin = File::options().read(true).write(true).open("CONIN$")?;
            Ok(State {
                old_mode: mode(conin.as_raw_handle())?,
                conin,
                // SAFETY: no arguments.
                old_cp: unsafe { GetConsoleOutputCP() },
                // ENABLE_VIRTUAL_TERMINAL_INPUT
                vt_input: if vt_capable(Output::Stdout) { 0x200 } else { 0 },
            })
        }

        pub fn restore(&self) -> io::Result<()> {
            set_mode(self.conin.as_raw_handle(), self.old_mode)?;
            set_output_cp(self.old_cp)
        }

        pub fn raw(&self) -> io::Result<()> {
            set_mode(self.conin.as_raw_handle(), 0x19)?;
            set_output_cp(65001)
        }

        pub fn start_reading(&self) -> io::Result<()> {
            set_mode(self.conin.as_raw_handle(), 0x18 | self.vt_input)
        }
    }

    pub fn vt_capable(o: Output) -> bool {
        let which = match o {
            Output::Stdout => STD_OUTPUT_HANDLE,
            Output::Stderr => STD_ERROR_HANDLE,
        };
        std_handle(which)
            .is_some_and(|h| mode(h).is_ok_and(|m| m & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0))
    }

    pub fn read_stdin(buf: &mut [u8]) -> io::Result<usize> {
        let h =
            std_handle(STD_INPUT_HANDLE).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        // SAFETY: the process's stdin handle, valid while it runs; it isn't
        // closed, as the file is never dropped.
        let mut f = ManuallyDrop::new(unsafe { File::from_raw_handle(h) });
        match f.read(buf) {
            // A pipe whose writer has gone: the end of the input.
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(0),
            r => r,
        }
    }
}
