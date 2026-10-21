//! The text user interface, in batch mode.
//!
//! Ports Unison `src/uitext.ml`: its startup (`start` and `start2`), one
//! sync and its retries (`synchronizeOnce`, `synchronizeUntilNoFailures`),
//! the items as batch mode lists them (`interact`, `displayri`), the
//! propagation with its progress line and summary
//! (`interactAndPropagateChanges`, `doTransport`), the warning printers
//! and `handleException`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! It drives the engine through `gemel_ui_api::session`. While a step
//! runs, only the engine writes to stderr (log entries, statuses, the
//! wait message), through the trace, and only this thread writes to
//! stdout; so this thread's own messages to stderr come between steps.
//!
//! Not ported yet (phase 8b): interactive mode (the prompts, the terminal's
//! raw mode, interrupts), `-repeat`, the profile manager (`-i`), and the
//! timers of the whole propagation.

use std::io::IsTerminal;
use std::sync::Mutex;

use gemel_base::path::{Global, Path};
use gemel_base::prefs::BoolWithDefault;
use gemel_base::trace::Trace;
use gemel_base::util::{self, MY_NAME};
use gemel_fs::system;
use gemel_rpc::runtime::EngineThread;
use gemel_ui_api::cli::Interface;
use gemel_ui_api::exit_code::ExitCode;
use gemel_ui_api::session::{
    self, Command, Config, Event, ItemView, LogKind, Outcome, Overrides, Progress, Question,
    Settings, Summary, Updates,
};
use gemel_ui_api::uicommon::{self, Action, Repeat};

use crate::infos::{self, Infos};

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

struct Text {
    session: EngineThread<Command, Event>,
    infos: Infos,
    args: Vec<String>,
    settings: Option<Settings>,
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

/// `Uitext.start`: starts a session, syncs once (with retries) in batch
/// mode, and returns the exit code.
pub fn start(args: Vec<String>, interface: Interface) -> i32 {
    let infos = Infos::default();
    if interface != Interface::Text {
        infos.msg(&format!(
            "This {MY_NAME} binary only provides the text GUI...\n"
        ));
    }
    let mut trace = Trace::with_sinks(Box::new(std::io::stdout()), Box::new(infos.stderr()));
    trace.set_status_formatter(Some(format_status()));
    let session = match session::spawn(Config {
        unison_dir: gemel_fs::os::unison_dir(),
        args: args.clone(),
        trace,
        loopback: false,
        status_events: false,
        scan_status: true,
        log_warnings: false,
        print_wait_message: true,
    }) {
        Ok(s) => s,
        Err(e) => {
            infos.msg(&format!("Error: {e}\n"));
            return ExitCode::Fatal as i32;
        }
    };
    let mut ui = Text {
        session,
        infos,
        args,
        settings: None,
        colors: false,
        running: false,
        asked_roots: false,
        failed: Vec::new(),
        partial: Vec::new(),
        progress: None,
    };
    let code = match ui.run() {
        Ok(code) | Err(Stop::Exit(code)) => code,
        Err(Stop::Error(e)) => {
            ui.handle_exception(&e);
            ExitCode::Fatal as i32
        }
    };
    // So that what was asked to be logged is.
    let Text { session, .. } = ui;
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
            Color::AError => "\x1b[31m",
            Color::ASkip => "\x1b[1;35m",
            Color::ALtoRt | Color::ARtoLt => "\x1b[1;33m",
            Color::AMerge => "\x1b[1;36m",
        }
    }

    /// `setColorPreference`.
    fn set_color_preference(&mut self) {
        let s = self.settings();
        let env_ok = std::env::var_os("NO_COLOR").is_none();
        let term_ok = std::env::var_os("TERM").is_none_or(|t| t != "dumb");
        let tty_ok = std::io::stdout().is_terminal() && std::io::stderr().is_terminal();
        let ok = env_ok && term_ok && tty_ok && !s.dumbtty;
        self.colors = match s.color {
            BoolWithDefault::True => true,
            BoolWithDefault::False => false,
            BoolWithDefault::Default => ok,
        };
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

    /// Sends `c` and handles the events up to its reply.
    fn call(&mut self, c: Command) -> Result<Event, Stop> {
        let sync = matches!(c, Command::Synchronize);
        self.send(c);
        loop {
            let Some(e) = self.session.recv_blocking() else {
                return Err(Stop::Error("The session stopped unexpectedly".to_owned()));
            };
            match e {
                Event::Error(e) => return Err(Stop::Error(e)),
                Event::Exit(code) => return Err(Stop::Exit(code as i32)),
                Event::Warning(w) => self.warn(&w),
                Event::Ask(q) => self.answer(q),
                Event::Scanning(p) => self.show_scanning(&p),
                Event::Progress(p) => self.show_progress(&p),
                Event::Item(_, v) if sync => self.item_done(&v),
                Event::Merge { text, .. } => self.verify_merge(&text),
                Event::Status(_) | Event::Contacting(_) | Event::Refused(_) => {}
                e => return Ok(e),
            }
        }
    }

    /// The warning printers: `setWarnPrinterForInitialization`, then
    /// `setWarnPrinter`'s (which asks, outside batch mode: phase 8b).
    fn warn(&self, w: &str) {
        if self.running {
            self.infos.set("", "");
            infos::print("Warning: ");
            infos::print(&format!("{w}\n"));
        } else {
            infos::print(&format!("Warning: {w}\n\n"));
        }
    }

    fn answer(&mut self, q: Question) {
        match q {
            // Only shown at initialisation; asking is phase 8b.
            Question::Warning(w, r) => {
                self.warn(&w);
                r.send(true);
            }
            // `promptForRoots`: an error, after the session's reply.
            Question::Roots(r) => {
                self.asked_roots = true;
                drop(r);
            }
            Question::Merge { text, reply, .. } => {
                self.verify_merge(&text);
                reply.send(true);
            }
        }
    }

    /// `verifyMerge`, in batch mode: shows the merge's output and goes on.
    fn verify_merge(&self, text: &str) {
        self.infos.set("", "");
        infos::print(&format!("{text}\n"));
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
        let nowrap = std::io::stdout().is_terminal()
            && !std::env::var("TERM").is_ok_and(|s| {
                s == "dumb"
                    || s == "emacs"
                    || ["sun", "cons", "eterm", "cygwin", "dvtm"]
                        .iter()
                        .any(|p| s.starts_with(p))
            });
        if nowrap && !self.settings().dumbtty {
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

    /// `handleException`.
    fn handle_exception(&self, e: &str) {
        infos::print("\n");
        self.infos.set("", "");
        let msg = format!("Error: {e}");
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
            // `getProfile`, without the profile manager.
            None if self.profile_manager_asked() => {
                return Err(self.unsupported("The interactive profile manager (-i)"));
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
            return Err(self.unsupported("-repeat"));
        }
        self.set_color_preference();
        self.start2()
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
        self.call(Command::Connect)?;
        if self.settings().test_server {
            return Ok(0);
        }
        if self.settings().run_tests {
            return Err(self.unsupported("-selftest"));
        }
        if !self.batch() {
            return Err(self.unsupported("Interactive mode (use -batch)"));
        }
        self.running = true;
        self.synchronize_until_no_failures()
    }

    /// `synchronizeUntilNoFailures`: syncs, then the paths that failed
    /// again, `retry` times.
    fn synchronize_until_no_failures(&mut self) -> Result<i32, Stop> {
        let mut tries_left = self.settings().retry;
        let mut paths = None;
        loop {
            let (code, failed) = self.synchronize_once(paths)?;
            if failed.is_empty() || tries_left == 0 {
                return Ok(code);
            }
            tries_left -= 1;
            paths = Some(failed);
        }
    }

    /// `synchronizeOnce`: the exit code, and the paths that failed.
    fn synchronize_once(
        &mut self,
        paths: Option<Vec<Path<Global>>>,
    ) -> Result<(i32, Vec<Path<Global>>), Stop> {
        let u = match self.call(Command::Update {
            allow_partial: true,
            paths,
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

    /// `checkForDangerousPath`, in batch mode: aborts.
    fn check_for_dangerous_path(&self, u: &Updates) -> Result<(), Stop> {
        let Some(msg) = &u.dangerous_message else {
            return Ok(());
        };
        if !self.settings().confirm_big_deletes {
            return Ok(());
        }
        self.log(LogKind::Log, format!("{msg}\n"));
        if self.batch() {
            infos::print("Aborting...\n");
            return Err(Stop::Exit(ExitCode::Fatal as i32));
        }
        Ok(())
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

    /// `displayDetails`.
    fn display_details(&self, v: &ItemView) {
        if self.silent() {
            return;
        }
        infos::print(&format!("{}\n", v.details));
        for e in &v.errors1 {
            infos::print(&format!("[root 1]: {e}\n"));
        }
        for e in &v.errors2 {
            infos::print(&format!("[root 2]: {e}\n"));
        }
    }

    /// `interact`, in batch mode: lists the items.
    fn interact(&self, items: &[ItemView]) {
        let auto = self.settings().auto;
        for v in items {
            self.display_ri(v);
            if let Some(p) = &v.problem {
                infos::print(&format!("\n{p}\n"));
                continue;
            }
            let conflict = matches!(v.action, Action::Skip(_));
            if auto && !conflict {
                self.display("\n");
                continue;
            }
            if self.silent() && conflict {
                infos::print("\n");
            }
            self.display("\n");
            if !self.terse() {
                self.display_details(v);
            }
        }
    }

    /// The skipped items, as the summary lists them.
    fn log_skipped(&self, items: &[ItemView]) {
        for v in items {
            if let Some(r) = &v.skip_reason {
                self.log(
                    LogKind::Log,
                    format!("  skipped: {} ({r})\n", v.path_string),
                );
            }
        }
    }

    /// `interactAndPropagateChanges`, in batch mode.
    fn interact_and_propagate(&mut self, u: &Updates) -> Result<Propagated, Stop> {
        use Color::*;
        self.interact(&u.items);
        let (mut to_do, mut skipped, mut to_root1, mut to_root2) = (0, 0, 0, 0);
        for v in &u.items {
            if v.problematic {
                skipped += 1;
                continue;
            }
            to_do += 1;
            match v.action {
                Action::LtoR(_) => to_root2 += v.length,
                Action::RtoL(_) => to_root1 += v.length,
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
                        "Synchronization {}complete{} at {}  (0 items transferred, {}{skipped} \
                         skipped{}, 0 failed)\n",
                        self.color(Success),
                        self.color(Reset),
                        hms(),
                        self.color(Information),
                        self.color(Reset),
                    ),
                );
                self.log_skipped(&u.items);
            }
            return Ok(Propagated {
                skipped: skipped > 0,
                ..Propagated::default()
            });
        }
        // `doit`.
        if !self.terse() {
            self.status("Propagating updates");
        }
        let summary = self.do_transport()?;
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
            self.log_skipped(&u.items);
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
        Ok(Propagated {
            skipped: skipped > 0,
            partial: partials > 0,
            failures: failures > 0,
            not_started: not_started > 0,
            failed: std::mem::take(&mut self.failed),
        })
    }

    /// `doTransport`: propagates, with the progress line, and the failures
    /// as they come.
    fn do_transport(&mut self) -> Result<Summary, Stop> {
        self.failed.clear();
        self.partial.clear();
        let now = system::gettimeofday();
        self.progress = Some(ProgressLine {
            shown: 0.,
            items: 0,
            logged: now,
        });
        if self.show_infos() {
            self.infos.set("Starting...", "");
        }
        let r = self.call(Command::Synchronize);
        self.progress = None;
        self.infos.set("", "");
        match r {
            Ok(Event::Synchronized(s)) => Ok(s),
            Ok(e) => Err(Stop::Error(format!("unexpected {e:?}"))),
            Err(Stop::Error(e)) => {
                self.log(
                    LogKind::LogColor,
                    format!(
                        "\nSynchronization {}failed{} at {} due to a fatal error\n\n",
                        self.color(Color::Failure),
                        self.color(Color::Reset),
                        hms()
                    ),
                );
                Err(Stop::Error(e))
            }
            Err(e) => Err(e),
        }
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
