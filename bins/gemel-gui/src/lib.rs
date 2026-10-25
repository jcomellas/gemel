//! `gemel-gui`: the graphical frontend, built with Slint.
//!
//! Ports Unison's `src/uigtk3.ml`: its flows, menus, keys and dialogs.
//! Started from the GUI spike in §6 of `docs/unison-port-analysis.md`. The
//! window is described in `ui/app.slint`; this file drives it from the
//! session (`gemel-ui-api`'s, on its engine thread), whose events a timer
//! takes, and turns the window's callbacks into the session's commands.
//!
//! It is a whole gemel, as `unison-gui` is a whole unison: the command
//! line is Unison's, and `gemel`'s library runs it. `-ui text` runs the
//! text interface, and so does a system without a display; `-server` and
//! `-socket` serve.
//!
//! What `uigtk3` does that isn't here yet (milestone 4's plan): the
//! profile manager, so a profile must be named on the command line (or
//! roots given); the progress bar, statistics and summary; rescanning and
//! stopping; diffs, ignoring, sorting, the Expert menu and the profile
//! keys; ssh's prompts.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.

pub mod recon_view;
mod task;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gemel_ui_api::clroot::{self, Clroot};
use gemel_ui_api::exit_code::ExitCode;
use gemel_ui_api::session::{
    self, Command, Config, Event, Favor, ItemView, LogKind, Overrides, Question, Reply, RootNames,
    SetAction, Settings, Summary, Updates,
};
use gemel_ui_api::{MY_NAME, Subpaths, Trace, padto};
use slint::{ComponentHandle, Model, ModelNotify, ModelRc, ModelTracker, SharedString};

use crate::recon_view::{Arrow, ClickMods, Done, ReconTable, Row};

/// The window, compiled from `ui/app.slint`.
pub mod ui {
    #![allow(missing_docs, unsafe_code, clippy::all, clippy::todo)]
    slint::include_modules!();
}

use ui::{AppWindow, RowData};

/// How the window starts its session.
pub struct Startup {
    /// The Unison directory.
    pub unison_dir: PathBuf,
    /// The command line, without the program's name.
    pub args: Vec<String>,
    /// Where the session prints (the log's entries, the debugging
    /// output); its status messages come to the window.
    pub trace: Trace,
}

impl Startup {
    /// As the program starts: the Unison directory (`UNISON`, or the
    /// user's), printing to standard output and error.
    pub fn new(args: Vec<String>) -> Startup {
        Startup {
            unison_dir: gemel_ui_api::unison_dir(),
            args,
            trace: Trace::with_sinks(Box::new(std::io::stdout()), Box::new(std::io::stderr())),
        }
    }
}

/// `myNameCapitalized`.
fn my_name_capitalized() -> String {
    let mut c = MY_NAME.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// `formatStatus`: the status, padded, then its detail.
fn format_status(major: &str, minor: &str) -> String {
    padto(30, &format!("{major}  ")) + minor
}

/// `tryAgainMessage`.
fn try_again_message() -> String {
    format!(
        "You can use {} to synchronize a local directory with another local directory,\n\
         or with a remote directory.\n\
         You can also synchronize a single file if you enter a file name.\n\
         \n\
         Please enter the first (local) directory that you want to synchronize.",
        my_name_capitalized()
    )
}

// ---- The rows ----

/// Exposes the [`ReconTable`] to Slint without copying it: rows are built
/// on demand for the visible part of the `ListView`, and changes are
/// reported row by row.
struct RowsModel {
    table: RefCell<ReconTable>,
    notify: ModelNotify,
}

impl RowsModel {
    fn changed(&self, rows: impl IntoIterator<Item = usize>) {
        for row in rows {
            self.notify.row_changed(row);
        }
    }
}

fn row_data(row: &Row, selected: bool) -> RowData {
    let item = &row.item;
    RowData {
        replica1: item.status1.trim_end().into(),
        replica2: item.status2.trim_end().into(),
        arrow: item.action.nice_string().into(),
        arrow_kind: match row.arrow() {
            Arrow::Error => 0,
            Arrow::Skip => 1,
            Arrow::Right { partial: true } | Arrow::Left { partial: true } => 3,
            Arrow::Right { .. } | Arrow::Left { .. } | Arrow::Merge => 2,
        },
        changed: item.changed_from_default,
        done: match row.done() {
            Done::No => 0,
            Done::Yes => 1,
            Done::Failed => 2,
        },
        status: row.status_text().into(),
        path: row.path().into(),
        selected,
        spoken: row.spoken().into(),
    }
}

impl Model for RowsModel {
    type Data = RowData;

    fn row_count(&self) -> usize {
        self.table.borrow().len()
    }

    fn row_data(&self, i: usize) -> Option<RowData> {
        let table = self.table.borrow();
        (i < table.len()).then(|| row_data(table.row(i), table.is_selected(i)))
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

// ---- Dialogs ----

/// What answering a dialog does.
enum OnAnswer {
    /// Nothing more (`messageBox`, a warning in batch mode).
    Close,
    /// Goes on (OK), or stops (Quit), at a warning (`warnBox`).
    Warning(Reply<bool>),
    /// The same, for a warning the window gives itself (big deletions):
    /// Quit ends the program.
    WarnHere,
    /// Keeps a merge's result (Yes), or not.
    Merge(Reply<bool>),
    /// Quit ends the program, Close goes on (`fatalError`).
    Error,
    /// Ends the program (`fatalError ~quit:true`).
    Fatal,
    /// Ends the program while it works (Yes), or not (`safeExit`).
    PrematureExit,
    /// The first root, typed or picked (`getFirstRoot`).
    FirstRoot(Reply<Option<(String, String)>>),
    /// The second (`getSecondRoot`), after the first.
    SecondRoot(String, Reply<Option<(String, String)>>),
}

/// A dialog waiting to be shown, or shown.
struct Dialog {
    /// 1 a message, 2 the first root, 3 the second (`ui/app.slint`).
    kind: i32,
    title: String,
    message: String,
    ok: &'static str,
    cancel: &'static str,
    on: OnAnswer,
}

impl Dialog {
    fn message(
        title: &str,
        message: &str,
        ok: &'static str,
        cancel: &'static str,
        on: OnAnswer,
    ) -> Dialog {
        Dialog {
            kind: 1,
            title: title.to_owned(),
            message: message.to_owned(),
            ok,
            cancel,
            on,
        }
    }
}

// ---- The window ----

/// What happens once [`Command::Configure`] has been applied.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AfterConfigure {
    Nothing,
    Synchronize,
}

/// What a commit ends.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AfterCommit {
    /// Update detection, with nothing to propagate.
    Detect,
    /// A sync.
    Sync,
}

#[derive(Default)]
struct State {
    /// Working: everything is disabled (`make_busy`, `grDisactivateAll`).
    busy: bool,
    /// `grGo`.
    go: bool,
    /// `grRescan`: what may be done once the items are shown.
    rescan: bool,
    settings: Option<Settings>,
    roots: Option<RootNames>,
    /// `profileInitSuccess`.
    profile_loaded: bool,
    /// The first items have been shown: an error no longer ends the
    /// program (`start`'s `initLoop`, then `fatalErrorHandler`).
    started: bool,
    /// `unsynchronizedPaths`.
    unsynchronized: Subpaths,
    after_configure: Option<AfterConfigure>,
    after_commit: Option<AfterCommit>,
    /// The last sync's, once committed.
    summary: Option<Summary>,
    dialogs: VecDeque<Dialog>,
    exit: Option<i32>,
    /// Without a session, the commands sent (for tests).
    sent: Vec<Command>,
}

/// The graphical interface: the window, its session, and what it shows.
pub struct App {
    ui: AppWindow,
    model: Rc<RowsModel>,
    session: RefCell<Option<session::Handle>>,
    detached: bool,
    state: RefCell<State>,
    /// The program is quitting: no more events are taken.
    quitting: Cell<bool>,
}

impl App {
    /// The window, with its session started as `startup` says, and asked
    /// for the profile the command line names.
    pub fn new(startup: Startup) -> Result<Rc<Self>, slint::PlatformError> {
        let mut trace = startup.trace;
        // `Trace.sendLogMsgsToStderr := false`, and uigtk3's status format.
        trace.send_log_msgs_to_stderr = false;
        trace.set_status_formatter(Some(Box::new(format_status)));
        let session = session::spawn(Config {
            unison_dir: startup.unison_dir,
            args: startup.args,
            trace,
            loopback: false,
            status_events: true,
            scan_status: false,
            log_warnings: false,
            print_wait_message: false,
            complete_progress: true,
            traffic: None,
        })
        .map_err(|e| slint::PlatformError::Other(format!("Can't start the session: {e}")))?;
        let app = App::create(Some(session))?;
        app.send(Command::Start);
        Ok(app)
    }

    /// The window without a session: events are given to it with
    /// [`App::handle`], and the commands it would send are kept
    /// ([`App::sent`]). For tests, and fake sources of events.
    pub fn detached() -> Result<Rc<Self>, slint::PlatformError> {
        App::create(None)
    }

    fn create(session: Option<session::Handle>) -> Result<Rc<Self>, slint::PlatformError> {
        let ui = AppWindow::new()?;
        let model = Rc::new(RowsModel {
            table: RefCell::new(ReconTable::default()),
            notify: ModelNotify::default(),
        });
        ui.set_rows(ModelRc::from(model.clone()));
        ui.set_mono_font(
            if cfg!(target_os = "macos") {
                "Menlo"
            } else if cfg!(windows) {
                "Consolas"
            } else {
                "DejaVu Sans Mono"
            }
            .into(),
        );
        ui.set_window_title(my_name_capitalized().into());
        let app = Rc::new(App {
            ui,
            model,
            detached: session.is_none(),
            session: RefCell::new(session),
            state: RefCell::new(State::default()),
            quitting: Cell::new(false),
        });
        app.wire();
        app.refresh_groups();
        Ok(app)
    }

    /// The window.
    pub fn ui(&self) -> &AppWindow {
        &self.ui
    }

    /// The items, as the window shows them.
    pub fn table(&self) -> std::cell::Ref<'_, ReconTable> {
        self.model.table.borrow()
    }

    /// The exit code, once the program has been asked to end.
    pub fn exit_code(&self) -> Option<i32> {
        self.state.borrow().exit
    }

    /// Working: the window is waiting for the session.
    pub fn busy(&self) -> bool {
        self.state.borrow().busy
    }

    /// The first items have been shown.
    pub fn started(&self) -> bool {
        self.state.borrow().started
    }

    /// The last sync's summary, once it has been committed.
    pub fn summary(&self) -> Option<Summary> {
        self.state.borrow().summary.clone()
    }

    /// Without a session, the commands sent so far.
    pub fn sent(&self) -> Vec<Command> {
        self.state.borrow().sent.clone()
    }

    /// Starts the timer that applies what the session sends (~60 Hz); it
    /// runs while the timer is kept.
    pub fn start_ticks(self: &Rc<Self>) -> slint::Timer {
        let timer = slint::Timer::default();
        let weak = Rc::downgrade(self);
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(16),
            move || {
                if let Some(app) = weak.upgrade() {
                    app.pump();
                }
            },
        );
        timer
    }

    /// Applies whatever the session sent since the last tick.
    fn pump(&self) {
        while !self.quitting.get() {
            let e = self
                .session
                .borrow_mut()
                .as_mut()
                .and_then(|s| s.try_recv());
            match e {
                Some(e) => self.handle(e),
                None => return,
            }
        }
    }

    fn send(&self, c: Command) {
        if self.detached {
            self.state.borrow_mut().sent.push(c);
        } else if let Some(s) = self.session.borrow().as_ref() {
            s.send(c);
        }
    }

    fn set_status(&self, s: &str) {
        self.ui.set_status_text(s.into());
    }

    /// `Trace.status`, as the window shows it.
    fn status(&self, s: &str) {
        self.set_status(&format_status(s, ""));
    }

    // ---- What is enabled ----

    /// Sets the window's groups from the state (`grSet`, `updateButtons`).
    fn refresh_groups(&self) {
        let (busy, go, rescan) = {
            let s = self.state.borrow();
            (s.busy, s.go, s.rescan)
        };
        let (actions, detail) = if busy {
            (false, false)
        } else {
            let t = self.model.table.borrow();
            match t.current() {
                None => (
                    t.selected().into_iter().any(|i| t.row(i).action_possible()),
                    false,
                ),
                Some(i) => (t.row(i).action_possible(), t.row(i).has_details()),
            }
        };
        self.ui.set_actions_enabled(actions);
        self.ui.set_detail_enabled(detail);
        self.ui.set_go_enabled(go && !busy);
        self.ui.set_select_enabled(rescan && !busy);
    }

    /// `grDisactivateAll` and `make_busy`.
    fn set_busy(&self, busy: bool) {
        {
            let mut s = self.state.borrow_mut();
            s.busy = busy;
            if busy {
                s.go = false;
                s.rescan = false;
            }
        }
        self.refresh_groups();
    }

    /// `updateDetails`: the item selected, if only one is, and what is
    /// enabled.
    fn update_details(&self) {
        {
            let t = self.model.table.borrow();
            match t.current().map(|i| t.row(i)) {
                None => {
                    self.ui.set_details_path(SharedString::new());
                    self.ui.set_details_text(SharedString::new());
                }
                Some(row) => {
                    let item = &row.item;
                    let formatted = item.problem.is_none()
                        && !matches!(item.outcome, Some(session::Outcome::Failed(_)));
                    let details = item.details.strip_suffix('\n').unwrap_or(&item.details);
                    self.ui.set_details_path(item.path_string.as_str().into());
                    self.ui.set_details_text(details.into());
                    self.ui.set_details_formatted(formatted);
                }
            }
        }
        self.refresh_groups();
    }

    /// Selects only row `i`, and shows it (`select`).
    fn select(&self, i: usize) {
        let changed = self.model.table.borrow_mut().select(i);
        self.model.changed(changed);
        self.ui.invoke_bring_into_view(i as i32);
        self.update_details();
    }

    /// `nextInteresting`.
    fn next_interesting(&self) {
        let auto = self
            .state
            .borrow()
            .settings
            .as_ref()
            .is_some_and(|s| s.auto);
        let next = self.model.table.borrow().next_interesting(auto);
        if let Some(i) = next {
            self.select(i);
        }
    }

    // ---- Dialogs ----

    fn push_dialog(&self, d: Dialog) {
        let first = {
            let mut s = self.state.borrow_mut();
            s.dialogs.push_back(d);
            s.dialogs.len() == 1
        };
        if first {
            self.show_dialog();
        }
    }

    /// Shows the first dialog waiting, if any.
    fn show_dialog(&self) {
        let s = self.state.borrow();
        let Some(d) = s.dialogs.front() else {
            self.ui.set_dialog(0);
            drop(s);
            self.ui.invoke_focus_table();
            return;
        };
        self.ui.set_dialog_title(d.title.as_str().into());
        self.ui.set_dialog_message(d.message.as_str().into());
        self.ui.set_dialog_ok(d.ok.into());
        self.ui.set_dialog_cancel(d.cancel.into());
        if d.kind != 1 {
            self.ui.set_root_dir(SharedString::new());
            self.ui.set_root_kind(0);
            self.ui.set_root_host(SharedString::new());
            self.ui.set_root_user(SharedString::new());
            self.ui.set_root_port(SharedString::new());
            self.ui.set_root_error(SharedString::new());
        }
        self.ui.set_dialog(d.kind);
    }

    /// A button of the dialog shown: OK (or the first one), or not.
    fn dialog_done(&self, ok: bool) {
        // The root selection checks what was typed first.
        let second_root = {
            let s = self.state.borrow();
            match s.dialogs.front().map(|d| &d.on) {
                Some(OnAnswer::SecondRoot(..)) if ok => true,
                Some(_) => false,
                None => return,
            }
        };
        let root = if second_root {
            match self.second_root() {
                Ok(r) => Some(r),
                Err(e) => {
                    self.ui.set_root_error(e.into());
                    return;
                }
            }
        } else {
            None
        };
        let Some(d) = self.state.borrow_mut().dialogs.pop_front() else {
            return;
        };
        match d.on {
            OnAnswer::Close => {}
            OnAnswer::Warning(r) | OnAnswer::Merge(r) => r.send(ok),
            OnAnswer::WarnHere => {
                if !ok {
                    self.quit(0);
                }
            }
            OnAnswer::Error => {
                if ok {
                    self.quit(1);
                }
            }
            OnAnswer::Fatal => self.quit(1),
            OnAnswer::PrematureExit => {
                if ok {
                    self.quit(0);
                }
            }
            OnAnswer::FirstRoot(r) => {
                if ok {
                    let dir = self.ui.get_root_dir().trim().to_owned();
                    let first = Clroot::ConnectLocal(Some(dir)).to_string();
                    self.state.borrow_mut().dialogs.push_front(Dialog {
                        kind: 3,
                        title: "Root selection".to_owned(),
                        message: "Please enter the second directory you want to synchronize."
                            .to_owned(),
                        ok: "OK",
                        cancel: "Cancel",
                        on: OnAnswer::SecondRoot(first, r),
                    });
                } else {
                    r.send(None);
                }
            }
            OnAnswer::SecondRoot(first, r) => {
                r.send(root.map(|second| (first, second)));
            }
        }
        if !self.quitting.get() {
            self.show_dialog();
        }
    }

    /// `getSecondRoot`'s `getRoot`: the root typed, or what is wrong with
    /// it.
    fn second_root(&self) -> Result<String, String> {
        let ui = &self.ui;
        let file = Some(ui.get_root_dir().trim().to_owned());
        let opt = |s: SharedString| Some(s.trim().to_owned()).filter(|s| !s.is_empty());
        let host = ui.get_root_host().trim().to_owned();
        let root = match ui.get_root_kind() {
            0 => return Ok(Clroot::ConnectLocal(file).to_string()),
            1 => Clroot::ConnectByShell {
                shell: "ssh".to_owned(),
                host,
                user: opt(ui.get_root_user()),
                port: opt(ui.get_root_port()),
                path: file,
            },
            _ => {
                let port = ui.get_root_port().to_string();
                if port.is_empty() {
                    return Err("Please enter a port".to_owned());
                }
                if port.parse::<u16>().is_err() {
                    return Err("The port you specify must be an integer".to_owned());
                }
                Clroot::ConnectBySocket {
                    host,
                    port,
                    path: file,
                }
            }
        };
        clroot::fix_host(root)
            .map(|r| r.to_string())
            .map_err(|e| format!("Something's wrong with the values you entered, try again.\n{e}"))
    }

    // ---- The session's events ----

    /// Takes an event from the session.
    pub fn handle(&self, e: Event) {
        match e {
            Event::Started(Some(profile)) => self.load_profile(&profile),
            Event::Started(None) => self.error(
                "Profiles can't be chosen in the window yet: \
                 name a profile, or two roots, on the command line.",
            ),
            Event::ProfileLoaded(settings) => {
                let after = {
                    let mut s = self.state.borrow_mut();
                    s.settings = Some(settings);
                    s.after_configure.take()
                };
                match after {
                    // `loadProfile`'s `connectRoots`.
                    None => self.send(Command::Connect),
                    Some(AfterConfigure::Nothing) => {}
                    Some(AfterConfigure::Synchronize) => {
                        self.state.borrow_mut().busy = false;
                        self.synchronize();
                    }
                }
            }
            Event::Connected(names) => {
                {
                    let mut s = self.state.borrow_mut();
                    s.roots = Some(names);
                    s.profile_loaded = true;
                }
                self.update_from_profile();
                self.detect();
            }
            Event::Contacting(msg) => {
                // `displayWaitMessage`.
                self.set_busy(true);
                self.status(&msg);
            }
            Event::Status(s) => self.set_status(&s),
            Event::Scanning(_) | Event::Merge { .. } | Event::Refused(_) => {}
            Event::Warning(w) => {
                // warnBox in batch mode: shown, and the session goes on.
                self.push_dialog(Dialog::message("Warning", &w, "Close", "", OnAnswer::Close))
            }
            Event::Ask(q) => self.ask(q),
            Event::Updates(u) => self.detected(u),
            Event::Item(i, item) => self.set_item(i, item),
            Event::Items { items, .. } => self.display_main(items),
            Event::Progress(p) => {
                // `showProgress`'s `updateRowStatus`.
                let changed = self
                    .model
                    .table
                    .borrow_mut()
                    .set_progress(p.item, &p.item_progress);
                self.model.changed(changed);
            }
            Event::Synchronized(summary) => {
                // `commitUpdates`.
                {
                    let mut s = self.state.borrow_mut();
                    s.summary = Some(summary);
                    s.after_commit = Some(AfterCommit::Sync);
                }
                self.status("Updating synchronizer state");
                self.send(Command::Commit);
            }
            Event::Committed => {
                let after = self.state.borrow_mut().after_commit.take();
                match after {
                    Some(AfterCommit::Sync) => self.synchronized(),
                    Some(AfterCommit::Detect) | None => {
                        {
                            let mut s = self.state.borrow_mut();
                            s.busy = false;
                            s.rescan = true;
                        }
                        self.refresh_groups();
                    }
                }
            }
            Event::Profiles(_)
            | Event::ProfileUnchanged(_)
            | Event::Changed
            | Event::Diff { .. }
            | Event::Interrupted => {}
            Event::Error(msg) => self.error(&msg),
            // The user chose to stop at a warning (`doExit`).
            Event::Exit(_) => self.quit(0),
        }
    }

    fn ask(&self, q: Question) {
        let d = match q {
            Question::Warning(w, r) => {
                Dialog::message("Warning", &w, "OK", "Quit", OnAnswer::Warning(r))
            }
            Question::Roots(r) => Dialog {
                kind: 2,
                title: "Root selection".to_owned(),
                message: try_again_message(),
                ok: "OK",
                cancel: "Cancel",
                on: OnAnswer::FirstRoot(r),
            },
            Question::Merge { title, text, reply } => Dialog::message(
                &title,
                &format!("Do you want to commit the changes to the replicas ?\n\n{text}"),
                "Yes",
                "No",
                OnAnswer::Merge(reply),
            ),
        };
        self.push_dialog(d);
    }

    /// `fatalErrorHandler`, or before the first items are shown, `start`'s
    /// `fatalError`: there is no profile manager to go back to yet, so the
    /// program ends.
    fn error(&self, msg: &str) {
        let started = self.state.borrow().started;
        let title = if started { "Error" } else { "Fatal error" };
        self.send(Command::Log(LogKind::Log, format!("{title}: {msg}\n")));
        {
            let mut s = self.state.borrow_mut();
            s.after_configure = None;
            s.after_commit = None;
            s.busy = false;
            s.rescan = s.profile_loaded;
        }
        self.refresh_groups();
        if started {
            self.status(&format!("Fatal error: {msg}"));
            self.push_dialog(Dialog::message(
                title,
                msg,
                "Close",
                "Quit",
                OnAnswer::Error,
            ));
        } else {
            self.push_dialog(Dialog::message(title, msg, "Quit", "", OnAnswer::Fatal));
        }
    }

    /// `loadProfile`.
    fn load_profile(&self, profile: &str) {
        self.status("Loading profile");
        {
            let mut s = self.state.borrow_mut();
            s.unsynchronized = None;
            s.profile_loaded = false;
            s.after_configure = None;
        }
        self.send(Command::LoadProfile(profile.to_owned()));
    }

    /// `updateFromProfile`: the title and profile's name
    /// (`displayNewProfileLabel`), the column headings and the Action
    /// menu's directions.
    fn update_from_profile(&self) {
        let s = self.state.borrow();
        let (Some(settings), Some(roots)) = (&s.settings, &s.roots) else {
            return;
        };
        let name = match (settings.profile.as_str(), settings.label.as_str()) {
            ("", _) => String::new(),
            (p, "") => p.to_owned(),
            ("default", l) => l.to_owned(),
            (p, l) => format!("{p} ({l})"),
        };
        let raw = settings.roots.join(" ↔ ");
        let raw = if raw.is_empty() {
            raw
        } else {
            format!("   |   {raw}")
        };
        self.ui.set_window_title(
            if name.is_empty() {
                my_name_capitalized()
            } else {
                format!("{} [{name}]{raw}", my_name_capitalized())
            }
            .into(),
        );
        self.ui.set_profile_text(
            if name.is_empty() {
                "No profile".to_owned()
            } else {
                format!("Profile: {name}")
            }
            .into(),
        );
        self.ui
            .set_replica1_heading(format!(" {} ", roots.narrow.0).into());
        self.ui
            .set_replica2_heading(format!(" {} ", roots.narrow.1).into());
        let (l1, l2) = &roots.hosts;
        let (ltr, rtl) = if l1 == l2 {
            ("Left to Right".to_owned(), "Right to Left".to_owned())
        } else {
            (format!("from {l1} to {l2}"), format!("from {l2} to {l1}"))
        };
        self.ui.set_left_to_right(ltr.into());
        self.ui.set_right_to_left(rtl.into());
    }

    /// `clearMainWindow`.
    fn clear_main_window(&self) {
        self.set_busy(true);
        self.model.table.borrow_mut().replace(Vec::new(), false);
        self.model.notify.reset();
        self.update_details();
    }

    /// `detectCmd`: finds the updates, if the profile has loaded.
    fn detect(&self) {
        self.ui.invoke_focus_table();
        if !self.state.borrow().profile_loaded {
            self.state.borrow_mut().rescan = true;
            self.set_busy(false);
            return;
        }
        self.clear_main_window();
        let paths = self.state.borrow().unsynchronized.clone();
        self.send(Command::Update {
            allow_partial: true,
            want_watcher: true,
            paths,
        });
    }

    /// The rest of `detectUpdatesAndReconcile`, then of `detectCmd`.
    fn detected(&self, u: Updates) {
        let empty = u.items.is_empty();
        let commit = empty && u.found_archives;
        self.status(if !empty {
            "Check and/or adjust selected actions; then press Go"
        } else if u.any_equal_updates {
            "Replicas have been changed only in identical ways since last sync"
        } else {
            "Everything is up to date"
        });
        let (confirm, batch) = {
            let mut s = self.state.borrow_mut();
            s.unsynchronized = Some((u.items.iter().map(|i| i.path.clone()).collect(), Vec::new()));
            s.started = true;
            s.go = !empty;
            s.rescan = !commit;
            s.busy = commit;
            let settings = s.settings.as_ref();
            (
                settings.is_some_and(|s| s.confirm_big_deletes),
                settings.is_some_and(|s| s.batch),
            )
        };
        self.display_main(u.items);
        if commit {
            self.state.borrow_mut().after_commit = Some(AfterCommit::Detect);
            self.send(Command::Commit);
        }
        match u.dangerous_message {
            Some(msg) if confirm => {
                self.configure(false, AfterConfigure::Nothing);
                self.push_dialog(Dialog::message(
                    "Warning",
                    &msg,
                    "OK",
                    "Quit",
                    OnAnswer::WarnHere,
                ));
            }
            // "Detect Updates and Proceed": once, without waiting.
            _ if batch && !empty => {
                self.configure(false, AfterConfigure::Synchronize);
                self.set_busy(true);
            }
            _ if batch => self.configure(false, AfterConfigure::Nothing),
            _ => {}
        }
    }

    /// `Prefs.set Globals.batch`, then `then`.
    fn configure(&self, batch: bool, then: AfterConfigure) {
        self.state.borrow_mut().after_configure = Some(then);
        self.send(Command::Configure(Overrides {
            batch: Some(batch),
            ..Overrides::default()
        }));
    }

    /// `displayMain`: shows `items`, keeping the rows selected, or
    /// selecting the first interesting one.
    fn display_main(&self, items: Vec<ItemView>) {
        let selected = {
            let mut t = self.model.table.borrow_mut();
            t.replace(items, true);
            t.selected_count()
        };
        self.model.notify.reset();
        if selected == 0 {
            self.next_interesting();
        }
        self.update_details();
    }

    /// An item has changed: its action set, or its propagation done
    /// (`redisplay`, `fastRedisplay`).
    fn set_item(&self, i: usize, item: ItemView) {
        let changed = self.model.table.borrow_mut().set_item(i, item);
        self.model.changed(changed);
        if self.model.table.borrow().is_selected(i) {
            self.update_details();
        }
    }

    // ---- Actions ----

    /// `doAction`: `a` on the item selected, then on to the next
    /// interesting one; or on every item selected.
    fn action(&self, a: SetAction) {
        if self.state.borrow().busy {
            return;
        }
        let (current, selected) = {
            let t = self.model.table.borrow();
            let possible = |i: &usize| t.row(*i).action_possible();
            (
                t.current().map(|i| (i, possible(&i))),
                t.selected()
                    .into_iter()
                    .filter(possible)
                    .collect::<Vec<_>>(),
            )
        };
        match current {
            Some((i, possible)) => {
                if possible {
                    self.send(Command::SetAction(i, a));
                }
                self.next_interesting();
            }
            None => {
                for i in selected {
                    self.send(Command::SetAction(i, a));
                }
                self.update_details();
            }
        }
    }

    /// `showDetCommand`: the errors of the item selected.
    fn details(&self) {
        let shown = {
            let t = self.model.table.borrow();
            t.current().map(|i| {
                let item = &t.row(i).item;
                let path = &item.path_string;
                let text = match &item.problem {
                    Some(err) => err.clone(),
                    None => {
                        let failure = match &item.outcome {
                            Some(session::Outcome::Failed(e)) => vec![e.clone()],
                            _ => Vec::new(),
                        };
                        let prefix = |p: &str, l: &[String]| {
                            l.iter().map(|e| format!("{p}{e}\n")).collect::<Vec<_>>()
                        };
                        failure
                            .into_iter()
                            .chain(prefix("[root 1]: ", &item.errors1))
                            .chain(prefix("[root 2]: ", &item.errors2))
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                };
                (format!("Errors for file {path}"), text)
            })
        };
        if let Some((title, text)) = shown {
            self.push_dialog(Dialog::message(&title, &text, "Close", "", OnAnswer::Close));
        }
    }

    // ---- Syncing ----

    /// `synchronize`.
    fn synchronize(&self) {
        if self.state.borrow().busy {
            return;
        }
        if self.model.table.borrow().is_empty() {
            self.status("Nothing to synchronize");
            return;
        }
        self.set_busy(true);
        self.status("Propagating changes");
        self.send(Command::Synchronize);
    }

    /// The end of `synchronize`, once committed.
    fn synchronized(&self) {
        let summary = self.state.borrow().summary.clone().unwrap_or_default();
        let count = |n: usize, what: &str| match n {
            0 => None,
            n => Some(format!("{n} {what}")),
        };
        let failures = match summary.failed.len() {
            0 => None,
            1 => Some("1 failure".to_owned()),
            n => Some(format!("{n} failures")),
        };
        let parts: Vec<String> = [
            failures,
            count(summary.partial.len(), "partially transferred"),
            count(summary.skipped.len(), "skipped"),
            count(summary.not_started, "not started"),
        ]
        .into_iter()
        .flatten()
        .collect();
        self.status(&format!(
            "Synchronization complete         {}",
            parts.join(", ")
        ));
        {
            let mut s = self.state.borrow_mut();
            s.unsynchronized = Some((summary.unsynchronized.clone(), Vec::new()));
            s.busy = false;
            s.rescan = true;
        }
        self.refresh_groups();
    }

    // ---- Quitting ----

    /// `safeExit`: ends the program, after asking if it is working.
    fn safe_exit(&self) {
        let asking = self
            .state
            .borrow()
            .dialogs
            .iter()
            .any(|d| matches!(d.on, OnAnswer::PrematureExit));
        if asking {
            return;
        }
        if !self.state.borrow().busy {
            return self.quit(0);
        }
        self.push_dialog(Dialog::message(
            "Premature exit",
            &format!("{} is working, exit anyway ?", my_name_capitalized()),
            "Yes",
            "No",
            OnAnswer::PrematureExit,
        ));
    }

    /// Ends the program with `code`: the questions waiting are answered
    /// no, and the session ends (once what it was doing is done).
    pub fn quit(&self, code: i32) {
        if self.quitting.replace(true) {
            return;
        }
        let dialogs = {
            let mut s = self.state.borrow_mut();
            s.exit = Some(code);
            std::mem::take(&mut s.dialogs)
        };
        drop(dialogs);
        self.ui.set_dialog(0);
        if let Some(s) = self.session.borrow_mut().take() {
            // What it is doing stops at its next wait (a sync, between
            // items), rather than holding up the end.
            s.send(Command::Interrupt);
            let _ = s.join();
        }
        let _ = self.ui.hide();
        let _ = slint::quit_event_loop();
    }

    // ---- The window's callbacks ----

    fn wire(self: &Rc<Self>) {
        let ui = &self.ui;
        let weak = Rc::downgrade(self);
        let with = move |f: &dyn Fn(&App)| {
            if let Some(s) = weak.upgrade() {
                f(&s);
            }
        };

        let w = with.clone();
        ui.on_row_clicked(move |i, shift, command| {
            w(&|s| {
                let changed = s
                    .model
                    .table
                    .borrow_mut()
                    .click(i as usize, ClickMods { shift, command });
                s.model.changed(changed);
                s.update_details();
            })
        });
        let w = with.clone();
        ui.on_move_cursor(move |delta, extend| {
            w(&|s| {
                let (cursor, changed) = s
                    .model
                    .table
                    .borrow_mut()
                    .move_cursor(delta as isize, extend);
                s.model.changed(changed);
                if let Some(c) = cursor {
                    s.ui.invoke_bring_into_view(c as i32);
                }
                s.update_details();
            })
        });
        let w = with.clone();
        ui.on_action(move |name| {
            w(&|s| {
                // Native menus' items bypass Slint's focus: none while a
                // dialog is up.
                if s.ui.get_dialog() != 0 {
                    return;
                }
                let a = match name.as_str() {
                    "right" => SetAction::Right,
                    "left" => SetAction::Left,
                    "skip" => SetAction::Conflict,
                    "merge" => SetAction::Merge,
                    "first" => SetAction::Prefer(Favor::First),
                    "second" => SetAction::Prefer(Favor::Second),
                    "newer" => SetAction::Prefer(Favor::Newer),
                    "older" => SetAction::Prefer(Favor::Older),
                    "force-newer" => SetAction::Newer,
                    "force-older" => SetAction::Older,
                    "revert" => SetAction::Revert,
                    "merge-default" => SetAction::MergeDefault,
                    _ => return,
                };
                s.action(a);
            })
        });
        let w = with.clone();
        ui.on_select_all(move || {
            w(&|s| {
                if s.ui.get_dialog() != 0 || !s.ui.get_select_enabled() {
                    return;
                }
                let changed = s.model.table.borrow_mut().select_all();
                s.model.changed(changed);
                s.update_details();
            })
        });
        let w = with.clone();
        ui.on_deselect_all(move || {
            w(&|s| {
                if s.ui.get_dialog() != 0 || !s.ui.get_select_enabled() {
                    return;
                }
                let changed = s.model.table.borrow_mut().deselect_all();
                s.model.changed(changed);
                s.update_details();
            })
        });
        let w = with.clone();
        ui.on_details(move || {
            w(&|s| {
                if s.ui.get_dialog() == 0 && s.ui.get_detail_enabled() {
                    s.details();
                }
            })
        });
        let w = with.clone();
        ui.on_go(move || {
            w(&|s| {
                if s.ui.get_dialog() == 0 && s.ui.get_go_enabled() {
                    s.synchronize();
                }
            })
        });
        let w = with.clone();
        ui.on_quit(move || w(&|s| s.safe_exit()));
        let w = with.clone();
        ui.on_dialog_done(move |ok| w(&|s| s.dialog_done(ok)));
        let w = with.clone();
        ui.on_browse_root(move || {
            w(&|s| {
                // With a parent window this is a sheet on macOS, so the
                // event loop (and the session's events) keeps running.
                let dialog = rfd::AsyncFileDialog::new()
                    .set_title("Select a local directory")
                    .set_parent(&s.ui.window().window_handle())
                    .pick_folder();
                let ui = s.ui.as_weak();
                task::spawn(dialog, move |picked| {
                    if let Some(path) = picked.map(|h| h.path().display().to_string()) {
                        let _ = ui.upgrade_in_event_loop(move |ui| ui.set_root_dir(path.into()));
                    }
                });
            })
        });
        let w = with;
        ui.window().on_close_requested(move || {
            w(&|s| s.safe_exit());
            slint::CloseRequestResponse::KeepWindowShown
        });
    }
}

fn run(args: Vec<String>) -> Result<i32, slint::PlatformError> {
    let app = App::new(Startup::new(args))?;
    app.ui.invoke_focus_table();
    let _ticks = app.start_ticks();
    app.ui.run()?;
    // The window was closed some other way: as Quit.
    app.quit(0);
    Ok(app.exit_code().unwrap_or(ExitCode::Perfect as i32))
}

/// Runs the program on its command line, with the window as its
/// graphical interface: the exit code.
pub fn main() -> std::process::ExitCode {
    gemel::main(std::env::args().skip(1).collect(), Some(&graphical))
}

/// The graphical interface on `args`: the exit code.
fn graphical(args: Vec<String>) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("gemel-gui: {e}");
            ExitCode::Fatal as i32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_are_padded_as_uigtk3_pads_them() {
        assert_eq!(
            format_status("Loading profile", ""),
            "Loading profile               "
        );
        assert_eq!(
            format_status("Propagating changes", "f"),
            "Propagating changes           f"
        );
        assert_eq!(my_name_capitalized(), "Gemel");
    }
}
