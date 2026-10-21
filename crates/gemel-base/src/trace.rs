//! Debug and log output.
//!
//! Ports Unison `src/ubase/trace.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Three kinds of output:
//!
//! - debugging messages, enabled per module by the `debug` preference
//!   ([`enabled`], [`Trace::debug`]);
//! - user-visible messages and status lines, which a server forwards to the
//!   client ([`Message`] is what crosses the wire);
//! - the log file (`log`, `logfile`).
//!
//! OCaml keeps this state in globals; here it is a [`Trace`] owned by the
//! session, and methods take the [`Prefs`] they read. Not ported yet:
//! closing the log on `SIGUSR1` (for log rotation), which belongs to the
//! binaries.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use gemel_compat::umarshal::{self, Reader, Writer};

use crate::prefs::{Category, Internal, PrefDef, Prefs, Topic};
use crate::util::Error;

/// `debug`: modules to print debugging messages for (`all`, `verbose`, or
/// module names, optionally with `+`).
pub static DEBUG: PrefDef<Vec<String>> = PrefDef::new(
    "debug",
    Category::Expert,
    Vec::new,
    "debug module xxx ('all' -> everything, 'verbose' -> more)",
    "This preference is used to make Unison print various sorts of \
     information about what it is doing internally on the standard \
     error stream.  It can be used many times, each time with the name \
     of a module for which debugging information should be printed.  \
     Possible arguments for \\verb|debug| can be found \
     by looking for calls to \\verb|Util.debug| in the \
     sources (using, e.g., \\verb|grep|).  \
     Setting \\verb|-debug all| causes information from {\\em all} \
     modules to be printed (this mode of usage is the first one to try, \
     if you are trying to understand something that Unison seems to be \
     doing wrong); \\verb|-debug verbose| turns on some additional \
     debugging output from some modules (e.g., it will show exactly \
     what bytes are being sent across the network).",
);

/// `debugtimes`: timestamp debugging messages.
pub static DEBUG_TIMES: PrefDef<bool> = PrefDef::new(
    "debugtimes",
    Category::Internal(Internal::Devel),
    || false,
    "*annotate debugging messages with timestamps",
    "",
);

/// `log`: record actions in the log file.
pub static LOGGING: PrefDef<bool> = PrefDef::new(
    "log",
    Category::Advanced(Topic::General),
    || true,
    "record actions in logfile",
    "When this flag is set, Unison will log all changes to the filesystems\n     on a file.",
);

/// `logfile`: the log file, relative to the Unison directory unless absolute.
pub static LOGFILE: PrefDef<String> = PrefDef::new(
    "logfile",
    Category::Advanced(Topic::General),
    || "unison.log".into(),
    "logfile name",
    "By default, logging messages will be appended to the file\n     \
     \\verb|unison.log| in your .unison directory.  Set this preference if\n     \
     you prefer another file.  It can be a path relative to your .unison directory.\n     \
     Sending SIGUSR1 will close the logfile; the logfile will be re-opened (and\n     \
     created, if needed) automatically, to allow for log rotation.",
);

/// `terse`: suppress status messages.
pub static TERSE: PrefDef<bool> = PrefDef::new(
    "terse",
    Category::Basic(Topic::Syncprocess_CLI),
    || false,
    "suppress status messages",
    "When this preference is set to {\\tt true}, the user \
     interface will not print status messages.",
);

/// `timers`: print timing information.
pub static TIMERS: PrefDef<bool> = PrefDef::new(
    "timers",
    Category::Internal(Internal::Devel),
    || false,
    "*print timing information",
    "",
);

/// Registers this module's preferences.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&DEBUG)?;
    prefs.register(&DEBUG_TIMES)?;
    prefs.register(&LOGGING)?;
    prefs.register(&LOGFILE)?;
    prefs.register(&TERSE)?;
    prefs.register(&TIMERS)
}

/// `Trace.enabled`: whether debugging messages for `modname` are on.
///
/// `""` is on whenever anything is; `verbose` and `all+` turn on everything;
/// `all` turns on every module whose name doesn't end in `+`; `m+` turns on
/// both `m` and `m+`.
pub fn enabled(debugmods: &[String], modname: &str) -> bool {
    let has = |s: &str| debugmods.iter().any(|m| m == s);
    !debugmods.is_empty()
        && (modname.is_empty()
            || has("verbose")
            || has("all+")
            || (has("all") && !modname.ends_with('+'))
            || has(modname)
            || has(&format!("{modname}+")))
}

/// `Trace.enable`: turns debugging for `modname` on or off.
pub fn enable(prefs: &mut Prefs, modname: &str, on: bool) {
    let mut m = prefs.read(&DEBUG).clone();
    if on {
        m.insert(0, modname.to_owned());
    } else {
        // Safelist.remove drops every occurrence.
        m.retain(|x| x != modname);
    }
    prefs.set(&DEBUG, m);
}

/// The kind of a [`Message`] (`msgtype`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// A message for the user.
    Msg,
    /// A new top-level status (the phase).
    StatusMajor,
    /// A new detail status (the file being worked on).
    StatusMinor,
    /// A log entry.
    Log,
    /// A log entry with ANSI colours, stripped in the log file.
    LogColor,
}

/// A message to display where the user is (`Trace.msg`), which a server
/// sends to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// What it is.
    pub kind: MessageKind,
    /// The text.
    pub text: String,
}

impl Message {
    /// `Trace.mmsg`: the kind as a `sum5` tag, then the text.
    pub fn marshal(&self, w: &mut Writer) {
        w.tag(match self.kind {
            MessageKind::Msg => 0,
            MessageKind::StatusMajor => 1,
            MessageKind::StatusMinor => 2,
            MessageKind::Log => 3,
            MessageKind::LogColor => 4,
        });
        w.string(self.text.as_bytes());
    }

    /// `Trace.mmsg`. Messages are display text, so invalid UTF-8 is
    /// replaced.
    pub fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Message> {
        let kind = match r.tag(5)? {
            0 => MessageKind::Msg,
            1 => MessageKind::StatusMajor,
            2 => MessageKind::StatusMinor,
            3 => MessageKind::Log,
            _ => MessageKind::LogColor,
        };
        let text = String::from_utf8_lossy(&r.string()?).into_owned();
        Ok(Message { kind, text })
    }
}

/// Where debugging and log output goes (`Trace.redirect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Printer {
    /// Standard output.
    Stdout,
    /// Standard error (the default).
    #[default]
    Stderr,
    /// Standard output through OCaml's `Format`, which adds a space after
    /// log entries.
    FormatStdout,
}

/// `stripColorEscapes`: removes `ESC [ [0-9;:]* m` sequences.
pub fn strip_color_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("\x1b[") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let n = after
            .bytes()
            .take_while(|c| c.is_ascii_digit() || *c == b';' || *c == b':')
            .count();
        if after.as_bytes().get(n) == Some(&b'm') {
            rest = &after[n + 1..];
        } else {
            out.push_str("\x1b[");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// `Util.padto`.
fn pad_to(n: usize, s: &str) -> String {
    format!("{s:<n$}")
}

/// Displays a message line; `None` means Unison's default.
pub type Displayer = Box<dyn FnMut(&str) + Send>;
/// Sends a message to the client (set on a server).
pub type Forwarder = Box<dyn FnMut(Message) + Send>;
/// Combines the major and minor status into a line.
pub type StatusFormatter = Box<dyn Fn(&str, &str) -> String + Send>;

/// Tracing state (`Trace`'s globals).
pub struct Trace {
    printer: Printer,
    /// `Trace.runningasserver`: tags debugging messages with `server:`.
    pub running_as_server: bool,
    /// `Trace.sendLogMsgsToStderr`: also print log entries.
    pub send_log_msgs_to_stderr: bool,
    displayer: Option<Displayer>,
    status_formatter: Option<StatusFormatter>,
    forwarder: Option<Forwarder>,
    status_major: String,
    status_minor: String,
    unison_dir: Option<PathBuf>,
    log: Option<(File, String)>,
    stdout: Box<dyn Write + Send>,
    stderr: Box<dyn Write + Send>,
}

impl Default for Trace {
    fn default() -> Self {
        Self::new()
    }
}

impl Trace {
    /// Output to the process's stdout and stderr.
    pub fn new() -> Self {
        Self::with_sinks(Box::new(io::stdout()), Box::new(io::stderr()))
    }

    /// Output to the given sinks instead of stdout and stderr.
    pub fn with_sinks(stdout: Box<dyn Write + Send>, stderr: Box<dyn Write + Send>) -> Self {
        Trace {
            printer: Printer::default(),
            running_as_server: false,
            send_log_msgs_to_stderr: true,
            displayer: None,
            status_formatter: None,
            forwarder: None,
            status_major: String::new(),
            status_minor: String::new(),
            unison_dir: None,
            log: None,
            stdout,
            stderr,
        }
    }

    /// `Trace.redirect`.
    pub fn redirect(&mut self, printer: Printer) {
        self.printer = printer;
    }

    /// `Trace.messageDisplayer`: installed by a user interface.
    pub fn set_displayer(&mut self, displayer: Option<Displayer>) {
        self.displayer = displayer;
    }

    /// `Trace.statusFormatter`; the default joins with a space.
    pub fn set_status_formatter(&mut self, formatter: Option<StatusFormatter>) {
        self.status_formatter = formatter;
    }

    /// `Trace.messageForwarder`: set on a server to send messages to the client.
    pub fn set_forwarder(&mut self, forwarder: Option<Forwarder>) {
        self.forwarder = forwarder;
    }

    /// The directory a relative `logfile` is resolved against.
    pub fn set_unison_dir(&mut self, dir: impl Into<PathBuf>) {
        self.unison_dir = Some(dir.into());
    }

    fn print(&mut self, s: &str) {
        // Output errors are ignored, as OCaml ignores them.
        let _ = match self.printer {
            Printer::Stdout | Printer::FormatStdout => self.stdout.write_all(s.as_bytes()),
            Printer::Stderr => self.stderr.write_all(s.as_bytes()),
        };
    }

    /// `Util.msg`: prints `s` to standard error.
    pub fn msg(&mut self, s: &str) {
        let _ = self.stderr.write_all(s.as_bytes());
        let _ = self.stderr.flush();
    }

    /// `Trace.debug`: prints `msg()` to stderr if debugging for `modname`
    /// is on, after a `[time: server: module]` prefix. The timestamp is UTC
    /// (OCaml uses local time).
    pub fn debug(&mut self, prefs: &Prefs, modname: &str, msg: impl FnOnce() -> String) {
        if !enabled(prefs.read(&DEBUG), modname) {
            return;
        }
        let server = if self.running_as_server {
            "server: "
        } else {
            ""
        };
        let time = if *prefs.read(&DEBUG_TIMES) {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            format!(
                "{:02}:{:02}:{:02}",
                secs / 3600 % 24,
                secs / 60 % 60,
                secs % 60
            )
        } else {
            String::new()
        };
        if !time.is_empty() || !server.is_empty() || !modname.is_empty() {
            let time = if time.is_empty() || (server.is_empty() && modname.is_empty()) {
                time
            } else {
                time + ": "
            };
            self.print(&format!("[{time}{server}{modname}] "));
        }
        let m = msg();
        let _ = self.stderr.write_all(m.as_bytes());
        let _ = self.stderr.flush();
    }

    fn log_file(&self, name: &str) -> PathBuf {
        let p = Path::new(name);
        match &self.unison_dir {
            Some(dir) if p.is_relative() => dir.join(p),
            _ => p.to_path_buf(),
        }
    }

    /// `getLogch`: the open log file, reopened if `logfile` has changed.
    fn log_channel(&mut self, prefs: &Prefs) -> Result<&mut File, Error> {
        let name = prefs.read(&LOGFILE);
        if self.log.as_ref().is_some_and(|(_, n)| n != name) {
            self.log = None;
        }
        if self.log.is_none() {
            let path = self.log_file(name);
            let mut opts = OpenOptions::new();
            opts.append(true).create(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
            let f = opts.open(&path).map_err(|e| {
                Error::Fatal(format!("Error in getLogch:\n{e} ({})", path.display()))
            })?;
            self.log = Some((f, name.clone()));
        }
        Ok(&mut self.log.as_mut().expect("just opened").0)
    }

    /// `writeLog`.
    fn write_log(&mut self, prefs: &Prefs, s: &str, strip_color: bool) -> Result<(), Error> {
        let shown = if self.printer == Printer::FormatStdout {
            format!("{s} ")
        } else {
            s.to_owned()
        };
        if self.send_log_msgs_to_stderr {
            self.print(&shown);
        } else if enabled(prefs.read(&DEBUG), "") {
            self.debug(prefs, "", String::new);
            self.print(&shown);
        }
        if *prefs.read(&LOGGING) {
            let clean = if strip_color {
                strip_color_escapes(s)
            } else {
                s.to_owned()
            };
            let f = self.log_channel(prefs)?;
            let _ = f.write_all(clean.as_bytes()).and_then(|()| f.flush());
        }
        Ok(())
    }

    fn display(&mut self, prefs: &Prefs, s: &str) {
        if let Some(d) = self.displayer.as_mut() {
            d(s);
            return;
        }
        // defaultMessageDisplayer
        if *prefs.read(&TERSE) {
            return;
        }
        let line = if s.is_empty() {
            String::new()
        } else {
            format!("{s}\n")
        };
        if enabled(prefs.read(&DEBUG), "") {
            self.debug(prefs, "", || line);
        } else if !self.running_as_server {
            let _ = self.stderr.write_all(line.as_bytes());
        }
    }

    /// `Trace.displayMessageLocally`.
    pub fn display_message_locally(&mut self, prefs: &Prefs, m: Message) -> Result<(), Error> {
        let status = |t: &Self| match &t.status_formatter {
            Some(f) => f(&t.status_major, &t.status_minor),
            None => format!("{} {}", t.status_major, t.status_minor),
        };
        match m.kind {
            MessageKind::Msg => self.display(prefs, &m.text),
            MessageKind::StatusMajor => {
                self.status_major = m.text;
                self.status_minor.clear();
                let s = status(self);
                self.display(prefs, &s);
            }
            MessageKind::StatusMinor => {
                self.status_minor = m.text;
                let s = status(self);
                self.display(prefs, &s);
            }
            MessageKind::Log => self.write_log(prefs, &m.text, false)?,
            MessageKind::LogColor => self.write_log(prefs, &m.text, true)?,
        }
        Ok(())
    }

    /// `displayMessage`: forwards the message if a forwarder is set.
    pub fn display_message(&mut self, prefs: &Prefs, m: Message) -> Result<(), Error> {
        match self.forwarder.as_mut() {
            Some(f) => {
                f(m);
                Ok(())
            }
            None => self.display_message_locally(prefs, m),
        }
    }

    fn send(
        &mut self,
        prefs: &Prefs,
        kind: MessageKind,
        text: impl Into<String>,
    ) -> Result<(), Error> {
        self.display_message(
            prefs,
            Message {
                kind,
                text: text.into(),
            },
        )
    }

    /// `Trace.message`: shows a message to the user.
    pub fn message(&mut self, prefs: &Prefs, s: &str) -> Result<(), Error> {
        self.send(prefs, MessageKind::Msg, s)
    }

    /// `Trace.status`: a change of phase.
    pub fn status(&mut self, prefs: &Prefs, s: &str) -> Result<(), Error> {
        self.send(prefs, MessageKind::StatusMajor, s)
    }

    /// `Trace.statusMinor`: a change of detail.
    pub fn status_minor(&mut self, prefs: &Prefs, s: &str) -> Result<(), Error> {
        self.send(prefs, MessageKind::StatusMinor, s)
    }

    /// `Trace.statusDetail`: like [`Trace::status_minor`], tagged on a server.
    pub fn status_detail(&mut self, prefs: &Prefs, s: &str) -> Result<(), Error> {
        let s = if self.running_as_server {
            pad_to(30, s) + " [server]"
        } else {
            s.to_owned()
        };
        self.send(prefs, MessageKind::StatusMinor, s)
    }

    /// `Trace.log`: a log entry (include the newline).
    pub fn log(&mut self, prefs: &Prefs, s: &str) -> Result<(), Error> {
        self.send(prefs, MessageKind::Log, s)
    }

    /// `Trace.log_color`: a log entry whose ANSI colours are stripped in the
    /// log file.
    pub fn log_color(&mut self, prefs: &Prefs, s: &str) -> Result<(), Error> {
        self.send(prefs, MessageKind::LogColor, s)
    }

    /// `Trace.logonly`: a log entry that isn't also printed.
    pub fn log_only(&mut self, prefs: &Prefs, s: &str) -> Result<(), Error> {
        let saved = std::mem::replace(&mut self.send_log_msgs_to_stderr, false);
        let r = self.log(prefs, s);
        self.send_log_msgs_to_stderr = saved;
        r
    }

    /// `Trace.logverbose`: a log entry printed only when not `terse`.
    pub fn log_verbose(&mut self, prefs: &Prefs, s: &str) -> Result<(), Error> {
        let saved = self.send_log_msgs_to_stderr;
        self.send_log_msgs_to_stderr = saved && !*prefs.read(&TERSE);
        let r = self.log(prefs, s);
        self.send_log_msgs_to_stderr = saved;
        r
    }

    /// `Trace.startTimer`: announces `desc` and starts timing it, if
    /// `timers` is set.
    pub fn start_timer(&mut self, prefs: &Prefs, desc: &str) -> Result<Timer, Error> {
        if *prefs.read(&TIMERS) {
            self.message(prefs, &format!("{desc}..."))?;
        }
        Ok(Self::start_timer_quietly(prefs, desc))
    }

    /// `Trace.startTimerQuietly`.
    pub fn start_timer_quietly(prefs: &Prefs, desc: &str) -> Timer {
        Timer {
            desc: desc.to_owned(),
            start: prefs.read(&TIMERS).then(Instant::now),
        }
    }

    /// `Trace.showTimer`: shows the elapsed time (not on a server, where it
    /// would confuse the RPC stream).
    pub fn show_timer(&mut self, prefs: &Prefs, timer: &Timer) -> Result<(), Error> {
        match timer.start {
            Some(start) if !self.running_as_server && *prefs.read(&TIMERS) => self.message(
                prefs,
                &format!(
                    "{} ({:.2} seconds)",
                    timer.desc,
                    start.elapsed().as_secs_f64()
                ),
            ),
            _ => Ok(()),
        }
    }
}

/// A running timer (`Trace.timer`).
#[derive(Debug, Clone)]
pub struct Timer {
    desc: String,
    start: Option<Instant>,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    fn setup() -> (Prefs, Trace, Sink, Sink) {
        let mut prefs = Prefs::new();
        register_prefs(&mut prefs).unwrap();
        prefs.set(&LOGGING, false);
        let (out, err) = (Sink::default(), Sink::default());
        let trace = Trace::with_sinks(Box::new(out.clone()), Box::new(err.clone()));
        (prefs, trace, out, err)
    }

    #[test]
    fn debug_flags_follow_trace_ml() {
        let m = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(!enabled(&[], ""));
        assert!(enabled(&m(&["x"]), ""));
        assert!(enabled(&m(&["all"]), "update"));
        assert!(!enabled(&m(&["all"]), "remote+"));
        assert!(enabled(&m(&["all+"]), "remote+"));
        assert!(enabled(&m(&["verbose"]), "remote+"));
        assert!(enabled(&m(&["remote+"]), "remote"));
        assert!(!enabled(&m(&["remote"]), "remote+"));
    }

    #[test]
    fn debug_output_has_ocamls_prefix() {
        let (mut prefs, mut trace, _, err) = setup();
        enable(&mut prefs, "update", true);
        trace.debug(&prefs, "update", || "hello\n".into());
        trace.debug(&prefs, "other", || "hidden\n".into());
        trace.running_as_server = true;
        trace.debug(&prefs, "update", || "again\n".into());
        assert_eq!(err.text(), "[update] hello\n[server: update] again\n");
    }

    #[test]
    fn status_lines_and_forwarding() {
        let (prefs, mut trace, _, err) = setup();
        trace.status(&prefs, "Looking for changes").unwrap();
        trace.status_minor(&prefs, "a/b").unwrap();
        trace.message(&prefs, "done").unwrap();
        assert_eq!(
            err.text(),
            "Looking for changes \nLooking for changes a/b\ndone\n"
        );

        let sent = Arc::new(Mutex::new(Vec::new()));
        let s = sent.clone();
        trace.set_forwarder(Some(Box::new(move |m| s.lock().unwrap().push(m))));
        trace.running_as_server = true;
        trace.status_detail(&prefs, "x").unwrap();
        assert_eq!(
            sent.lock().unwrap()[0],
            Message {
                kind: MessageKind::StatusMinor,
                text: format!("{:<30} [server]", "x")
            }
        );
    }

    #[test]
    fn terse_hides_messages() {
        let (mut prefs, mut trace, _, err) = setup();
        prefs.set(&TERSE, true);
        trace.message(&prefs, "hidden").unwrap();
        assert_eq!(err.text(), "");
    }

    #[test]
    fn log_file_strips_colours() {
        let (mut prefs, mut trace, _, err) = setup();
        let dir = std::env::temp_dir().join(format!("gemel-trace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        trace.set_unison_dir(&dir);
        prefs.set(&LOGGING, true);
        trace
            .log_color(&prefs, "\x1b[1;31mred\x1b[0m plain\n")
            .unwrap();
        trace.log_only(&prefs, "quiet\n").unwrap();
        let logged = std::fs::read_to_string(dir.join("unison.log")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(logged, "red plain\nquiet\n");
        assert_eq!(err.text(), "\x1b[1;31mred\x1b[0m plain\n");
    }

    #[test]
    fn message_codec() {
        let m = Message {
            kind: MessageKind::LogColor,
            text: "hi".into(),
        };
        let bytes = umarshal::to_bytes(|w| m.marshal(w));
        assert_eq!(&bytes[8..], [4, 1, 2, b'h', b'i']);
        assert_eq!(umarshal::from_bytes(&bytes, Message::unmarshal), Ok(m));
        assert_eq!(strip_color_escapes("a\x1b[b\x1b[;:2m"), "a\x1b[b");
    }
}
