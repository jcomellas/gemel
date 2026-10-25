//! The text user interface.
//!
//! Ports Unison `src/uitext.ml`: its startup (`start` and `start2`), one
//! sync and its retries (`synchronizeOnce`, `synchronizeUntilNoFailures`),
//! going through the items ([`interact`]), the propagation with its
//! progress line and summary (`interactAndPropagateChanges`,
//! `doTransport`), the questions (warnings, dangerous deletions, merges),
//! the terminal's raw mode, interrupts, and `handleException`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! It drives the engine through `gemel_ui_api::session`. While a step
//! runs, only the engine writes to stderr (log entries, statuses, the
//! wait message), through the trace, and only this thread writes to
//! stdout; so this thread's own messages to stderr come between steps.
//! This thread waits on one channel ([`Input`]) for the session's events,
//! the keys typed and the signals caught. Events that come while the user
//! is asked something wait until the answer, as OCaml's whole program
//! waits for it.
//!
//! Interrupts: Ctrl-C stops the program (`Sys.catch_break`), except
//! during a sync, where it and `SIGTERM` abort the sync, and a fourth
//! stops the program. Stopping interrupts the session's step, which ends
//! at its next wait (work handed to other threads, such as a directory
//! being scanned, finishes first), and the session's end releases the
//! locks held.
//!
//! `-repeat` syncs again after an interval, or (`watch`) whenever the file
//! watcher reports changes, until the user asks to stop between syncs
//! (`SIGUSR2`).
//! The profile manager (`-i`) lists the profiles to choose one.

mod interact;

/// `watchinterval`: the least time between two syncs in watch mode, in
/// seconds.
const WATCH_INTERVAL: f64 = 1.;
/// `retrydelay`: the least delay before a path that failed is retried
/// again.
const RETRY_DELAY: f64 = 5.;
/// `maxdelay`: the most it is delayed, and the longest wait with a
/// timeout.
const MAX_DELAY: f64 = 30. * 60.;

/// The paths left unsynchronised, and those whose retry is delayed
/// (`Update.findUpdates`'s `subpaths`).
type Subpaths = Option<(Vec<Path<Global>>, Vec<Path<Global>>)>;

/// The time, in seconds since the epoch (`Unix.gettimeofday`).
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0., |d| d.as_secs_f64())
}

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gemel_base::path::{Global, Path};
use gemel_base::prefs::BoolWithDefault;
use gemel_base::trace::Trace;
use gemel_base::util::{self, MY_NAME};
use gemel_fs::signal::{self, Signal};
use gemel_fs::system;
use gemel_fs::terminal::{self, Output, TerminalState};
use gemel_rpc::runtime::EngineThread;
use gemel_ui_api::cli::Interface;
use gemel_ui_api::exit_code::ExitCode;
use gemel_ui_api::session::{
    self, Command, Config, Event, ItemView, LogKind, Outcome, Overrides, Progress, Question,
    RootNames, Settings, Summary, Updates,
};
use gemel_ui_api::uicommon::startup::ProfileInfo;
use gemel_ui_api::uicommon::{self, Action, Repeat};

use crate::infos::{self, Infos};
use crate::input::{self, Input, Read};
use interact::{Choice, Entry, Proceed};

/// `profmgrUsageMsg`.
fn profmgr_usage_msg() -> String {
    format!("To start interactive profile selection, type \"{MY_NAME} -i\".")
}

/// Why a step stopped.
enum Stop {
    /// It failed, with this message (for `handleException`).
    Error(String),
    /// The program exits with this code.
    Exit(i32),
    /// `Sys.Break`: the user stopped it.
    Break,
}

/// Why no key was read.
enum NoKey {
    /// A signal came during a raw read (`EINTR`): the question is asked
    /// again.
    Interrupted,
    /// The end of the input.
    Eof,
    /// The read failed.
    Os(io::Error),
    /// The program stops.
    Stop(Stop),
}

/// The colours (`color`).
#[derive(Clone, Copy)]
enum Color {
    Reset,
    Focus,
    Success,
    Information,
    Failure,
    AError,
    ASkip,
    ALtoRf,
    ALtoRt,
    ARtoLf,
    ARtoLt,
    AMerge,
    DiffAdd,
    DiffDel,
    DiffLoc,
}

/// How `interactAndPropagateChanges` went: whether any items were
/// skipped, partly transferred, failed or not started, and those that
/// failed.
#[derive(Default)]
struct Propagated {
    skipped: bool,
    partial: bool,
    failures: bool,
    not_started: bool,
    failed: Vec<Path<Global>>,
}

/// The progress line's state (`doTransport`'s).
struct ProgressLine {
    shown: f64,
    items: usize,
    logged: f64,
}

/// The terminal in raw mode (`cbreakMode`), shared with the thread that
/// suspends and resumes the program.
type Cbreak = Arc<Mutex<Option<Arc<TerminalState>>>>;

fn cbreak_state(c: &Cbreak) -> Option<Arc<TerminalState>> {
    c.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

struct Text {
    session: EngineThread<Command, Event>,
    inputs: mpsc::Receiver<Input>,
    /// Events that came while the user was being asked something.
    deferred: VecDeque<Event>,
    /// Where to ask for a read.
    reads: mpsc::Sender<Read>,
    /// A read was asked for, in raw mode or not, and hasn't come.
    reading: Option<bool>,
    /// A read that came while no key was wanted.
    ready: Option<(bool, io::Result<Option<Vec<u8>>>)>,
    /// What is left of the last read (`inputBuffer`).
    buffer: Vec<u8>,
    cbreak: Cbreak,
    infos: Infos,
    args: Vec<String>,
    settings: Option<Settings>,
    roots: Option<RootNames>,
    /// `colorEnabled`.
    colors: bool,
    /// The warnings after the roots are connected are shown as `start2`
    /// shows them; before, as at initialisation.
    running: bool,
    /// The user was asked for roots (`promptForRoots`).
    asked_roots: bool,
    /// During a sync: the failures and partial transfers as they come.
    failed: Vec<Path<Global>>,
    partial: Vec<Path<Global>>,
    progress: Option<ProgressLine>,
    /// A sync is running, and Ctrl-C and `SIGTERM` abort it.
    in_sync: bool,
    /// The interrupts during syncs (`intrcount`).
    interrupts: usize,
    /// `dumbtty` and `color` from the command line, before a profile is
    /// loaded (the profile manager's).
    cl_dumbtty: bool,
    cl_color: BoolWithDefault,
    /// The user asked to stop between syncs (`safeStopReqd`).
    safe_stop: bool,
    /// The session was told to interrupt its step.
    interrupted: bool,
    /// And it has.
    stopped: bool,
}

/// `formatStatus`: a status, with its detail on the next line, or the
/// detail alone under the same status.
fn format_status() -> gemel_base::trace::StatusFormatter {
    let last = Mutex::new(String::new());
    Box::new(move |major: &str, minor: &str| {
        let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
        let s = if major == *last {
            format!("  {minor}")
        } else if minor.is_empty() {
            major.to_owned()
        } else {
            format!("{major}\n  {minor}")
        };
        *last = major.to_owned();
        s
    })
}

/// The time, as the summary shows it.
fn hms() -> String {
    match system::localtime(system::gettimeofday()) {
        Some(t) => format!("{:02}:{:02}:{:02}", t.hour, t.min, t.sec),
        None => String::new(),
    }
}

/// `Sys.catch_break true`, and `suspend` and `resume` in raw mode: the
/// signals for this thread go to `inputs`.
fn catch_signals(inputs: mpsc::Sender<Input>, cbreak: Cbreak) {
    let listening = signal::listen(move |s| match s {
        // `suspend`.
        Signal::Suspend => {
            if let Some(t) = cbreak_state(&cbreak) {
                let _ = t.restore();
            }
            let _ = signal::set_default(Signal::Suspend);
            signal::raise(Signal::Suspend);
        }
        // `resume`.
        Signal::Resume => {
            let _ = signal::catch(Signal::Suspend);
            if let Some(t) = cbreak_state(&cbreak) {
                let _ = t.raw();
            }
        }
        s => {
            let _ = inputs.send(Input::Signal(s));
        }
    });
    if listening.is_ok() {
        let _ = signal::catch(Signal::Interrupt);
        // `setupSafeStop`.
        if signal::supported() {
            let _ = signal::catch(Signal::SafeStop);
        }
    }
}

/// `Uitext.start`: starts a session, syncs once (with retries), and
/// returns the exit code.
pub fn start(args: Vec<String>, interface: Interface) -> i32 {
    let infos = Infos::default();
    let (tx, inputs) = mpsc::channel();
    let cbreak: Cbreak = Arc::default();
    catch_signals(tx.clone(), cbreak.clone());
    if interface != Interface::Text {
        infos.msg(&format!(
            "This {MY_NAME} binary only provides the text GUI...\n"
        ));
    }
    let mut trace = Trace::with_sinks(Box::new(std::io::stdout()), Box::new(infos.stderr()));
    trace.set_status_formatter(Some(format_status()));
    let started = session::spawn(Config {
        unison_dir: gemel_fs::os::unison_dir(),
        args: args.clone(),
        trace,
        loopback: false,
        status_events: false,
        scan_status: true,
        log_warnings: false,
        print_wait_message: true,
        complete_progress: false,
        traffic: None,
    })
    .and_then(|mut s| {
        let events = tx.clone();
        s.forward_events(move |e| {
            let e = e.map_or(Input::Ended, |e| Input::Event(Box::new(e)));
            events.send(e).is_ok()
        })?;
        Ok((s, input::spawn_reader(tx)?))
    });
    let (session, reads) = match started {
        Ok(s) => s,
        Err(e) => {
            infos.msg(&format!("Error: {e}\n"));
            return ExitCode::Fatal as i32;
        }
    };
    // `dumbtty` and `color` as the command line sets them.
    let (cl_dumbtty, cl_color) = gemel_ui_api::new_prefs()
        .ok()
        .and_then(|mut p| {
            p.parse_cmd_line(&args).ok()?;
            Some((
                *p.read(&gemel_ui_api::cli::DUMBTTY),
                *p.read(&gemel_ui_api::cli::COLOR),
            ))
        })
        .unwrap_or_default();
    let mut ui = Text {
        session,
        inputs,
        deferred: VecDeque::new(),
        reads,
        reading: None,
        ready: None,
        buffer: Vec::new(),
        cbreak,
        infos,
        args,
        settings: None,
        roots: None,
        colors: false,
        running: false,
        asked_roots: false,
        failed: Vec::new(),
        partial: Vec::new(),
        progress: None,
        in_sync: false,
        interrupts: 0,
        cl_dumbtty,
        cl_color,
        safe_stop: false,
        interrupted: false,
        stopped: false,
    };
    let code = match ui.run() {
        Ok(code) | Err(Stop::Exit(code)) => code,
        Err(Stop::Error(e)) => {
            ui.stop_session();
            ui.handle_exception(Some(&e));
            ExitCode::Fatal as i32
        }
        Err(Stop::Break) => {
            ui.stop_session();
            ui.handle_exception(None);
            ExitCode::Fatal as i32
        }
    };
    // Put the terminal back in "sane" mode, if necessary.
    ui.restore_terminal();
    // So that what was asked to be logged is.
    let Text {
        session, inputs, ..
    } = ui;
    drop(inputs);
    let _ = session.join();
    code
}

impl Text {
    fn settings(&self) -> &Settings {
        self.settings.as_ref().expect("a profile is loaded")
    }

    fn batch(&self) -> bool {
        self.settings.as_ref().is_some_and(|s| s.batch)
    }

    fn silent(&self) -> bool {
        self.settings.as_ref().is_some_and(|s| s.silent)
    }

    fn terse(&self) -> bool {
        self.settings.as_ref().is_some_and(|s| s.terse)
    }

    fn dumbtty(&self) -> bool {
        self.settings
            .as_ref()
            .map_or(self.cl_dumbtty, |s| s.dumbtty)
    }

    /// The progress line and the scanned paths are shown only without
    /// `terse` or debugging.
    fn show_infos(&self) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|s| !s.terse && s.debug.is_empty())
    }

    /// `display`: unless `silent`.
    fn display(&self, s: &str) {
        if !self.silent() {
            infos::print(s);
        }
    }

    /// `displayWhenInteractive`: unless in batch mode.
    fn display_when_interactive(&self, s: &str) {
        if !self.batch() {
            infos::print(s);
        }
    }

    fn raw(&self) -> bool {
        cbreak_state(&self.cbreak).is_some()
    }

    /// `newLine`: in raw mode; in a dumb terminal the user has typed one.
    fn new_line(&self) {
        if self.raw() {
            self.display("\n");
        }
    }

    /// `overwrite`.
    fn overwrite(&self) {
        if self.raw() {
            self.display("\r");
        }
    }

    fn color(&self, c: Color) -> &'static str {
        if !self.colors {
            return "";
        }
        match c {
            Color::Reset => "\x1b[0m",
            Color::Focus => "\x1b[1m",
            Color::Success | Color::ALtoRf => "\x1b[1;32m",
            Color::Information | Color::ARtoLf => "\x1b[1;34m",
            Color::Failure => "\x1b[1;31m",
            Color::AError | Color::DiffDel => "\x1b[31m",
            Color::ASkip => "\x1b[1;35m",
            Color::ALtoRt | Color::ARtoLt => "\x1b[1;33m",
            Color::AMerge => "\x1b[1;36m",
            Color::DiffAdd => "\x1b[32m",
            Color::DiffLoc => "\x1b[36m",
        }
    }

    /// `setColorPreference`.
    fn set_color_preference(&mut self) {
        let color = self.settings.as_ref().map_or(self.cl_color, |s| s.color);
        let env_ok = std::env::var_os("NO_COLOR").is_none();
        let term_ok = std::env::var_os("TERM").is_none_or(|t| t != "dumb");
        let tty_ok = io::stdout().is_terminal() && io::stderr().is_terminal();
        let ok = env_ok && term_ok && tty_ok && !self.dumbtty();
        self.colors = match color {
            BoolWithDefault::True => true,
            BoolWithDefault::False => false,
            BoolWithDefault::Default => {
                ok && (!cfg!(windows)
                    || (terminal::vt_capable(Output::Stdout)
                        && terminal::vt_capable(Output::Stderr)))
            }
        };
    }

    /// `setupTerminal`: raw mode, unless `dumbtty` or stdin isn't a
    /// terminal.
    fn setup_terminal(&mut self) {
        if self.dumbtty() {
            return;
        }
        let Ok(t) = TerminalState::new() else {
            return self.restore_terminal();
        };
        let t = Arc::new(t);
        *self.cbreak.lock().unwrap_or_else(|e| e.into_inner()) = Some(t.clone());
        if signal::supported() {
            let _ = signal::catch(Signal::Resume);
            let _ = signal::catch(Signal::Suspend);
        }
        if t.raw().is_err() {
            self.restore_terminal();
        }
    }

    /// `restoreTerminal`.
    fn restore_terminal(&mut self) {
        if signal::supported() && !self.dumbtty() {
            let _ = signal::set_default(Signal::Resume);
        }
        let t = self.cbreak.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(t) = t {
            let _ = t.restore();
        }
    }

    fn send(&self, c: Command) {
        self.session.send(c);
    }

    fn log(&self, kind: LogKind, s: String) {
        self.send(Command::Log(kind, s));
    }

    fn status(&self, s: &str) {
        self.log(LogKind::Status, s.to_owned());
    }

    /// `Sys.Break`, or an error here: tells the session to stop its step
    /// at once (before a question's answer is dropped, which would answer
    /// it).
    fn brk(&mut self) -> Stop {
        if !self.interrupted {
            self.send(Command::Interrupt);
            self.interrupted = true;
        }
        Stop::Break
    }

    /// Has the session stop its step, and waits until it has: the
    /// progress of a sync goes on being shown meanwhile.
    fn stop_session(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        if !self.interrupted {
            self.brk();
        }
        loop {
            let e = match self.deferred.pop_front() {
                Some(e) => e,
                None => match self.inputs.recv() {
                    Ok(Input::Event(e)) => *e,
                    Ok(Input::Ended) | Err(_) => return,
                    Ok(_) => continue,
                },
            };
            match e {
                Event::Interrupted | Event::Exit(_) => return,
                Event::Progress(p) => self.show_progress(&p),
                _ => {}
            }
        }
    }

    /// A signal: during a sync, Ctrl-C and `SIGTERM` abort it
    /// (`sigtermHandler` and `ctrlCHandler`) until the fourth; otherwise
    /// Ctrl-C stops the program.
    fn on_signal(&mut self, s: Signal) -> Result<(), Stop> {
        match s {
            Signal::Interrupt | Signal::Terminate if self.in_sync => {
                if self.interrupts >= 3 {
                    return Err(self.brk());
                }
                self.send(Command::Abort);
                self.interrupts += 1;
                if s == Signal::Interrupt && self.interrupts == 1 {
                    let mut out = io::stdout().lock();
                    let _ = out.write_all(
                        b"\n\nUpdate propagation interrupted. It may take a while to stop.\n\
                          If the process doesn't stop soon then wait or press Ctrl-C\n\
                          3 more times to force immediate termination.\n\n\n",
                    );
                    let _ = out.flush();
                }
                Ok(())
            }
            Signal::Interrupt => Err(self.brk()),
            // `safeStop`.
            Signal::SafeStop => {
                self.safe_stop = true;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// `interruptibleSleep`: waits `secs` seconds, or until the user asks
    /// to stop between syncs.
    fn interruptible_sleep(&mut self, secs: f64) -> Result<(), Stop> {
        let end = Instant::now() + Duration::from_secs_f64(secs.max(0.));
        while !self.safe_stop {
            let Some(left) = end.checked_duration_since(Instant::now()) else {
                return Ok(());
            };
            match self.inputs.recv_timeout(left) {
                Ok(Input::Signal(s)) => self.on_signal(s)?,
                Ok(Input::Event(e)) => self.deferred.push_back(*e),
                Ok(Input::Key(k)) => self.ready = self.reading.take().map(|raw| (raw, k)),
                Ok(Input::Ended) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Stop::Error("The session stopped unexpectedly".to_owned()));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(()),
            }
        }
        Ok(())
    }

    /// `waitForChanges`: waits until the file watcher reports a change to
    /// a replica, until `t` (unless that is more than [`MAX_DELAY`] away),
    /// or until the user asks to stop between syncs.
    fn wait_for_changes(&mut self, t: f64) -> Result<(), Stop> {
        let dt = t - now();
        if dt <= 0. {
            return Ok(());
        }
        let end = (dt <= MAX_DELAY).then(|| Instant::now() + Duration::from_secs_f64(dt));
        self.send(Command::WaitForChanges);
        loop {
            let left = end.map(|e| e.saturating_duration_since(Instant::now()));
            if self.safe_stop || left == Some(Duration::ZERO) {
                break;
            }
            let input = match self.deferred.pop_front() {
                Some(e) => Ok(Input::Event(Box::new(e))),
                None => match left {
                    Some(left) => self.inputs.recv_timeout(left),
                    None => self
                        .inputs
                        .recv()
                        .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
                },
            };
            match input {
                Ok(Input::Event(e)) => match *e {
                    Event::Changed => return Ok(()),
                    Event::Error(e) => return Err(Stop::Error(e)),
                    Event::Exit(code) => return Err(Stop::Exit(code as i32)),
                    Event::Warning(w) => self.warn(&w),
                    _ => {}
                },
                Ok(Input::Signal(s)) => self.on_signal(s)?,
                Ok(Input::Key(k)) => self.ready = self.reading.take().map(|raw| (raw, k)),
                Ok(Input::Ended) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Stop::Error("The session stopped unexpectedly".to_owned()));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        // Time is up, or the user asked to stop: the wait ends, with
        // `Interrupted` (or `Changed` first, if it came meanwhile).
        match self.call(Command::Interrupt)? {
            Event::Changed => match self.call_reply()? {
                Event::Interrupted => Ok(()),
                e => Err(Stop::Error(format!("unexpected {e:?}"))),
            },
            Event::Interrupted => Ok(()),
            e => Err(Stop::Error(format!("unexpected {e:?}"))),
        }
    }

    /// Sends `c` and handles the events up to its reply.
    fn call(&mut self, c: Command) -> Result<Event, Stop> {
        let sync = matches!(c, Command::Synchronize);
        self.send(c);
        self.reply(sync)
    }

    /// Handles the events up to the next reply.
    fn call_reply(&mut self) -> Result<Event, Stop> {
        self.reply(false)
    }

    fn reply(&mut self, sync: bool) -> Result<Event, Stop> {
        loop {
            let e = match self.deferred.pop_front() {
                Some(e) => e,
                None => match self.inputs.recv() {
                    Ok(Input::Event(e)) => *e,
                    Ok(Input::Signal(s)) => {
                        self.on_signal(s)?;
                        continue;
                    }
                    Ok(Input::Key(k)) => {
                        self.ready = self.reading.take().map(|raw| (raw, k));
                        continue;
                    }
                    Ok(Input::Ended) | Err(_) => {
                        return Err(Stop::Error("The session stopped unexpectedly".to_owned()));
                    }
                },
            };
            match e {
                Event::Error(e) => return Err(Stop::Error(e)),
                Event::Exit(code) => return Err(Stop::Exit(code as i32)),
                Event::Warning(w) => self.warn(&w),
                Event::Ask(q) => self.answer(q)?,
                Event::Scanning(p) => self.show_scanning(&p),
                Event::Progress(p) => self.show_progress(&p),
                Event::Item(_, v) if sync => self.item_done(&v),
                Event::Merge { text, .. } => self.show_merge(&text),
                Event::Status(_) | Event::Contacting(_) | Event::Refused(_) => {}
                e => return Ok(e),
            }
        }
    }

    /// `readInput`: a line, or in raw mode what one read gives. Events
    /// that come meanwhile wait.
    ///
    /// A signal is handled at once. It interrupts a raw read
    /// (`Unix.read`, `EINTR`), and the question is asked again; a line is
    /// read on (`input_line`). (OCaml 4.14's runtime handles one only once
    /// the read has returned; OCaml 5's, as `uitext` expects, at once.)
    fn read_input(&mut self) -> Result<Vec<u8>, NoKey> {
        let (raw, r) = match self.ready.take() {
            Some(r) => r,
            None => {
                if self.reading.is_none() {
                    let t = cbreak_state(&self.cbreak);
                    self.reading = Some(t.is_some());
                    let _ = self.reads.send(t.map_or(Read::Line, Read::Raw));
                }
                loop {
                    match self.inputs.recv() {
                        Ok(Input::Key(k)) => break (self.reading.take().unwrap_or(false), k),
                        Ok(Input::Event(e)) => self.deferred.push_back(*e),
                        Ok(Input::Signal(s)) => {
                            self.on_signal(s).map_err(NoKey::Stop)?;
                            if self.reading == Some(true) {
                                return Err(NoKey::Interrupted);
                            }
                        }
                        Ok(Input::Ended) | Err(_) => {
                            return Err(NoKey::Stop(Stop::Error(
                                "The session stopped unexpectedly".to_owned(),
                            )));
                        }
                    }
                }
            }
        };
        match r {
            Ok(Some(b)) if raw && b.first() == Some(&3) => Err(NoKey::Stop(self.brk())),
            Ok(Some(b)) => Ok(b),
            Ok(None) => Err(NoKey::Eof),
            Err(e) => Err(NoKey::Os(e)),
        }
    }

    /// `getInput`: the next key, echoed.
    fn get_input(&mut self) -> Result<String, NoKey> {
        let s = if self.buffer.is_empty() {
            self.read_input()?
        } else {
            std::mem::take(&mut self.buffer)
        };
        let (t, rest) = input::split_token(&s);
        self.buffer = rest;
        let c = input::token_string(&t);
        if !c.is_empty() && !c.starts_with('\x1b') {
            self.display(&c);
        }
        Ok(c)
    }

    /// `selectAction`: asks for one of `choices` (`batch` answers
    /// without asking), with `again` to show the question again.
    fn select_action<A: Copy>(
        &mut self,
        batch: Option<&str>,
        choices: &[Choice<A>],
        again: &dyn Fn(&Text),
    ) -> Result<A, Stop> {
        loop {
            let a = match batch {
                Some(a) => Some(a.to_owned()),
                None => {
                    self.display("[");
                    for c in choices {
                        if c.names[0].is_empty() {
                            self.display(&input::format_name(&c.names[1]));
                        }
                    }
                    self.display("] ");
                    match self.get_input() {
                        Ok(a) => Some(a),
                        // Restart an interrupted read.
                        Err(NoKey::Interrupted) => None,
                        Err(NoKey::Eof) => return Err(self.read_failure("End of file")),
                        Err(NoKey::Os(e)) => {
                            return Err(self.read_failure(&input::error_message(&e)));
                        }
                        Err(NoKey::Stop(s)) => return Err(s),
                    }
                }
            };
            let Some(a) = a else {
                again(self);
                continue;
            };
            if a == "?" || a == input::KEY_F1 {
                self.new_line();
                self.display("Commands:\n");
                for c in choices {
                    let n = c
                        .names
                        .iter()
                        .map(|n| input::format_name(n))
                        .collect::<Vec<_>>()
                        .join(" or ");
                    let space = " ".repeat(22usize.saturating_sub(n.len()).max(2));
                    self.display(&format!("  {n}{space}{}\n", c.doc));
                }
                again(self);
                continue;
            }
            if let Some(c) = choices.iter().find(|c| c.names.contains(&a)) {
                return Ok(c.act);
            }
            self.new_line();
            if a.is_empty() {
                self.display("No default command [type '?' or F1 for help]\n");
            } else {
                self.display(&format!(
                    "Unrecognized command '{}': try again  [type '?' or F1 for help]\n",
                    input::escaped(&a)
                ));
            }
            again(self);
        }
    }

    /// `handleExn`: stdin can't be read.
    fn read_failure(&self, s: &str) -> Stop {
        // So that the error starts on its own line, not after the prompt.
        infos::print("\n");
        Stop::Error(format!("Failure reading from the standard input ({s})\n"))
    }

    /// The warning printers: `setWarnPrinterForInitialization`, then
    /// `setWarnPrinter`'s.
    fn warn(&self, w: &str) {
        if self.running {
            self.infos.set("", "");
            infos::print("Warning: ");
            infos::print(&format!("{w}\n"));
        } else {
            infos::print(&format!("Warning: {w}\n\n"));
        }
    }

    /// A question's answer, or why there is none: then the session is
    /// interrupted before the reply is dropped, which would answer no.
    fn asked<T>(&mut self, r: Result<T, Stop>) -> Result<T, Stop> {
        if r.is_err() {
            self.brk();
        }
        r
    }

    fn answer(&mut self, q: Question) -> Result<(), Stop> {
        match q {
            Question::Warning(w, r) => {
                self.warn(&w);
                if !self.running {
                    r.send(true);
                    return Ok(());
                }
                // `setWarnPrinter`, outside batch mode.
                self.display("Press return to continue.");
                let go = self.select_action(
                    None,
                    &[
                        Choice::new(&["", "", " ", "y"], "Continue", true),
                        Choice::new(&["n", "q", "x"], "Exit", false),
                    ],
                    &|t| t.display("Press return to continue."),
                );
                let go = self.asked(go)?;
                self.new_line();
                if !go {
                    self.restore_terminal();
                }
                r.send(go);
            }
            // `promptForRoots`: an error, after the session's reply.
            Question::Roots(r) => {
                self.asked_roots = true;
                drop(r);
            }
            Question::Merge { text, reply, .. } => {
                let keep = self.verify_merge(&text);
                reply.send(self.asked(keep)?);
            }
        }
        Ok(())
    }

    /// A merge's output, when it needn't be confirmed (`verifyMerge` in
    /// batch mode, or without `confirmmerge`).
    fn show_merge(&self, text: &str) {
        self.infos.set("", "");
        infos::print(&format!("{text}\n"));
    }

    /// `verifyMerge`, with `confirmmerge`: keep the merge's result?
    fn verify_merge(&mut self, text: &str) -> Result<bool, Stop> {
        self.show_merge(text);
        self.display("Commit results of merge? ");
        let keep = self.select_action(
            None,
            &[
                Choice::new(&["y", "g"], "Yes: commit", true),
                Choice::new(&["n"], "No: leave this file unchanged", false),
            ],
            &|t| t.display("Commit results of merge? "),
        )?;
        self.new_line();
        Ok(keep)
    }

    /// `showStatus`: the path being scanned, with a spinner.
    fn show_scanning(&self, path: &str) {
        if path.is_empty() {
            self.infos.set("", "");
            return;
        }
        if !self.show_infos() {
            return;
        }
        let c = b"-\\|/"[(4. * system::gettimeofday() % 4.) as usize % 4] as char;
        let nowrap = terminal::vt_capable(Output::Stdout)
            && !std::env::var("TERM").is_ok_and(|s| {
                s == "dumb"
                    || s == "emacs"
                    || ["sun", "cons", "eterm", "cygwin", "dvtm"]
                        .iter()
                        .any(|p| s.starts_with(p))
            });
        if nowrap && !self.dumbtty() {
            self.infos
                .set(&format!("{c} \x1b[?7l{path}\x1b[?7h"), "\r\x1b[K\r");
        } else {
            self.infos.set(&format!("{c} {}", shorten(path)), "");
        }
    }

    /// `errorOut`: the short usage, and `s`; exits with 1.
    fn error_out(&self, s: &str) -> Stop {
        self.infos.msg(&format!(
            "{}{}{s}\n",
            uicommon::short_usage_msg(),
            profmgr_usage_msg()
        ));
        Stop::Exit(1)
    }

    /// `handleException`: for an error, or (`None`) `Sys.Break`.
    fn handle_exception(&mut self, e: Option<&str>) {
        // Keep the current status line, if any, and don't show it again.
        infos::print("\n");
        self.infos.set("", "");
        self.restore_terminal();
        let msg = match e {
            Some(e) => format!("Error: {e}"),
            None => "Terminated!".to_owned(),
        };
        self.log(LogKind::Log, format!("{msg}\n"));
        // Unless log entries go to stderr too.
        if self.silent() {
            infos::print(&format!("\n{msg}\n"));
        }
    }

    /// Exits with a message on stderr (what isn't ported yet).
    fn unsupported(&self, what: &str) -> Stop {
        self.infos
            .msg(&format!("{MY_NAME}: {what} is not supported yet\n"));
        Stop::Exit(1)
    }

    fn run(&mut self) -> Result<i32, Stop> {
        let profile = match self.call(Command::Start) {
            Ok(Event::Started(profile)) => profile,
            Ok(e) => return Err(Stop::Error(format!("unexpected {e:?}"))),
            Err(Stop::Error(s)) => return Err(self.error_out(&format!("\n\n{s}"))),
            Err(e) => return Err(e),
        };
        let profile = match profile {
            Some(p) => p,
            None if self.profile_manager_asked() => {
                let p = self.get_profile("default");
                self.restore_terminal();
                match p? {
                    Some(p) => p,
                    None => return Ok(0),
                }
            }
            None => "default".to_owned(),
        };
        match self.call(Command::LoadProfile(profile)) {
            Ok(Event::ProfileLoaded(s)) => self.settings = Some(s),
            Ok(e) => return Err(Stop::Error(format!("unexpected {e:?}"))),
            Err(Stop::Error(_)) if self.asked_roots => return Err(self.error_out("")),
            Err(e) => return Err(e),
        }
        // Some settings imply others.
        if self.silent() {
            let o = Overrides {
                batch: Some(true),
                terse: Some(true),
                log_to_stderr: Some(false),
                auto: None,
            };
            if let Event::ProfileLoaded(mut s) = self.call(Command::Configure(o))? {
                s.dumbtty = true;
                self.settings = Some(s);
            }
        }
        if self.settings().repeat != Repeat::NoRepeat {
            let o = Overrides {
                batch: Some(true),
                ..Overrides::default()
            };
            if let Event::ProfileLoaded(s) = self.call(Command::Configure(o))? {
                let dumbtty = self.dumbtty();
                self.settings = Some(Settings { dumbtty, ..s });
            }
        }
        self.set_color_preference();
        self.start2()
    }

    /// `getProfile`: the profile manager. Lists the profiles (unless there
    /// are none, or too many) and asks for one; `None` if the user chooses
    /// none.
    fn get_profile(&mut self, default: &str) -> Result<Option<String>, Stop> {
        let mut profiles = match self.call(Command::ScanProfiles)? {
            Event::Profiles(p) => p,
            e => return Err(Stop::Error(format!("unexpected {e:?}"))),
        };
        if profiles.len() > 10 {
            let dir = gemel_fs::os::unison_dir();
            self.log(
                LogKind::Log,
                format!(
                    "You have too many profiles in {} for interactive selection. Please \
                     specify profile or roots on command line.\n",
                    dir.display()
                ),
            );
            self.log(LogKind::Log, "The profile names are:\n".to_owned());
            for (p, _) in &profiles {
                self.log(LogKind::Log, format!("  {p}\n"));
            }
            self.log(LogKind::Log, "\n".to_owned());
            return Ok(Some(default.to_owned()));
        }
        if profiles.is_empty() {
            return Ok(Some(default.to_owned()));
        }
        profiles.sort_by(|(p, _), (q, _)| p.cmp(q));
        let list = add_profile_keys(&profiles, default);
        self.setup_terminal();
        self.set_color_preference();
        use Color::*;
        self.display("Available profiles:\n key:  profilename         label\n");
        for (key, (profile, info)) in &list {
            self.display(&format!(
                "  {}{key}{} :",
                self.color(Focus),
                self.color(Reset)
            ));
            self.display(&format!(
                "  {}{profile:<18}{}  {}{}{}\n",
                self.color(Focus),
                self.color(Reset),
                self.color(Information),
                info.label.as_deref().unwrap_or(""),
                self.color(Reset)
            ));
            for (i, root) in info.roots.iter().enumerate() {
                self.display(&format!("         root {} = {root}\n", i + 1));
            }
        }
        self.display("\n");
        let mut choices = vec![
            Choice::new(&["", "n", "/"], "Don't select any profile", None),
            Choice::new(&["q"], format!("exit {MY_NAME}"), Some(None)),
        ];
        choices.extend(list.iter().enumerate().map(|(i, (key, (profile, _)))| {
            Choice::new(
                &[key.as_str()],
                format!("Profile: {profile}"),
                Some(Some(i)),
            )
        }));
        self.display("Select a profile ");
        let a = self.select_action(None, &choices, &|t| t.display("Select a profile "))?;
        self.new_line();
        match a {
            None => {
                self.display("\nNo profile selected\n\n");
                Ok(None)
            }
            Some(None) => Err(self.brk()),
            Some(Some(i)) => {
                let profile = list[i].1.0.clone();
                self.display(&format!("\nProfile {profile} selected\n\n"));
                Ok(Some(profile))
            }
        }
    }

    fn profile_manager_asked(&self) -> bool {
        gemel_ui_api::new_prefs()
            .ok()
            .and_then(|p| p.scan_cmd_line(&self.args).ok())
            .is_some_and(|a| {
                a.contains_key(gemel_ui_api::cli::PROFILE_MANAGER.name())
                    && !a.contains_key(uicommon::RUN_TESTS_PREF_NAME)
            })
    }

    /// `start2`: connects the roots, then syncs.
    fn start2(&mut self) -> Result<i32, Stop> {
        match self.call(Command::Connect)? {
            Event::Connected(r) => self.roots = Some(r),
            e => return Err(Stop::Error(format!("unexpected {e:?}"))),
        }
        if self.settings().test_server {
            return Ok(0);
        }
        if self.settings().run_tests {
            return Err(self.unsupported("-selftest"));
        }
        // Put the terminal in cbreak mode if possible.
        if !self.batch() {
            self.setup_terminal();
        }
        self.running = true;
        self.synchronize_until_done()
    }

    /// `synchronizeUntilDone`: syncs once, or with `-repeat` again after
    /// each interval until the user asks to stop between syncs.
    fn synchronize_until_done(&mut self) -> Result<i32, Stop> {
        let interval = match self.settings().repeat.clone() {
            Repeat::NoRepeat => return self.synchronize_until_no_failures(false),
            Repeat::Interval(i) => i,
            Repeat::Watch => return self.synchronize_paths_from_filesystem_watcher(None),
            Repeat::WatchAndInterval(i) => {
                return self.synchronize_paths_from_filesystem_watcher(Some(i));
            }
            Repeat::Invalid(_, e) => return Err(Stop::Error(e)),
        };
        loop {
            let code = self.synchronize_until_no_failures(true)?;
            if self.safe_stop {
                return Ok(code);
            }
            self.status(&format!("\nSleeping for {interval} seconds...\n"));
            self.interruptible_sleep(interval as f64)?;
            if self.safe_stop {
                return Ok(code);
            }
        }
    }

    /// `synchronizeUntilNoFailures`: syncs, then the paths that failed
    /// again, `retry` times (in repeat mode, unless the user asked to
    /// stop).
    fn synchronize_until_no_failures(&mut self, repeat: bool) -> Result<i32, Stop> {
        let mut tries_left = self.settings().retry;
        let mut paths = None;
        loop {
            let (code, failed) = self.synchronize_once(repeat, paths)?;
            if failed.is_empty() || tries_left == 0 || (repeat && self.safe_stop) {
                return Ok(code);
            }
            tries_left -= 1;
            paths = Some((failed, Vec::new()));
        }
    }

    /// `synchronizePathsFromFilesystemWatcher`: syncs, then again whenever
    /// the watcher reports changes, and whole every `full_interval`
    /// seconds if given. A path that fails is retried at once, then after
    /// [`RETRY_DELAY`] seconds, doubling up to [`MAX_DELAY`].
    fn synchronize_paths_from_filesystem_watcher(
        &mut self,
        full_interval: Option<i64>,
    ) -> Result<i32, Stop> {
        let full_interval = full_interval.map_or(1e20, |i| i as f64);
        let mut last_full = 0.;
        // For each path to retry: when, and the delay that led there.
        let mut delay_info: BTreeMap<Vec<u8>, (Path<Global>, f64, f64)> = BTreeMap::new();
        loop {
            let t = now();
            let is_full = t - last_full > full_interval;
            if is_full {
                last_full = t;
            }
            let next_full = last_full + full_interval;
            // In OCaml's order: a fold over the map, building lists.
            let (mut delayed, mut ready) = (Vec::new(), Vec::new());
            for (p, t2, _) in delay_info.values().rev() {
                if *t2 <= t {
                    ready.push(p.clone());
                } else {
                    delayed.push(p.clone());
                }
            }
            let paths = (!is_full).then(|| (ready, delayed.clone()));
            let (code, failed) = self.synchronize_once(true, paths)?;
            // After a failure, retried at once, then with a growing delay.
            let mut next = BTreeMap::new();
            for p in delayed.into_iter().chain(failed) {
                let key = p.as_bytes().to_vec();
                let (t2, d) = match delay_info.get(&key) {
                    Some((_, t2, d)) if *t2 > t => (*t2, *d),
                    Some((_, _, d)) => {
                        let d = RETRY_DELAY.max(MAX_DELAY.min(2. * d));
                        (t + d, d)
                    }
                    None => (t, 0.),
                };
                next.insert(key, (p, t2, d));
            }
            delay_info = next;
            self.interruptible_sleep(WATCH_INTERVAL)?;
            let next_time = delay_info
                .values()
                .fold(next_full, |m, (_, t2, _)| m.min(*t2));
            if !self.safe_stop {
                self.wait_for_changes(next_time)?;
            }
            if self.safe_stop {
                return Ok(code);
            }
        }
    }

    /// `synchronizeOnce`: the exit code, and the paths that failed.
    fn synchronize_once(
        &mut self,
        want_watcher: bool,
        paths: Subpaths,
    ) -> Result<(i32, Vec<Path<Global>>), Stop> {
        let u = match self.call(Command::Update {
            allow_partial: true,
            want_watcher,
            paths,
            only: None,
        })? {
            Event::Updates(u) => u,
            e => return Err(Stop::Error(format!("unexpected {e:?}"))),
        };
        let no_repeat = self.settings().repeat == Repeat::NoRepeat;
        if u.items.is_empty() {
            if u.found_archives && no_repeat {
                self.call(Command::Commit)?;
            }
            self.status(if u.any_equal_updates {
                "Nothing to do: replicas have been changed only in identical ways since last sync."
            } else {
                "Nothing to do: replicas have not changed since last sync."
            });
            return Ok((ExitCode::Perfect as i32, Vec::new()));
        }
        self.check_for_dangerous_path(&u)?;
        let p = self.interact_and_propagate(&u)?;
        let code = ExitCode::from_outcome(p.skipped || p.partial || p.not_started, p.failures);
        Ok((code as i32, p.failed))
    }

    /// `checkForDangerousPath`: aborts in batch mode, and otherwise asks.
    fn check_for_dangerous_path(&mut self, u: &Updates) -> Result<(), Stop> {
        let Some(msg) = &u.dangerous_message else {
            return Ok(());
        };
        if !self.settings().confirm_big_deletes {
            return Ok(());
        }
        self.log(LogKind::Log, format!("{msg}\n"));
        if self.batch() {
            infos::print("Aborting...\n");
            self.restore_terminal();
            return Err(Stop::Exit(ExitCode::Fatal as i32));
        }
        self.display_when_interactive("Do you really want to proceed? ");
        let go = self.select_action(
            None,
            &[
                Choice::new(&["y"], "Continue", true),
                Choice::new(&["n", "q", "x", ""], "Exit", false),
            ],
            &|t| t.display("Do you really want to proceed? "),
        )?;
        if go {
            return Ok(());
        }
        infos::print("\n");
        self.restore_terminal();
        Err(Stop::Exit(ExitCode::Fatal as i32))
    }

    /// `displayri`: an item on a line.
    fn display_ri(&self, v: &ItemView) {
        use Color::*;
        let (default, forced) = match v.action {
            Action::Error => ((AError, "error"), (AError, "error")),
            Action::Skip(_) => ((ASkip, "<-?->"), (ASkip, "<=?=>")),
            Action::LtoR(false) => ((ALtoRf, "---->"), (ALtoRf, "====>")),
            Action::LtoR(true) => ((ALtoRt, "--?->"), (ALtoRt, "==?=>")),
            Action::RtoL(false) => ((ARtoLf, "<----"), (ARtoLf, "<====")),
            Action::RtoL(true) => ((ARtoLt, "<-?--"), (ARtoLt, "<=?==")),
            Action::Merge => ((AMerge, "<-M->"), (AMerge, "<=M=>")),
        };
        let (c, a) = if v.changed_from_default {
            forced
        } else {
            default
        };
        let s = format!(
            "{} {}{a}{} {}   {}  ",
            v.status1,
            self.color(c),
            self.color(Reset),
            v.status2,
            v.shown_path
        );
        if v.problem.is_some() || matches!(v.action, Action::Skip(_)) {
            infos::print(&s);
        } else {
            self.display(&s);
        }
    }

    /// `alwaysDisplayDetails`.
    fn always_display_details(&self, v: &ItemView) {
        infos::print(&format!("{}\n", v.details));
        if v.problem.is_none() {
            for e in &v.errors1 {
                infos::print(&format!("[root 1]: {e}\n"));
            }
            for e in &v.errors2 {
                infos::print(&format!("[root 2]: {e}\n"));
            }
        }
    }

    /// `displayDetails`.
    fn display_details(&self, v: &ItemView) {
        if !self.silent() {
            self.always_display_details(v);
        }
    }

    /// The skipped items, as the summary lists them.
    fn log_skipped(&self, items: &[Entry]) {
        for e in items {
            if let Some(r) = &e.v.skip_reason {
                self.log(
                    LogKind::Log,
                    format!("  skipped: {} ({r})\n", e.v.path_string),
                );
            }
        }
    }

    /// `interactAndPropagateChanges`: goes through the items, then
    /// propagates them once the user says so.
    fn interact_and_propagate(&mut self, u: &Updates) -> Result<Propagated, Stop> {
        use Color::*;
        let mut prev = Vec::new();
        let mut ril: VecDeque<Entry> = u
            .items
            .iter()
            .enumerate()
            .map(|(i, v)| Entry { i, v: v.clone() })
            .collect();
        'interact: loop {
            let (proceed, mut list) = self.interact(prev, ril)?;
            let (mut to_do, mut skipped, mut to_root1, mut to_root2) = (0, 0, 0, 0);
            for e in &list {
                if e.v.problematic {
                    skipped += 1;
                    continue;
                }
                to_do += 1;
                match e.v.action {
                    Action::LtoR(_) => to_root2 += e.v.length,
                    Action::RtoL(_) => to_root1 += e.v.length,
                    _ => {}
                }
            }
            let (root1, root2) = &u.roots.full;
            if !self.terse() && (to_do > 0 || skipped > 0) {
                self.log(
                    LogKind::LogColor,
                    format!(
                        "\n{}{to_do}{} items will be synced, {}{skipped}{} skipped\n\
                         {} to be synced from {root1} to {root2}\n\
                         {} to be synced from {root2} to {root1}\n",
                        self.color(Focus),
                        self.color(Reset),
                        self.color(Information),
                        self.color(Reset),
                        util::bytes2string(to_root2),
                        util::bytes2string(to_root1),
                    ),
                );
            }
            if to_do == 0 {
                if u.found_archives && self.settings().repeat == Repeat::NoRepeat {
                    self.call(Command::Commit)?;
                }
                self.display("No updates to propagate\n");
                if skipped > 0 {
                    self.log(
                        LogKind::LogColor,
                        format!(
                            "Synchronization {}complete{} at {}  (0 items transferred, \
                             {}{skipped} skipped{}, 0 failed)\n",
                            self.color(Success),
                            self.color(Reset),
                            hms(),
                            self.color(Information),
                            self.color(Reset),
                        ),
                    );
                    self.log_skipped(&list);
                }
                return Ok(Propagated {
                    skipped: skipped > 0,
                    ..Propagated::default()
                });
            }
            if proceed == Proceed::Immediately {
                return self.doit(&list, to_do, skipped);
            }
            // `askagain`.
            loop {
                self.display_when_interactive("\nProceed with propagating updates? ");
                let batch = self.batch().then_some("y");
                let a = self.select_action(
                    batch,
                    &[
                        Choice::new(
                            &["y", "g"],
                            "Yes: proceed with updates as selected above",
                            Ask::Yes,
                        ),
                        Choice::new(
                            &["n"],
                            "No: go through reconciliation process again",
                            Ask::No,
                        ),
                        Choice::new(
                            &["p", "b"],
                            "go back to the last item of the reconciliation",
                            Ask::Back,
                        ),
                        Choice::new(&["N"], "sort by Name", Ask::Sort(session::SortKind::Name)),
                        Choice::new(&["S"], "sort by Size", Ask::Sort(session::SortKind::Size)),
                        Choice::new(
                            &["W"],
                            "sort neW first (toggle)",
                            Ask::Sort(session::SortKind::NewFirst),
                        ),
                        Choice::new(
                            &["D"],
                            "Default ordering",
                            Ask::Sort(session::SortKind::Default),
                        ),
                        Choice::new(&["R"], "Reverse the sort order", Ask::Reverse),
                        Choice::new(
                            &["q", input::KEY_ESC],
                            format!("exit {MY_NAME} without propagating any changes"),
                            Ask::Quit,
                        ),
                    ],
                    &|t| t.display("Proceed with propagating updates? "),
                )?;
                match a {
                    Ask::Yes => return self.doit(&list, to_do, skipped),
                    Ask::No | Ask::Back => {
                        self.new_line();
                        self.call(Command::Configure(Overrides {
                            auto: Some(false),
                            ..Overrides::default()
                        }))?;
                        if let Some(s) = &mut self.settings {
                            s.auto = false;
                        }
                        prev = Vec::new();
                        ril = list.into();
                        if a == Ask::Back {
                            prev = ril.drain(..ril.len().saturating_sub(1)).collect();
                        }
                        continue 'interact;
                    }
                    Ask::Sort(kind) => {
                        self.arrange(&mut list)?;
                        list = match self.call(Command::Sort(kind))? {
                            Event::Items { items, .. } => items
                                .into_iter()
                                .enumerate()
                                .map(|(i, v)| Entry { i, v })
                                .collect(),
                            e => return Err(Stop::Error(format!("unexpected {e:?}"))),
                        };
                    }
                    Ask::Reverse => list.reverse(),
                    Ask::Quit => {
                        self.new_line();
                        return Err(self.brk());
                    }
                }
            }
        }
    }

    /// Makes the session's items these, in this order; they are numbered
    /// again.
    fn arrange(&mut self, list: &mut [Entry]) -> Result<(), Stop> {
        let order = list.iter().map(|e| e.i).collect();
        match self.call(Command::Arrange(order))? {
            Event::Items { .. } => {}
            e => return Err(Stop::Error(format!("unexpected {e:?}"))),
        }
        for (i, e) in list.iter_mut().enumerate() {
            e.i = i;
        }
        Ok(())
    }

    /// `doit`: propagates, commits, and logs the summary.
    fn doit(&mut self, list: &[Entry], to_do: usize, skipped: usize) -> Result<Propagated, Stop> {
        use Color::*;
        let mut list = list.to_vec();
        self.arrange(&mut list)?;
        if !(self.batch() || self.terse()) {
            self.new_line();
        }
        if !self.terse() {
            self.status("Propagating updates");
        }
        let timer = self.start_timer("Transmitting all files");
        let summary = self.do_transport()?;
        self.show_timer(timer);
        if !self.terse() {
            self.status("Saving synchronizer state");
        }
        self.call(Command::Commit)?;
        let failures = self.failed.len();
        let partials = self.partial.len();
        let not_started = summary.not_started;
        let trans = to_do - not_started - failures;
        let done = failures == 0 && not_started == 0;
        let line = format!(
            "Synchronization {}{}{} at {}  ({trans} item{} transferred, {}{}, {}{})",
            self.color(if done { Success } else { Failure }),
            if done { "complete" } else { "incomplete" },
            self.color(Reset),
            hms(),
            if trans == 1 { "" } else { "s" },
            if partials != 0 {
                format!("{partials} partially transferred, ")
            } else {
                String::new()
            },
            if skipped == 0 {
                "0 skipped".to_owned()
            } else {
                format!(
                    "{}{skipped} skipped{}",
                    self.color(Information),
                    self.color(Reset)
                )
            },
            if failures == 0 {
                "0 failed".to_owned()
            } else {
                format!(
                    "{}{failures} failed{}",
                    self.color(Failure),
                    self.color(Reset)
                )
            },
            if not_started == 0 {
                String::new()
            } else {
                format!(
                    ", {}{not_started} not started{}",
                    self.color(Information),
                    self.color(Reset)
                )
            },
        );
        self.log(LogKind::LogColor, format!("{line}\n"));
        if skipped > 0 {
            self.log_skipped(&list);
        }
        for p in &self.partial {
            self.log(
                LogKind::Log,
                format!("  partially transferred: {}\n", uicommon::path_string(p)),
            );
        }
        for p in &self.failed {
            self.log(
                LogKind::Log,
                format!("  failed: {}\n", uicommon::path_string(p)),
            );
        }
        // Make sure repeat mode is stopped.
        if self.interrupts > 0 {
            return Err(self.brk());
        }
        Ok(Propagated {
            skipped: skipped > 0,
            partial: partials > 0,
            failures: failures > 0,
            not_started: not_started > 0,
            failed: std::mem::take(&mut self.failed),
        })
    }

    /// `Trace.startTimer`.
    fn start_timer(&self, desc: &'static str) -> (&'static str, Instant) {
        if self.settings().timers {
            self.log(LogKind::Message, format!("{desc}..."));
        }
        (desc, Instant::now())
    }

    /// `Trace.showTimer`.
    fn show_timer(&self, (desc, t0): (&'static str, Instant)) {
        if self.settings().timers {
            self.log(
                LogKind::Message,
                format!("{desc} ({:.2} seconds)", t0.elapsed().as_secs_f64()),
            );
        }
    }

    /// `doTransport`: propagates, with the progress line, the failures as
    /// they come, and Ctrl-C and `SIGTERM` aborting it (`stopAtIntr`).
    fn do_transport(&mut self) -> Result<Summary, Stop> {
        self.failed.clear();
        self.partial.clear();
        let now = system::gettimeofday();
        self.progress = Some(ProgressLine {
            shown: 0.,
            items: 0,
            logged: now,
        });
        let term = signal::catch(Signal::Terminate).ok();
        self.in_sync = true;
        if self.show_infos() {
            self.infos.set("Starting...", "");
        }
        let r = self.call(Command::Synchronize);
        if matches!(r, Err(Stop::Break)) {
            self.stop_session();
        }
        self.in_sync = false;
        if let Some(p) = term {
            signal::restore(Signal::Terminate, &p);
        }
        self.progress = None;
        let r = match r {
            Ok(Event::Synchronized(s)) => {
                self.infos.set("", "");
                return Ok(s);
            }
            Ok(e) => Stop::Error(format!("unexpected {e:?}")),
            Err(e) => e,
        };
        let why = match &r {
            Stop::Break => "interrupted",
            Stop::Error(_) => "failed",
            Stop::Exit(_) => return Err(r),
        };
        let how = if matches!(r, Stop::Break) {
            "by user request"
        } else {
            "due to a fatal error"
        };
        self.infos.set("", "");
        self.log(
            LogKind::LogColor,
            format!(
                "\nSynchronization {}{why}{} at {} {how}\n\n",
                self.color(Color::Failure),
                self.color(Color::Reset),
                hms()
            ),
        );
        Err(r)
    }

    /// An item propagated: a failure is shown at once.
    fn item_done(&mut self, v: &ItemView) {
        match &v.outcome {
            Some(Outcome::Failed(m)) => {
                self.infos.set("", "");
                infos::print(&format!("Failed [{}]: {m}\n", v.path_string));
                self.failed.push(v.path.clone());
            }
            Some(Outcome::Succeeded) if v.partially_problematic && !v.problematic => {
                self.partial.push(v.path.clone());
            }
            _ => {}
        }
    }

    /// `displayProgress`: the progress line, every 0.1 second or when an
    /// item finishes; in batch mode or with `terse`, in the log every
    /// minute.
    fn show_progress(&mut self, p: &Progress) {
        let show = self.show_infos();
        let to_log = self.terse() || self.batch();
        let Some(line) = self.progress.as_mut() else {
            return;
        };
        let t1 = system::gettimeofday();
        if !(t1 - line.shown >= 0.1 || line.items != p.items_done) {
            return;
        }
        line.shown = t1;
        line.items = p.items_done;
        let v = util::percentage_of_total_size(
            p.transferred + p.items_done as i64,
            p.length + p.items as i64,
        );
        let rem = if v <= 0. {
            "--:--".to_owned()
        } else if v >= 100. {
            "00:00:00".to_owned()
        } else if p.rate.is_nan() {
            "--:--".to_owned()
        } else {
            format!("{:>8}/s    {}", util::bytes2string(p.rate as i64), p.eta)
        };
        let s = format!(
            "{}  {}/{}  ({} of {})  {rem} ETA",
            util::percent2string(v),
            p.items_done,
            p.items,
            util::bytes2string(p.transferred),
            util::bytes2string(p.length),
        );
        let log = to_log && t1 - line.logged >= 60.;
        if log {
            line.logged = t1;
        }
        if show {
            self.infos.set(&s, "");
        }
        if log {
            self.log(LogKind::LogOnly, format!("{s}\n"));
        }
    }
}

/// The proceed menu's choices (`askagain`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ask {
    Yes,
    No,
    Back,
    Sort(session::SortKind),
    Reverse,
    Quit,
}

/// `addProfileKeys`: a key for each profile (sorted by name): its own, `d`
/// for the default, or the next digit no profile has; sorted by key.
fn add_profile_keys(
    profiles: &[(String, ProfileInfo)],
    default: &str,
) -> Vec<(String, (String, ProfileInfo))> {
    // `profileKeymap`: the scan has checked that each key is a digit.
    let taken: Vec<i64> = profiles
        .iter()
        .filter_map(|(_, i)| i.key.as_deref().and_then(util::int_of_string))
        .collect();
    let next_avail_key = |i: usize| {
        (i + 1..10)
            .find(|n| !taken.contains(&(*n as i64)))
            .unwrap_or(10.max(i + 1))
    };
    let mut keyed = Vec::new();
    let mut i = 0;
    for (p, info) in profiles {
        let key = match &info.key {
            Some(k) => k.clone(),
            None if p == default => "d".to_owned(),
            None => {
                let k = i.to_string();
                i = next_avail_key(i);
                k
            }
        };
        keyed.push((key, (p.clone(), info.clone())));
    }
    // OCaml builds the list backwards, and sorts it stably.
    keyed.reverse();
    keyed.sort_by(|(a, _), (b, _)| a.cmp(b));
    keyed
}

/// `shorten`: a path of at most 70 bytes, with its middle left out.
fn shorten(path: &str) -> String {
    const MAX: usize = 70;
    let mid = (MAX - 3) / 2;
    let b = path.as_bytes();
    if b.len() <= MAX {
        return path.to_owned();
    }
    let head = &b[..MAX - mid - 3];
    let tail = &b[b.len() - mid..];
    format!(
        "{}...{}",
        String::from_utf8_lossy(head),
        String::from_utf8_lossy(tail)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_are_formatted_as_uitext_does() {
        let f = format_status();
        assert_eq!(f("Looking for changes", ""), "Looking for changes");
        assert_eq!(f("Looking for changes", "a/b"), "  a/b");
        assert_eq!(f("Propagating", "x"), "Propagating\n  x");
        assert_eq!(f("", ""), "");
    }

    #[test]
    fn long_paths_are_shortened() {
        let p = "x".repeat(100);
        let s = shorten(&p);
        assert_eq!(s.len(), 70);
        assert_eq!(&s[34..37], "...");
        assert_eq!(shorten("short"), "short");
    }
}
