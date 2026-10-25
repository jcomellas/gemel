//! Pseudo-consoles (ConPTY), so that ssh asks for passwords where a
//! graphical interface can answer, on Windows.
//!
//! Ports Unison `src/pty.c` (`win_openpty`, `win_closepty`,
//! `w_create_process_pty`), and `make_cmdline` and the start of
//! `win_create_session` in `src/terminal.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Not ported: `win_alloc_console`, OCaml's fallback without a
//! pseudo-console, which opens a console window for a GUI started without
//! one. gemel-gui is a console program.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, TRUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::SearchPathW;
use windows_sys::Win32::System::Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetPriorityClass, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject,
};

/// A pseudo-console, closed when dropped (`win_closepty`).
pub struct Console(HPCON);

impl Drop for Console {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: the handle is ours, closed once.
        unsafe { ClosePseudoConsole(self.0) };
    }
}

/// A process started on a pseudo-console.
pub struct Process {
    handle: OwnedHandle,
    id: u32,
}

impl Process {
    /// Its process id.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Whether it ends within `ms` milliseconds (or has).
    #[allow(unsafe_code)]
    pub fn wait(&self, ms: u32) -> bool {
        // SAFETY: the handle is open.
        unsafe { WaitForSingleObject(self.handle.as_raw_handle(), ms) == WAIT_OBJECT_0 }
    }

    /// Ends it (`Unix.kill` with `SIGKILL`, on Windows).
    #[allow(unsafe_code)]
    pub fn terminate(&self) {
        // SAFETY: the handle is open, with the right to terminate.
        unsafe { TerminateProcess(self.handle.as_raw_handle(), 1) };
    }
}

/// A process started on a pseudo-console, and its ends.
pub struct Started {
    /// The process.
    pub process: Process,
    /// The pseudo-console, which must outlive what is read from it.
    pub console: Console,
    /// What the console shows.
    pub output: File,
    /// What is typed at it.
    pub input: File,
    /// The process's standard input.
    pub stdin: OwnedHandle,
    /// Its standard output.
    pub stdout: OwnedHandle,
}

/// `CreatePipe`: its read and write ends, neither inherited.
#[allow(unsafe_code)]
fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (mut r, mut w): (HANDLE, HANDLE) = (null_mut(), null_mut());
    // SAFETY: the out-pointers are valid; no security attributes.
    if unsafe { CreatePipe(&mut r, &mut w, null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreatePipe opened both, and nothing else owns them.
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(r),
            OwnedHandle::from_raw_handle(w),
        )
    })
}

/// `h`, duplicated to be inherited.
#[allow(unsafe_code)]
fn inheritable(h: &OwnedHandle) -> io::Result<OwnedHandle> {
    let mut dup: HANDLE = null_mut();
    // SAFETY: both handles are this process's; the out-pointer is valid.
    let ok = unsafe {
        let me = GetCurrentProcess();
        DuplicateHandle(
            me,
            h.as_raw_handle(),
            me,
            &mut dup,
            0,
            TRUE,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: DuplicateHandle opened it, and nothing else owns it.
    Ok(unsafe { OwnedHandle::from_raw_handle(dup) })
}

/// `s` as a NUL-terminated wide string.
fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// `Filename.quote`, as OCaml's runtime quotes for Windows: in double
/// quotes, a quote escaped with a backslash, and the backslashes before a
/// quote doubled. (Backslashes at the end go after the closing quote, as
/// OCaml has them.)
fn quote(s: &str) -> String {
    let mut b = String::from('"');
    let mut bs = 0;
    for c in s.chars() {
        match c {
            '\\' => bs += 1,
            '"' => {
                b.extend(std::iter::repeat_n('\\', 2 * bs + 1));
                b.push('"');
                bs = 0;
            }
            c => {
                b.extend(std::iter::repeat_n('\\', bs));
                b.push(c);
                bs = 0;
            }
        }
    }
    b.push('"');
    b.extend(std::iter::repeat_n('\\', bs));
    b
}

/// `make_cmdline`: the arguments in a command line, those with a space or
/// a quote quoted.
fn make_cmdline(args: &[OsString]) -> String {
    let maybe_quote = |a: &OsString| {
        let a = a.to_string_lossy();
        if a.contains([' ', '"']) {
            quote(&a)
        } else {
            a.into_owned()
        }
    };
    args.iter().map(maybe_quote).collect::<Vec<_>>().join(" ")
}

/// `win_openpty` and `w_create_process_pty`: starts `program` with `args`
/// on a new 80×25 pseudo-console, its standard input and output on pipes
/// of their own, and its standard error the console's (`hStdError` is
/// null, which lets ssh built for Cygwin or MSYS2 find its terminal).
#[allow(unsafe_code)]
pub fn spawn(program: &OsStr, args: &[OsString]) -> io::Result<Started> {
    let fail = || io::Error::last_os_error();
    // The console's input and output: separate pipes, as read-write ones
    // deadlock with a pseudo-console.
    let (con_in, input) = pipe()?;
    let (output, con_out) = pipe()?;
    let mut hpc: HPCON = 0;
    let size = COORD { X: 80, Y: 25 };
    // SAFETY: the pipe handles are open; the out-pointer is valid.
    let r = unsafe {
        CreatePseudoConsole(
            size,
            con_in.as_raw_handle(),
            con_out.as_raw_handle(),
            0,
            &mut hpc,
        )
    };
    if r < 0 {
        return Err(io::Error::from_raw_os_error(r));
    }
    let console = Console(hpc);
    drop(con_in);
    // The process's standard input and output.
    let (child_in, stdin) = pipe()?;
    let (stdout, child_out) = pipe()?;
    let (child_in, child_out) = (inheritable(&child_in)?, inheritable(&child_out)?);
    // `SearchPathW`: the program's full name.
    let mut full = vec![0u16; 32_768];
    let ext = wide(OsStr::new(".exe"));
    // SAFETY: the strings are NUL-terminated, the buffer as long as said.
    let n = unsafe {
        SearchPathW(
            null(),
            wide(program).as_ptr(),
            ext.as_ptr(),
            full.len() as u32,
            full.as_mut_ptr(),
            null_mut(),
        )
    };
    if n == 0 || n as usize >= full.len() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{}: {}", program.to_string_lossy(), fail()),
        ));
    }
    // The attributes: the pseudo-console, and the handles to inherit.
    let mut size = 0usize;
    // SAFETY: asking for the size needed, with no list.
    unsafe { InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut size) };
    let mut list_buf = vec![0u8; size];
    let list: LPPROC_THREAD_ATTRIBUTE_LIST = list_buf.as_mut_ptr().cast();
    // SAFETY: the buffer is as long as asked for.
    if unsafe { InitializeProcThreadAttributeList(list, 2, 0, &mut size) } == 0 {
        return Err(fail());
    }
    let inherit = [child_in.as_raw_handle(), child_out.as_raw_handle()];
    // SAFETY: the list is initialised; the values outlive CreateProcessW
    // (the pseudo-console is passed by value, as its handle).
    let attributes = unsafe {
        UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            hpc as *const core::ffi::c_void,
            std::mem::size_of::<HPCON>(),
            null_mut(),
            null(),
        ) != 0
            && UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                inherit.as_ptr().cast(),
                std::mem::size_of_val(&inherit),
                null_mut(),
                null(),
            ) != 0
    };
    let result = (|| {
        if !attributes {
            return Err(fail());
        }
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.hStdInput = child_in.as_raw_handle();
        si.StartupInfo.hStdOutput = child_out.as_raw_handle();
        si.StartupInfo.hStdError = null_mut();
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.lpAttributeList = list;
        let mut all = vec![program.to_owned()];
        all.extend(args.iter().cloned());
        let mut cmdline = wide(OsStr::new(&make_cmdline(&all)));
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: the strings are NUL-terminated (the command line
        // writable), the startup information initialised, and the
        // out-pointer valid.
        let ok = unsafe {
            let flags = GetPriorityClass(GetCurrentProcess()) | EXTENDED_STARTUPINFO_PRESENT;
            CreateProcessW(
                full.as_ptr(),
                cmdline.as_mut_ptr(),
                null(),
                null(),
                TRUE,
                flags,
                null(),
                null(),
                &si.StartupInfo,
                &mut pi,
            )
        };
        if ok == 0 {
            return Err(fail());
        }
        // SAFETY: CreateProcessW opened both, and nothing else owns them.
        let (process, thread) = unsafe {
            (
                OwnedHandle::from_raw_handle(pi.hProcess),
                OwnedHandle::from_raw_handle(pi.hThread),
            )
        };
        drop(thread);
        Ok(Process {
            handle: process,
            id: pi.dwProcessId,
        })
    })();
    // SAFETY: the list was initialised, and isn't used again.
    unsafe { DeleteProcThreadAttributeList(list) };
    let process = result?;
    // The process has its own: the console keeps its output end.
    drop((child_in, child_out, con_out));
    Ok(Started {
        process,
        console,
        output: File::from(output),
        input: File::from(input),
        stdin,
        stdout,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_quoted_as_ocaml_quotes_them() {
        assert_eq!(quote("a b"), "\"a b\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote("a\\\"b"), "\"a\\\\\\\"b\"");
        assert_eq!(quote("a\\b"), "\"a\\b\"");
        assert_eq!(quote("a b\\"), "\"a b\"\\");
        let args = ["ssh", "-e none", "C:\\Program Files\\x", "plain"].map(OsString::from);
        assert_eq!(
            make_cmdline(&args),
            "ssh \"-e none\" \"C:\\Program Files\\x\" plain"
        );
    }
}
