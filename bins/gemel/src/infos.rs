//! The status line: what is going on, on standard output, overwritten as
//! it changes and cleared before anything goes to standard error.
//!
//! Ports `Util.set_infos`, `Util.clear_infos` and `Util.msg` from Unison
//! `src/ubase/util.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! The engine thread writes to standard error through [`Stderr`], the
//! trace's sink, so the line is shared between the threads.

use std::io::{self, Write};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Default)]
struct Line {
    /// What is shown.
    s: String,
    /// What clears it, if not spaces over it.
    clr: String,
}

impl Line {
    /// `clear_infos`.
    fn clear(&self, out: &mut impl Write) {
        if !self.clr.is_empty() {
            let _ = out.write_all(self.clr.as_bytes());
        } else if !self.s.is_empty() {
            let _ = write!(out, "\r{}\r", " ".repeat(self.s.len()));
        }
        let _ = out.flush();
    }

    /// `show_infos`.
    fn show(&self, out: &mut impl Write) {
        if !self.s.is_empty() {
            let _ = out.write_all(self.s.as_bytes());
            let _ = out.flush();
        }
    }
}

/// The status line, shared by the threads that print.
#[derive(Clone, Default)]
pub struct Infos(Arc<Mutex<Line>>);

impl Infos {
    fn line(&self) -> MutexGuard<'_, Line> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `Util.set_infos`: shows `s` instead, cleared with `clr` (or spaces).
    pub fn set(&self, s: &str, clr: &str) {
        let mut line = self.line();
        if line.s != s {
            let mut out = io::stdout().lock();
            line.clear(&mut out);
            *line = Line {
                s: s.to_owned(),
                clr: clr.to_owned(),
            };
            line.show(&mut out);
        }
    }

    /// `Util.msg`: prints `s` on standard error, with the line cleared.
    pub fn msg(&self, s: &str) {
        let line = self.line();
        line.clear(&mut io::stdout().lock());
        let mut err = io::stderr().lock();
        let _ = err.write_all(s.as_bytes());
        let _ = err.flush();
        line.show(&mut io::stdout().lock());
    }

    /// The trace's standard error.
    pub fn stderr(&self) -> Stderr {
        Stderr(self.clone())
    }
}

/// `alwaysDisplay`: prints on standard output (the line is left as it
/// is).
pub fn print(s: &str) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

/// Standard error, as `Util.msg` writes it: clearing the status line
/// first, and showing it again after.
pub struct Stderr(Infos);

impl Write for Stderr {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let line = self.0.line();
        line.clear(&mut io::stdout().lock());
        let mut err = io::stderr().lock();
        err.write_all(buf)?;
        err.flush()?;
        line.show(&mut io::stdout().lock());
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush()
    }
}
