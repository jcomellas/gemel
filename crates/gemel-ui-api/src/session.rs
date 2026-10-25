//! The command/event contract between engine and frontend.
//!
//! Ports Unison `src/uimacbridge.ml`, with what `src/uitext.ml` needs of
//! the same steps.
//!
//! Derived from Unison (`src/uimacbridge.ml` carries no copyright line of its
//! own; Unison is Copyright Benjamin C. Pierce and other contributors).
//!
//! # How a frontend uses it
//!
//! [`spawn`] starts a session on the engine thread, which owns the client,
//! the installed roots and the items. The frontend sends [`Command`]s and
//! receives [`Event`]s, which carry snapshots of the items ([`ItemView`]),
//! not the engine's own values. Every command but [`Command::Abort`] and
//! [`Command::Log`] ends with one reply event, named on the command;
//! [`Event::Error`] replaces it when the command fails. Events in between
//! report status and progress, and [`Event::Ask`] puts a question to the
//! user, with the channel for the answer. A command refused because a sync
//! is running gets [`Event::Refused`], which is never a reply: the sync's
//! is still to come.
//!
//! The steps are those both interfaces take, and what they do differently
//! is left to them: the session shows no status text of its own beyond
//! what the engine shows, and logs no summary. A text interface's batch
//! run, say, is [`Command::Start`], [`Command::LoadProfile`],
//! [`Command::Update`], then [`Command::Synchronize`] unless every item is
//! skipped, and [`Command::Commit`].
//!
//! # While a sync runs
//!
//! [`Command::Synchronize`] propagates the items while the session goes
//! on reading commands, so that [`Command::Abort`] gets through, even
//! while the sync waits for the user to confirm a merge. Any other command
//! is refused ([`Event::Refused`]) until the sync ends. (The plan had the sync as a task of its
//! own; here it is a future polled alongside the commands, which does the
//! same without sharing the session.)
//!
//! # Warnings
//!
//! The engine warns through a synchronous callback, which can't wait for
//! the user. So warnings are kept, and the session asks about them at the
//! end of the step that gave them (a sync's and a commit's too), before
//! anything is committed; and, while finding updates, once the archives
//! are loaded, before the scan. In batch
//! mode they are only shown ([`Event::Warning`]); otherwise the user
//! decides whether to go on ([`Question::Warning`]), and if not, the
//! archives are unlocked and the session ends ([`Event::Exit`]).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Poll;
use std::time::Instant;

use gemel_base::case;
use gemel_base::path::{Global, Path};
use gemel_base::prefs::BoolWithDefault;
use gemel_base::trace::{self, Trace};
use gemel_base::util::{self, Error, FileId};
use gemel_engine::abort::Abort;
use gemel_engine::common::{self, Direction, ReconItem, Replicas, Status};
use gemel_engine::recon::{self, Choice, Strength};
use gemel_engine::transport::{self, Task};
use gemel_engine::{archive, globals, sortri, update, uutil, with_trace};
use gemel_fs::fileinfo::Typ;
use gemel_fs::props::Filesize;
use gemel_rpc::runtime::{Commands, EngineThread, Events};
use tokio::sync::{mpsc, oneshot};

use crate::cli;
use crate::exit_code::ExitCode;
use crate::uicommon::startup::{ProfileInfo, Ui, client_rpc};
use crate::uicommon::stats::Stats;
use crate::uicommon::{self, Action, Format, Repeat};

// ---- The contract ----

/// How to start a session.
pub struct Config {
    /// The Unison directory, for profiles, archives and the log.
    pub unison_dir: PathBuf,
    /// The command line, without the program's name.
    pub args: Vec<String>,
    /// The client's trace: where it prints, and whether log entries go to
    /// stderr too (`send_log_msgs_to_stderr`).
    pub trace: Trace,
    /// Root commands run on a separate host, marshalled, as on a server.
    pub loopback: bool,
    /// Status messages (and timers) come as [`Event::Status`]; otherwise
    /// the trace shows them itself.
    pub status_events: bool,
    /// The path being scanned comes as [`Event::Scanning`], now and then.
    pub scan_status: bool,
    /// Warnings are logged, as the Mac interface logs them.
    pub log_warnings: bool,
    /// The message that the roots are being connected goes to stderr,
    /// through the trace (unless `silent`), as the text interface shows
    /// it; otherwise it comes as [`Event::Contacting`].
    pub print_wait_message: bool,
    /// Once an item has been propagated, its progress is made up to its
    /// length, as the Mac interface does; otherwise only once it has
    /// failed, as the text interface does (`doTransport`).
    pub complete_progress: bool,
    /// Where the bytes the client's connections write and read are
    /// counted, as they go (`Remote.emittedBytes`, `Remote.receivedBytes`),
    /// for a frontend's statistics.
    pub traffic: Option<Arc<Traffic>>,
}

pub use gemel_rpc::host::Traffic;

/// What a frontend asks the session to do.
#[derive(Debug, Clone)]
pub enum Command {
    /// `uiInitClRootsAndProfile`: the profile the command line names.
    /// Replies [`Event::Started`]. (The other profiles are left alone, as
    /// the text interface leaves them, unless the frontend asks for them.)
    Start,
    /// `scanProfiles`: every profile, with its roots, label and key.
    /// Replies [`Event::Profiles`].
    ScanProfiles,
    /// `unisonProfileInit`: makes the profile `name`, with these roots.
    /// Replies [`Event::Profiles`].
    NewProfile {
        /// Its name.
        name: String,
        /// Its first root.
        root1: String,
        /// Its second.
        root2: String,
    },
    /// `initPrefs`: loads a profile, and the command line over it (asking
    /// for roots if neither gives them). Replies [`Event::ProfileLoaded`].
    LoadProfile(String),
    /// `Prefs.profileUnchanged`: whether the files the profile was read
    /// from are as they were, so that it needn't be loaded again. Replies
    /// [`Event::ProfileUnchanged`].
    ProfileUnchanged,
    /// Overrides some of the profile's settings, as `-silent` and
    /// `-repeat` imply others. Replies [`Event::ProfileLoaded`].
    Configure(Overrides),
    /// `connectRoots`: connects the roots if they aren't, and sends them
    /// the preferences. Replies [`Event::Connected`].
    Connect,
    /// Connects the roots if they aren't, finds the updates and reconciles
    /// them; commits at once if there were no archives. Replies
    /// [`Event::Updates`].
    Update {
        /// Keep errors deep in a directory in its item (`allowPartial`), as
        /// the text interface does.
        allow_partial: bool,
        /// Start the file watcher on the replicas if the scan is a whole
        /// one (`wantWatcher`).
        want_watcher: bool,
        /// The paths left unsynchronised, and those whose retry is delayed
        /// (`subpaths`). With a watcher running since the last whole scan,
        /// only these and what the watcher reported are scanned; otherwise
        /// everything is.
        paths: update::Subpaths,
        /// Scan only these, as if `paths` named them, then set `paths` back
        /// (`uigtk3`'s "Recheck Unsynchronized Items").
        only: Option<Vec<Path<Global>>>,
    },
    /// `waitForChanges` on every root: replies [`Event::Changed`] once the
    /// file watcher reports a change to any replica. [`Command::Interrupt`]
    /// stops the wait.
    WaitForChanges,
    /// Changes an item's action. Replies [`Event::Item`].
    SetAction(usize, SetAction),
    /// Propagates every item not propagated yet: all but the deletions,
    /// then the deletions. Replies [`Event::Synchronized`].
    Synchronize,
    /// `Abort.all`: stops a sync; what is copying stops at its next check,
    /// and nothing more starts. No reply. (Taken during a sync.)
    Abort,
    /// `commitUpdates`: saves the archives. Replies [`Event::Committed`].
    Commit,
    /// `addIgnorePattern`: ignores these items' paths, names or extensions
    /// from now on (in the profile too), each pattern once and in order
    /// (`uigtk3`'s `addRegExpByPath`), and drops the items that are now
    /// ignored. Replies [`Event::Items`].
    Ignore(Vec<usize>, IgnoreKind),
    /// `showDiffs` on an item. Replies [`Event::Diff`].
    Diff(usize),
    /// Keeps only these items, in this order: the text interface's list
    /// once the user has been through it, with some dropped for this
    /// session and the order changed. Replies [`Event::Items`].
    Arrange(Vec<usize>),
    /// Changes `Sortri`'s settings, and sorts the items again
    /// (`sortReconItems`). Replies [`Event::Items`].
    Sort(SortKind),
    /// `Sys.Break`: stops the command running at its next wait, including
    /// one waiting for an answer, or a sync. Work already handed to other
    /// threads (a directory being scanned) finishes first. Replies
    /// [`Event::Interrupted`]. The frontend then ends the session, which
    /// releases the locks held. (Taken at any time.)
    Interrupt,
    /// Writes to the log, or shows a status. No reply. (Taken during a
    /// sync.)
    Log(LogKind, String),
}

/// A new action for an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetAction {
    /// From the second replica to the first (`unisonRiSetLeft`).
    Left,
    /// From the first to the second (`unisonRiSetRight`).
    Right,
    /// Skip it (`unisonRiSetConflict`).
    Skip,
    /// Merge it (`unisonRiSetMerge`).
    Merge,
    /// The older version wins (`unisonRiForceOlder`).
    Older,
    /// The newer version wins (`unisonRiForceNewer`).
    Newer,
    /// Back to the default (`unisonRiRevert`).
    Revert,
    /// The older version wins if the item is a conflict, or its default is
    /// one; nothing otherwise (`setDirectionIfConflict`).
    OlderIfConflict,
    /// The same, for the newer version.
    NewerIfConflict,
    /// One version wins if the item's default is a conflict; nothing
    /// otherwise (`Recon.setDirection` with `` `Prefer ``): `uigtk3`'s
    /// "Resolve Conflicts in Favor of …".
    Prefer(Favor),
    /// Merge it, if it is a path to merge (`merge`); nothing otherwise
    /// (`Recon.setDirection` with `` `Merge `` and `` `Force ``): `uigtk3`'s
    /// "Revert to the Merging Default, if Available".
    MergeDefault,
    /// Skip it, with no reason given (`uigtk3`'s `questionAction`).
    Conflict,
}

/// Which version [`SetAction::Prefer`] lets win.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Favor {
    /// The first replica's.
    First,
    /// The second's.
    Second,
    /// The newer.
    Newer,
    /// The older.
    Older,
}

/// How [`Command::Sort`] sorts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKind {
    /// By name (`sortByName`).
    Name,
    /// By size (`sortBySize`).
    Size,
    /// New files first, or not, in turn (`sortNewFirst`).
    NewFirst,
    /// As the profile has it (`restoreDefaultSettings`).
    Default,
}

/// What to ignore of an item's path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoreKind {
    /// The path (`ignorePath`).
    Path,
    /// Its name, anywhere (`ignoreName`).
    Name,
    /// Its extension, anywhere (`ignoreExt`).
    Ext,
}

/// How [`Command::Log`] logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    /// `Trace.log`: the log, and stderr if log entries go there.
    Log,
    /// `Trace.log_color`: the same, without colours in the file.
    LogColor,
    /// `Trace.logonly`: the log only.
    LogOnly,
    /// `Trace.status`: a status message.
    Status,
    /// `Trace.message`: a message, as the timers show theirs.
    Message,
}

/// What the session tells the frontend.
#[derive(Debug)]
pub enum Event {
    /// [`Command::Start`]'s reply: the profile the command line names, if
    /// any (roots alone name `default`); `None` for the user to choose one.
    Started(Option<String>),
    /// The profiles found.
    Profiles(Vec<(String, ProfileInfo)>),
    /// [`Command::LoadProfile`]'s and [`Command::Configure`]'s reply.
    ProfileLoaded(Settings),
    /// [`Command::ProfileUnchanged`]'s reply.
    ProfileUnchanged(bool),
    /// [`Command::Connect`]'s reply: the roots' names.
    Connected(RootNames),
    /// The roots are being connected (`contactingServerMsg`).
    Contacting(String),
    /// A status message, or a timer's (with [`Config::status_events`]).
    Status(String),
    /// The path being scanned, now and then; `""` when the scan is done
    /// (with [`Config::scan_status`]).
    Scanning(String),
    /// A warning, in batch mode.
    Warning(String),
    /// A question for the user.
    Ask(Question),
    /// [`Command::Update`]'s reply.
    Updates(Updates),
    /// An item has changed: [`Command::SetAction`]'s reply, and each item
    /// a sync has finished with.
    Item(usize, ItemView),
    /// [`Command::Ignore`]'s reply: the items left, and the index of the
    /// first one at or after the first ignored (`unisonUpdateForIgnore`).
    /// Also [`Command::Arrange`]'s and [`Command::Sort`]'s, from 0.
    Items {
        /// The items.
        items: Vec<ItemView>,
        /// Where to go on from.
        next: usize,
    },
    /// Progress during a sync.
    Progress(Progress),
    /// A merge's result, when it needn't be confirmed.
    Merge {
        /// "Results of merging …".
        title: String,
        /// What the merge program printed.
        text: String,
    },
    /// [`Command::Synchronize`]'s reply.
    Synchronized(Summary),
    /// [`Command::Commit`]'s reply.
    Committed,
    /// [`Command::WaitForChanges`]'s reply.
    Changed,
    /// [`Command::Diff`]'s reply: the diffs shown (title and text), and
    /// why some couldn't be.
    Diff {
        /// Titles and diffs.
        shown: Vec<(String, String)>,
        /// The errors.
        errors: Vec<String>,
    },
    /// A command failed, with this message (`exn2string`).
    Error(String),
    /// A command was refused, because a sync is running: not a reply.
    Refused(Command),
    /// [`Command::Interrupt`]'s reply: the command running, if any, was
    /// stopped, and sends no reply of its own.
    Interrupted,
    /// The session has ended: the user chose to stop at a warning.
    Exit(ExitCode),
}

/// A question, with the channel for the answer. Dropping it answers no.
#[derive(Debug)]
pub enum Question {
    /// No roots in the profile or on the command line: which?
    Roots(Reply<Option<(String, String)>>),
    /// A warning: go on (`true`), or stop?
    Warning(String, Reply<bool>),
    /// A merge's result (with `confirmmerge`): keep it?
    Merge {
        /// "Results of merging …".
        title: String,
        /// What the merge program printed.
        text: String,
        /// The answer.
        reply: Reply<bool>,
    },
}

/// Where to send the answer to a [`Question`].
#[derive(Debug)]
pub struct Reply<T>(oneshot::Sender<T>);

impl<T> Reply<T> {
    /// Answers.
    pub fn send(self, answer: T) {
        let _ = self.0.send(answer);
    }
}

/// Settings to override ([`Command::Configure`]); `None` leaves one as it
/// is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    /// `batch`.
    pub batch: Option<bool>,
    /// `terse`.
    pub terse: Option<bool>,
    /// `auto`.
    pub auto: Option<bool>,
    /// Log entries also go to stderr (`Trace.sendLogMsgsToStderr`).
    pub log_to_stderr: Option<bool>,
}

/// The roots' names, for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootNames {
    /// `roots2niceStrings 32`.
    pub short: (String, String),
    /// `roots2niceStrings 15`: `uigtk3`'s column headings.
    pub narrow: (String, String),
    /// The ends of their paths that differ (`Fspath.differentSuffix`), or
    /// `local` and the host's name, in full.
    pub full: (String, String),
    /// `roots2string`: the headings of the columns.
    pub headings: String,
    /// The hosts' names (`root2hostname`): `local`, or the server's.
    pub hosts: (String, String),
}

/// The settings a frontend needs, once a profile is loaded.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// The profile's name.
    pub profile: String,
    /// Its file (`profilePathname`).
    pub profile_path: PathBuf,
    /// The roots, as given.
    pub roots: Vec<String>,
    /// `batch`.
    pub batch: bool,
    /// `auto`.
    pub auto: bool,
    /// `terse`.
    pub terse: bool,
    /// `expert`.
    pub expert: bool,
    /// `repeat`.
    pub repeat: Repeat,
    /// `retry`.
    pub retry: i64,
    /// `confirmmerge`.
    pub confirm_merge: bool,
    /// `confirmbigdeletes`.
    pub confirm_big_deletes: bool,
    /// `label`.
    pub label: String,
    /// `silent`.
    pub silent: bool,
    /// `dumbtty`.
    pub dumbtty: bool,
    /// `color`.
    pub color: BoolWithDefault,
    /// `debug`: the modules being debugged.
    pub debug: Vec<String>,
    /// `selftest`.
    pub run_tests: bool,
    /// `testserver`.
    pub test_server: bool,
    /// `timers`.
    pub timers: bool,
}

/// [`Command::Update`]'s result.
#[derive(Debug, Clone)]
pub struct Updates {
    /// The items, sorted.
    pub items: Vec<ItemView>,
    /// Some paths were changed the same way in both replicas.
    pub any_equal_updates: bool,
    /// Paths whose whole contents are being deleted in one replica.
    pub dangerous: Vec<Path<Global>>,
    /// What to tell the user about them (`dangerousPathMsg`), if any.
    pub dangerous_message: Option<String>,
    /// There were archives (`Update.foundArchives`).
    pub found_archives: bool,
    /// The roots' names.
    pub roots: RootNames,
}

/// An item, as a frontend shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemView {
    /// Its path (in the first replica).
    pub path: Path<Global>,
    /// The path, for display.
    pub path_string: String,
    /// What happened in each replica: `Created`, `Modified`, `Deleted`,
    /// `PropsChanged` (also for a move), or nothing (`unisonRiToLeft`).
    pub left: &'static str,
    /// The same, for the second replica.
    pub right: &'static str,
    /// What will be done.
    pub action: Action,
    /// The two replicas conflict (the default is to skip it).
    pub conflict: bool,
    /// The action isn't the default.
    pub changed_from_default: bool,
    /// The two versions can be diffed.
    pub can_diff: bool,
    /// How much propagating it transfers (`riLength`).
    pub length: Filesize,
    /// `riFileType`.
    pub file_type: &'static str,
    /// The item on a line (`reconItem2string`), as the text interface
    /// lists it.
    pub line: String,
    /// What changed, in detail (`details2string`).
    pub details: String,
    /// A problem, or a conflict: it won't be propagated.
    pub problematic: bool,
    /// Some errors deep inside.
    pub partially_problematic: bool,
    /// A deletion.
    pub deletion: bool,
    /// Bytes transferred so far.
    pub transferred: Filesize,
    /// How far it has got (`unisonRiToProgress`): `start`, a percentage,
    /// `done` or `FAILED`.
    pub progress: String,
    /// How it went, once propagated.
    pub outcome: Option<Outcome>,
    /// What the text interface shows of it (`reconItem2stringList` from
    /// the empty path): the first replica's short status, ...
    pub status1: &'static str,
    /// ... the second's, ...
    pub status2: &'static str,
    /// ... and the path, with a move's other name.
    pub shown_path: String,
    /// A problem found during update detection, if it is one.
    pub problem: Option<String>,
    /// Why it is skipped, if it is: a problem, or a conflict.
    pub skip_reason: Option<String>,
    /// The errors deep inside, in the first replica.
    pub errors1: Vec<String>,
    /// The same, in the second.
    pub errors2: Vec<String>,
    /// Only properties changed, in one replica or both (the text
    /// interface's `ispropschanged`).
    pub props_only: bool,
    /// The contents changed in one replica and only properties in the
    /// other: the action that keeps the contents (`setdirchanged`).
    pub favor_changed: Option<SetAction>,
}

/// How propagating an item went (`Util.confirmation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It did.
    Succeeded,
    /// It failed, for this reason.
    Failed(String),
}

/// Progress during a sync.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// The item.
    pub item: usize,
    /// Its bytes transferred so far.
    pub item_transferred: Filesize,
    /// What it transfers.
    pub item_length: Filesize,
    /// How far it has got (see [`ItemView::progress`]).
    pub item_progress: String,
    /// All the bytes transferred so far.
    pub transferred: Filesize,
    /// All the bytes to transfer.
    pub length: Filesize,
    /// The items finished (not counting those skipped).
    pub items_done: usize,
    /// The items to propagate (not counting those skipped).
    pub items: usize,
    /// The rate, in bytes per second (`Stats.avgRate1`): `NaN` until
    /// known.
    pub rate: f64,
    /// The time left, or `--:--`.
    pub eta: String,
}

/// How a sync went.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Summary {
    /// The items propagated (not counting those skipped or failed).
    pub transferred: usize,
    /// The items skipped, with why: a problem, or a conflict.
    pub skipped: Vec<(usize, String)>,
    /// Those that failed, with why.
    pub failed: Vec<(usize, String)>,
    /// Those propagated with errors deep inside left behind.
    pub partial: Vec<usize>,
    /// Those not started, because everything was aborted.
    pub not_started: usize,
    /// Everything was aborted.
    pub aborted: bool,
    /// The paths still to sync (failed, partial or skipped), for the next
    /// [`Command::Update`] (`unsynchronizedPaths`).
    pub unsynchronized: Vec<Path<Global>>,
}

impl Summary {
    /// `Uicommon.exitCode`, as the text interface computes it.
    pub fn exit_code(&self) -> ExitCode {
        ExitCode::from_outcome(
            !self.skipped.is_empty() || !self.partial.is_empty() || self.not_started > 0,
            !self.failed.is_empty(),
        )
    }
}

/// A running session, as a frontend holds it.
pub type Handle = EngineThread<Command, Event>;

/// Starts a session on its own engine thread.
pub fn spawn(config: Config) -> std::io::Result<Handle> {
    EngineThread::spawn(move |commands, events| run(config, commands, events))
}

/// Runs a session, on the engine thread, until the commands end or the
/// user stops at a warning.
pub async fn run(config: Config, mut commands: Commands<Command>, events: Events<Event>) {
    let mut s = match Session::new(config, events.clone()) {
        Ok(s) => s,
        Err(e) => {
            let _ = events.send(Event::Error(uicommon::exn2string(&e)));
            return;
        }
    };
    let mut pending = VecDeque::new();
    loop {
        let c = match pending.pop_front() {
            Some(c) => c,
            None => match commands.recv().await {
                Some(c) => c,
                None => return,
            },
        };
        let r = match c {
            Command::Interrupt => Err(Stop::Interrupted),
            Command::Synchronize => s.synchronize(&mut commands).await,
            c => interruptible(s.command(c), &mut commands, &mut pending)
                .await
                .unwrap_or(Err(Stop::Interrupted)),
        };
        match r {
            Ok(()) => {}
            Err(Stop::Error(e)) => s.send(Event::Error(uicommon::exn2string(&e))),
            Err(Stop::Interrupted) => s.send(Event::Interrupted),
            Err(Stop::Exit) => {
                s.send(Event::Exit(ExitCode::Fatal));
                return;
            }
        }
    }
}

/// Runs `step`, taking the commands that come meanwhile: an interrupt
/// drops it at its next wait (`None`), and the others wait in `pending`.
/// It is started all the same, so that one sent before the interrupt is
/// done if it doesn't wait.
async fn interruptible<T>(
    step: impl Future<Output = T>,
    commands: &mut Commands<Command>,
    pending: &mut VecDeque<Command>,
) -> Option<T> {
    let mut step = pin!(step);
    let mut started = false;
    let mut open = true;
    std::future::poll_fn(|cx| {
        if !started {
            started = true;
            if let Poll::Ready(r) = step.as_mut().poll(cx) {
                return Poll::Ready(Some(r));
            }
        }
        while open {
            match commands.poll_recv(cx) {
                Poll::Ready(Some(Command::Interrupt)) => return Poll::Ready(None),
                Poll::Ready(Some(c)) => pending.push_back(c),
                Poll::Ready(None) => open = false,
                Poll::Pending => break,
            }
        }
        step.as_mut().poll(cx).map(Some)
    })
    .await
}

// ---- The session ----

/// Why a command stopped.
enum Stop {
    /// It failed.
    Error(Error),
    /// The user chose to stop at a warning.
    Exit,
    /// [`Command::Interrupt`].
    Interrupted,
}

impl From<Error> for Stop {
    fn from(e: Error) -> Stop {
        Stop::Error(e)
    }
}

/// The `paths` preference, set for a while: set back when dropped.
struct PathsSetBack {
    host: Rc<gemel_rpc::host::Host>,
    saved: Vec<Path<Global>>,
}

impl PathsSetBack {
    fn new(host: &Rc<gemel_rpc::host::Host>, paths: Vec<Path<Global>>) -> PathsSetBack {
        let mut prefs = host.prefs().borrow_mut();
        let saved = prefs.read(&globals::PATHS).clone();
        prefs.set(&globals::PATHS, paths);
        PathsSetBack {
            host: host.clone(),
            saved,
        }
    }
}

impl Drop for PathsSetBack {
    fn drop(&mut self) {
        let saved = std::mem::take(&mut self.saved);
        self.host.prefs().borrow_mut().set(&globals::PATHS, saved);
    }
}

/// `stateItem`: an item, and how its propagation goes.
struct Item {
    ri: ReconItem,
    transferred: Filesize,
    to_transfer: Filesize,
    outcome: Option<Outcome>,
    status: Option<String>,
    started: bool,
}

/// A sync's totals, for its progress.
struct Totals {
    transferred: Filesize,
    length: Filesize,
    items_done: usize,
    items: usize,
    stats: Stats,
    shown: Option<Instant>,
}

struct Session {
    ui: Ui,
    events: Events<Event>,
    scan_status: bool,
    log_warnings: bool,
    print_wait_message: bool,
    complete_progress: bool,
    profile: String,
    items: RefCell<Vec<Item>>,
    warnings: Rc<RefCell<Vec<String>>>,
    totals: RefCell<Option<Totals>>,
    progress: RefCell<Option<mpsc::UnboundedReceiver<(FileId, Filesize)>>>,
}

impl Session {
    fn new(config: Config, events: Events<Event>) -> Result<Session, Error> {
        let mut trace = config.trace;
        if config.status_events {
            let ev = events.clone();
            trace.set_displayer(Some(Box::new(move |m: &str| {
                let _ = ev.send(Event::Status(m.to_owned()));
            })));
        }
        let rpc = client_rpc(&config.unison_dir, trace, config.loopback)?;
        if let Some(t) = config.traffic {
            rpc.client_host().set_traffic(t);
        }
        // The engine warns synchronously, often holding the preferences:
        // kept for the end of the step. A loopback server's warnings too
        // (a real server's will come with remote roots).
        let warnings = Rc::new(RefCell::new(Vec::new()));
        for host in [rpc.client_host(), rpc.local_host()] {
            let w = warnings.clone();
            host.set_warn_printer(Some(Rc::new(move |s: &str| {
                w.borrow_mut().push(s.to_owned())
            })));
        }
        Ok(Session {
            ui: Ui::new(rpc, config.args),
            events,
            scan_status: config.scan_status,
            log_warnings: config.log_warnings,
            print_wait_message: config.print_wait_message,
            complete_progress: config.complete_progress,
            profile: String::new(),
            items: RefCell::new(Vec::new()),
            warnings,
            totals: RefCell::new(None),
            progress: RefCell::new(None),
        })
    }

    fn send(&self, e: Event) {
        let _ = self.events.send(e);
    }

    fn rpc(&self) -> &gemel_rpc::registry::Rpc {
        self.ui.rpc()
    }

    fn roots(&self) -> Result<&globals::Roots, Error> {
        self.ui
            .roots()
            .ok_or_else(|| Error::Fatal("The roots are not connected".to_owned()))
    }

    fn item_ri(&self, i: usize) -> Result<ReconItem, Error> {
        self.items
            .borrow()
            .get(i)
            .map(|it| it.ri.clone())
            .ok_or_else(|| Error::Fatal(format!("There is no item {i}")))
    }

    async fn command(&mut self, c: Command) -> Result<(), Stop> {
        match c {
            Command::Start => {
                let profile = self
                    .ui
                    .ui_init_cl_roots_and_profile()
                    .map_err(Error::Fatal)?;
                self.ask_warnings().await?;
                self.send(Event::Started(profile));
            }
            Command::ScanProfiles => {
                self.ui.scan_profiles()?;
                self.ask_warnings().await?;
                self.send(Event::Profiles(self.ui.profiles_and_roots.clone()));
            }
            Command::NewProfile { name, root1, root2 } => {
                {
                    let mut prefs = self.rpc().client_host().prefs().borrow_mut();
                    prefs.reset_to_defaults();
                    prefs.set_profile_name(Some(name));
                    // Makes the file, which mustn't exist.
                    prefs.add_comment("Unison preferences file")?;
                    prefs.add("root", &root1)?;
                    prefs.add("root", &root2)?;
                }
                self.ui.scan_profiles()?;
                self.send(Event::Profiles(self.ui.profiles_and_roots.clone()));
            }
            Command::LoadProfile(name) => self.load_profile(&name).await?,
            Command::ProfileUnchanged => {
                let unchanged = self
                    .rpc()
                    .client_host()
                    .prefs()
                    .borrow()
                    .profile_unchanged();
                self.send(Event::ProfileUnchanged(unchanged));
            }
            Command::Configure(o) => {
                {
                    let client = self.rpc().client_host();
                    let mut prefs = client.prefs().borrow_mut();
                    if let Some(b) = o.batch {
                        prefs.set(&globals::BATCH, b);
                    }
                    if let Some(b) = o.terse {
                        prefs.set(&trace::TERSE, b);
                    }
                    if let Some(b) = o.auto {
                        prefs.set(&uicommon::AUTO, b);
                    }
                }
                if let Some(b) = o.log_to_stderr {
                    with_trace(self.rpc(), |t, _| t.send_log_msgs_to_stderr = b);
                }
                self.send(Event::ProfileLoaded(self.settings()));
            }
            Command::Connect => {
                self.connect().await?;
                self.send(Event::Connected(self.root_names()?));
            }
            Command::Update {
                allow_partial,
                want_watcher,
                paths,
                only,
            } => {
                // Set back however the scan ends, an interrupt included.
                let _paths = only.map(|only| PathsSetBack::new(self.rpc().client_host(), only));
                self.update(allow_partial, want_watcher, paths).await?
            }
            Command::WaitForChanges => {
                self.wait_for_changes().await?;
                self.send(Event::Changed);
            }
            Command::SetAction(i, a) => self.set_action(i, a)?,
            Command::Synchronize | Command::Interrupt => unreachable!("run handles it"),
            Command::Abort => self.abort(),
            Command::Commit => {
                let r = update::commit_updates(self.rpc(), self.roots()?).await;
                self.ask_warnings().await?;
                r?;
                self.send(Event::Committed);
            }
            Command::Ignore(items, kind) => self.ignore(&items, kind).await?,
            Command::Diff(i) => self.diff(i).await?,
            Command::Arrange(order) => self.arrange(&order)?,
            Command::Sort(kind) => self.sort(kind)?,
            Command::Log(kind, s) => self.log(kind, &s)?,
        }
        Ok(())
    }

    fn log(&self, kind: LogKind, s: &str) -> Result<(), Error> {
        with_trace(self.rpc(), |t, p| match kind {
            LogKind::Log => t.log(p, s),
            LogKind::LogColor => t.log_color(p, s),
            LogKind::LogOnly => t.log_only(p, s),
            LogKind::Status => t.status(p, s),
            LogKind::Message => t.message(p, s),
        })
    }

    fn arrange(&self, order: &[usize]) -> Result<(), Error> {
        {
            let mut items = self.items.borrow_mut();
            let mut seen = vec![false; items.len()];
            for &i in order {
                match seen.get_mut(i) {
                    None => return Err(Error::Fatal(format!("There is no item {i}"))),
                    Some(true) => return Err(Error::Fatal(format!("Item {i} is given twice"))),
                    Some(s) => *s = true,
                }
            }
            let mut old: Vec<Option<Item>> = items.drain(..).map(Some).collect();
            items.extend(order.iter().filter_map(|&i| old[i].take()));
        }
        self.send(Event::Items {
            items: self.views(),
            next: 0,
        });
        Ok(())
    }

    fn sort(&self, kind: SortKind) -> Result<(), Error> {
        let client = self.rpc().client_host();
        match kind {
            SortKind::Name => sortri::sort_by_name(client),
            SortKind::Size => sortri::sort_by_size(client),
            SortKind::NewFirst => sortri::sort_new_first(client),
            SortKind::Default => sortri::restore_default_settings(client),
        }
        {
            let prefs = client.prefs().borrow();
            let cmp = sortri::compare_recon_items(&prefs)?;
            self.items.borrow_mut().sort_by(|a, b| cmp(&a.ri, &b.ri));
        }
        self.send(Event::Items {
            items: self.views(),
            next: 0,
        });
        Ok(())
    }

    /// Shows the warnings kept, or (unless in batch mode) asks whether to
    /// go on after each; if not, unlocks the archives and stops.
    async fn ask_warnings(&self) -> Result<(), Stop> {
        loop {
            let Some(w) = ({
                let mut ws = self.warnings.borrow_mut();
                (!ws.is_empty()).then(|| ws.remove(0))
            }) else {
                return Ok(());
            };
            if self.log_warnings {
                with_trace(self.rpc(), |t, p| t.log(p, &format!("Warning: {w}\n")))?;
            }
            let batch = *self
                .rpc()
                .client_host()
                .prefs()
                .borrow()
                .read(&globals::BATCH);
            if batch {
                self.send(Event::Warning(w));
                continue;
            }
            let (tx, rx) = oneshot::channel();
            self.send(Event::Ask(Question::Warning(w, Reply(tx))));
            if !rx.await.unwrap_or(false) {
                if let Some(roots) = self.ui.roots() {
                    archive::unlock_archives(self.rpc(), roots).await?;
                }
                return Err(Stop::Exit);
            }
        }
    }

    async fn load_profile(&mut self, name: &str) -> Result<(), Stop> {
        self.items.borrow_mut().clear();
        let events = self.events.clone();
        let asked = self
            .ui
            .init_prefs(name, async move || {
                let (tx, rx) = oneshot::channel();
                let _ = events.send(Event::Ask(Question::Roots(Reply(tx))));
                rx.await.ok().flatten()
            })
            .await;
        self.ask_warnings().await?;
        asked?;
        self.profile = name.to_owned();
        self.send(Event::ProfileLoaded(self.settings()));
        Ok(())
    }

    fn settings(&self) -> Settings {
        let prefs = self.rpc().client_host().prefs().borrow();
        Settings {
            profile: self.profile.clone(),
            profile_path: prefs
                .profile_pathname(&self.profile, true)
                .unwrap_or_default(),
            roots: globals::raw_roots(&prefs),
            batch: *prefs.read(&globals::BATCH),
            auto: *prefs.read(&uicommon::AUTO),
            terse: *prefs.read(&trace::TERSE),
            expert: *prefs.read(&uicommon::EXPERT),
            repeat: prefs.read(&uicommon::REPEAT).clone(),
            retry: *prefs.read(&uicommon::RETRY),
            confirm_merge: *prefs.read(&uicommon::CONFIRM_MERGE),
            confirm_big_deletes: *prefs.read(&globals::CONFIRM_BIG_DELETES),
            label: prefs.read(&uicommon::PROFILE_LABEL).clone(),
            silent: *prefs.read(&cli::SILENT),
            dumbtty: *prefs.read(&cli::DUMBTTY),
            color: *prefs.read(&cli::COLOR),
            debug: prefs.read(&trace::DEBUG).clone(),
            run_tests: *prefs.read(&uicommon::RUN_TESTS),
            test_server: *prefs.read(&uicommon::TEST_SERVER),
            timers: *prefs.read(&trace::TIMERS),
        }
    }

    fn root_names(&self) -> Result<RootNames, Error> {
        let roots = self.roots()?.pair();
        Ok(RootNames {
            short: uicommon::roots_to_nice_strings(32, roots),
            narrow: uicommon::roots_to_nice_strings(15, roots),
            full: uicommon::root_suffixes(roots),
            headings: uicommon::roots_to_string(roots),
            hosts: (host_name(roots.0), host_name(roots.1)),
        })
    }

    /// `connectRoots`, with the message that it may take a while.
    async fn connect(&mut self) -> Result<(), Stop> {
        let events = self.events.clone();
        let print = self.print_wait_message;
        let client = self.ui.rpc().client_host().clone();
        self.ui
            .connect_roots(&mut || {
                let msg = uicommon::contacting_server_msg();
                if !print {
                    let _ = events.send(Event::Contacting(msg));
                } else if !*client.prefs().borrow().read(&cli::SILENT) {
                    client
                        .state::<Trace>()
                        .borrow_mut()
                        .msg(&format!("{msg}\n"));
                }
            })
            .await?;
        self.ask_warnings().await
    }

    /// `waitForChanges`: until the watcher reports a change to any of the
    /// replicas (OCaml's `Lwt.choose` over the roots).
    async fn wait_for_changes(&mut self) -> Result<(), Stop> {
        self.connect().await?;
        let (rpc, roots) = (self.ui.rpc(), self.roots()?);
        let mut waits: Vec<_> = roots
            .list()
            .iter()
            .map(|r| Box::pin(rpc.call_root(&gemel_engine::fswatch::WAIT_FOR_CHANGES, r, ())))
            .collect();
        std::future::poll_fn(|cx| {
            for w in &mut waits {
                if let Poll::Ready(r) = w.as_mut().poll(cx) {
                    return Poll::Ready(r);
                }
            }
            Poll::Pending
        })
        .await?;
        Ok(())
    }

    async fn update(
        &mut self,
        allow_partial: bool,
        want_watcher: bool,
        paths: update::Subpaths,
    ) -> Result<(), Stop> {
        self.items.borrow_mut().clear();
        self.connect().await?;
        let (rpc, roots) = (self.ui.rpc(), self.roots()?);
        with_trace(rpc, |t, p| t.status(p, "Looking for changes"))?;
        let client = rpc.client_host();
        if self.scan_status {
            let ev = self.events.clone();
            uutil::set_update_status_printer(
                client,
                Some(Arc::new(move |p: &str| {
                    let _ = ev.send(Event::Scanning(p.to_owned()));
                })),
            );
        }
        // The warnings loading the archives gives come before the scan.
        let loaded = archive::load_archives_safely(rpc, roots).await;
        let found = match loaded {
            Ok(_) => match self.ask_warnings().await {
                Ok(()) => {
                    let (rpc, roots) = (self.ui.rpc(), self.roots()?);
                    Ok(update::find_loaded_updates(rpc, roots, want_watcher, paths).await)
                }
                Err(stop) => Err(stop),
            },
            Err(e) => Ok(Err(e)),
        };
        if self.scan_status {
            uutil::set_update_status_printer(client, None);
            self.send(Event::Scanning(String::new()));
        }
        let found = found??;
        self.ask_warnings().await?;
        let (rpc, roots) = (self.ui.rpc(), self.roots()?);
        let (items, any_equal_updates, dangerous) =
            recon::reconcile_all(rpc, roots, allow_partial, found).await?;
        self.ask_warnings().await?;
        let (rpc, roots) = (self.ui.rpc(), self.roots()?);
        let found_archives = archive::found_archives(rpc);
        if !found_archives {
            update::commit_updates(rpc, roots).await?;
        }
        *self.items.borrow_mut() = items
            .into_iter()
            .map(|ri| Item {
                ri,
                transferred: 0,
                to_transfer: 0,
                outcome: None,
                status: None,
                started: false,
            })
            .collect();
        let updates = Updates {
            items: self.views(),
            any_equal_updates,
            dangerous_message: (!dangerous.is_empty())
                .then(|| uicommon::dangerous_path_msg(&dangerous)),
            dangerous,
            found_archives,
            roots: self.root_names()?,
        };
        self.send(Event::Updates(updates));
        Ok(())
    }

    fn set_action(&self, i: usize, a: SetAction) -> Result<(), Error> {
        let mut ri = self.item_ri(i)?;
        if let Replicas::Different(d) = &mut ri.replicas {
            match a {
                SetAction::Left => d.direction = Direction::Replica2ToReplica1,
                SetAction::Right => d.direction = Direction::Replica1ToReplica2,
                SetAction::Skip => d.direction = Direction::Conflict("skip requested".to_owned()),
                SetAction::Conflict => d.direction = Direction::Conflict(String::new()),
                SetAction::Merge => d.direction = Direction::Merge,
                SetAction::Older | SetAction::Newer => {
                    let dir = if a == SetAction::Older {
                        Choice::Older
                    } else {
                        Choice::Newer
                    };
                    let prefs = self.rpc().client_host().prefs().borrow();
                    recon::set_direction(&prefs, &mut ri, dir, Strength::Force)?;
                }
                SetAction::Revert => recon::revert_to_default_direction(&mut ri),
                SetAction::OlderIfConflict | SetAction::NewerIfConflict => {
                    let dir = if a == SetAction::OlderIfConflict {
                        Choice::Older
                    } else {
                        Choice::Newer
                    };
                    let strength = if d.direction.is_conflict() {
                        Strength::Force
                    } else {
                        Strength::Prefer
                    };
                    let prefs = self.rpc().client_host().prefs().borrow();
                    recon::set_direction(&prefs, &mut ri, dir, strength)?;
                }
                SetAction::Prefer(f) => {
                    let dir = match f {
                        Favor::First => Choice::Replica1ToReplica2,
                        Favor::Second => Choice::Replica2ToReplica1,
                        Favor::Newer => Choice::Newer,
                        Favor::Older => Choice::Older,
                    };
                    let prefs = self.rpc().client_host().prefs().borrow();
                    recon::set_direction(&prefs, &mut ri, dir, Strength::Prefer)?;
                }
                SetAction::MergeDefault => {
                    let prefs = self.rpc().client_host().prefs().borrow();
                    recon::set_direction(&prefs, &mut ri, Choice::Merge, Strength::Force)?;
                }
            }
        }
        self.items.borrow_mut()[i].ri = ri;
        self.send(Event::Item(i, self.view(i)));
        Ok(())
    }

    async fn ignore(&mut self, indices: &[usize], kind: IgnoreKind) -> Result<(), Stop> {
        let mut patterns = std::collections::BTreeSet::new();
        for &i in indices {
            let path = self.item_ri(i)?.path1;
            patterns.insert(match kind {
                IgnoreKind::Path => uicommon::ignore_path(&path),
                IgnoreKind::Name => uicommon::ignore_name(&path),
                IgnoreKind::Ext => uicommon::ignore_ext(&path),
            });
        }
        for re in &patterns {
            uicommon::add_ignore_pattern(self.rpc(), self.roots()?, re).await?;
        }
        self.ask_warnings().await?;
        let i = indices.iter().copied().min().unwrap_or(0);
        // `unisonUpdateForIgnore`. (Only the items' own paths are tested,
        // as in OCaml.)
        let (ignore, items) = {
            let prefs = self.rpc().client_host().prefs().borrow();
            let ignore = globals::Ignore::new(&prefs, case::mode(&prefs))?;
            (ignore, std::mem::take(&mut *self.items.borrow_mut()))
        };
        let mut next = None;
        let mut kept = Vec::new();
        for (j, it) in items.into_iter().enumerate() {
            if !ignore.test(&it.ri.path1) {
                if j >= i && next.is_none() {
                    next = Some(kept.len());
                }
                kept.push(it);
            }
        }
        let next = next.unwrap_or(kept.len().saturating_sub(1));
        *self.items.borrow_mut() = kept;
        self.send(Event::Items {
            items: self.views(),
            next,
        });
        Ok(())
    }

    async fn diff(&self, i: usize) -> Result<(), Stop> {
        let ri = self.item_ri(i)?;
        let (mut shown, mut errors) = (Vec::new(), Vec::new());
        uicommon::show_diffs(
            self.rpc(),
            self.roots()?,
            &ri,
            &mut |title, text| shown.push((title.to_owned(), text.to_owned())),
            &mut |e| errors.push(e.to_owned()),
            FileId(i as i64),
        )
        .await?;
        self.send(Event::Diff { shown, errors });
        Ok(())
    }

    fn abort(&self) {
        self.rpc().client_host().state::<Abort>().borrow_mut().all();
    }

    // ---- Items, as the frontend sees them ----

    fn views(&self) -> Vec<ItemView> {
        (0..self.items.borrow().len())
            .map(|i| self.view(i))
            .collect()
    }

    fn view(&self, i: usize) -> ItemView {
        let items = self.items.borrow();
        let it = &items[i];
        let ri = &it.ri;
        let fmt = Format::of(self.rpc());
        let previous = i
            .checked_sub(1)
            .map_or_else(Path::empty, |j| items[j].ri.path1.clone());
        let details = match (&it.outcome, self.ui.roots()) {
            (Some(Outcome::Failed(s)), _) => s.clone(),
            (_, Some(roots)) => fmt.details(roots.pair(), ri, "  "),
            (_, None) => String::new(),
        };
        let (left, right, conflict, changed_from_default, can_diff) = match &ri.replicas {
            Replicas::Problem(_) => ("", "", false, false, false),
            Replicas::Different(d) => (
                rc_to_string(&d.rc1.status),
                rc_to_string(&d.rc2.status),
                d.default_direction.is_conflict(),
                d.direction != d.default_direction,
                d.rc1.typ == Typ::File
                    && d.rc2.typ == Typ::File
                    && files_are_different(&d.rc1.status, &d.rc2.status),
            ),
        };
        let progress = match (&it.status, &it.outcome, &ri.replicas) {
            (None, None, _) => String::new(),
            (Some(s), None, _) => s.clone(),
            (_, _, Replicas::Problem(_)) => String::new(),
            (_, _, Replicas::Different(d)) if d.direction.is_conflict() => String::new(),
            (_, Some(Outcome::Succeeded), _) => "done".to_owned(),
            (_, Some(Outcome::Failed(_)), _) => "FAILED".to_owned(),
        };
        let (status1, action, status2, shown_path) = fmt.recon_item_parts(&Path::empty(), ri);
        let (errors1, errors2) = match &ri.replicas {
            Replicas::Different(d) => (d.errors1.clone(), d.errors2.clone()),
            Replicas::Problem(_) => (Vec::new(), Vec::new()),
        };
        ItemView {
            path: ri.path1.clone(),
            path_string: uicommon::path_string(&ri.path1),
            left,
            right,
            action,
            conflict,
            changed_from_default,
            can_diff,
            length: common::ri_length(ri),
            file_type: common::ri_file_type(ri),
            line: fmt.recon_item(&previous, ri, ""),
            details,
            problematic: common::problematic(ri),
            partially_problematic: common::partially_problematic(ri),
            deletion: common::is_deletion(ri),
            transferred: it.transferred,
            progress,
            outcome: it.outcome.clone(),
            status1,
            status2,
            shown_path,
            problem: match &ri.replicas {
                Replicas::Problem(s) => Some(s.clone()),
                Replicas::Different(_) => None,
            },
            skip_reason: match &ri.replicas {
                Replicas::Problem(s) => Some(s.clone()),
                Replicas::Different(d) => match &d.direction {
                    Direction::Conflict(r) => Some(r.clone()),
                    _ => None,
                },
            },
            errors1,
            errors2,
            props_only: match &ri.replicas {
                Replicas::Different(d) => matches!(
                    (&d.rc1.status, &d.rc2.status),
                    (
                        Status::PropsChanged,
                        Status::PropsChanged | Status::Unchanged
                    ) | (Status::Unchanged, Status::PropsChanged)
                ),
                Replicas::Problem(_) => false,
            },
            favor_changed: match &ri.replicas {
                Replicas::Different(d) => match (&d.rc1.status, &d.rc2.status) {
                    (Status::Modified, Status::PropsChanged) => Some(SetAction::Right),
                    (Status::PropsChanged, Status::Modified) => Some(SetAction::Left),
                    _ => None,
                },
                Replicas::Problem(_) => None,
            },
        }
    }

    // ---- Synchronizing ----

    /// `unisonSynchronize`, with what the text interface needs of
    /// `doTransport`: propagates the items not propagated yet, while
    /// reading commands (only [`Command::Abort`] is taken), and reporting
    /// progress as it comes.
    async fn synchronize(&mut self, commands: &mut Commands<Command>) -> Result<(), Stop> {
        // Only with the roots connected.
        self.roots()?;
        let rpc = self.ui.rpc();
        let (tx, rx) = mpsc::unbounded_channel();
        let client = rpc.client_host();
        uutil::set_progress_printer(
            client,
            Some(Arc::new(move |id, n, _| {
                let _ = tx.send((id, n));
            })),
        );
        *self.progress.borrow_mut() = Some(rx);
        let r = {
            let mut sync = pin!(self.propagate_all());
            let mut commands_open = true;
            loop {
                let next = std::future::poll_fn(|cx| {
                    if let Some(rx) = self.progress.borrow_mut().as_mut() {
                        while let Poll::Ready(Some((id, n))) = rx.poll_recv(cx) {
                            self.report(id.0 as usize, n);
                        }
                    }
                    if commands_open {
                        if let Poll::Ready(c) = commands.poll_recv(cx) {
                            return Poll::Ready(Err(c));
                        }
                    }
                    sync.as_mut().poll(cx).map(Ok)
                })
                .await;
                match next {
                    Ok(r) => break r.map_err(Stop::from),
                    Err(Some(Command::Abort)) => self.abort(),
                    Err(Some(Command::Interrupt)) => break Err(Stop::Interrupted),
                    Err(Some(Command::Log(kind, s))) => {
                        if let Err(e) = self.log(kind, &s) {
                            self.send(Event::Error(uicommon::exn2string(&e)));
                        }
                    }
                    Err(Some(c)) => self.send(Event::Refused(c)),
                    // The frontend has gone: the sync finishes all the same.
                    Err(None) => commands_open = false,
                }
            }
        };
        if matches!(r, Err(Stop::Interrupted)) {
            // The items being propagated count as done, as the text
            // interface counts them while the interrupt goes through.
            self.drain_progress();
            let in_flight: Vec<usize> = (self.items.borrow().iter().enumerate())
                .filter(|(_, it)| {
                    it.started && it.outcome.is_none() && !common::problematic(&it.ri)
                })
                .map(|(i, _)| i)
                .collect();
            for i in in_flight {
                self.item_done(i);
            }
        }
        uutil::set_progress_printer(client, None);
        self.drain_progress();
        *self.progress.borrow_mut() = None;
        *self.totals.borrow_mut() = None;
        r?;
        self.ask_warnings().await?;
        let summary = self.summary();
        self.send(Event::Synchronized(summary));
        Ok(())
    }

    /// Applies the progress reports waiting.
    fn drain_progress(&self) {
        loop {
            let Some((id, n)) = self
                .progress
                .borrow_mut()
                .as_mut()
                .and_then(|rx| rx.try_recv().ok())
            else {
                return;
            };
            self.report(id.0 as usize, n);
        }
    }

    /// `showProgress`: `n` more bytes for item `i`.
    fn report(&self, i: usize, n: Filesize) {
        let (status_changed, item_transferred, item_length, item_status) = {
            let mut items = self.items.borrow_mut();
            let Some(it) = items.get_mut(i) else {
                return;
            };
            it.transferred += n;
            let (b, len) = (it.transferred, it.to_transfer);
            let status = if b == 0 || len == 0 {
                "start ".to_owned()
            } else {
                util::percent2string(util::percentage_of_total_size(b, len))
            };
            let changed = it.status.as_deref() != Some(&status);
            it.status = Some(status.clone());
            (changed, b, len, status)
        };
        let mut totals = self.totals.borrow_mut();
        let Some(t) = totals.as_mut() else {
            return;
        };
        t.transferred += n;
        let now = Instant::now();
        t.stats
            .update(gemel_fs::system::gettimeofday(), t.transferred);
        // As the text interface shows it: every 0.1 second, or when an item
        // finishes or changes its percentage.
        let due = t
            .shown
            .is_none_or(|s| now.duration_since(s).as_secs_f64() >= 0.1);
        if !(due || status_changed || n == 0) {
            return;
        }
        t.shown = Some(now);
        let p = Progress {
            item: i,
            item_transferred,
            item_length,
            item_progress: item_status,
            transferred: t.transferred,
            length: t.length,
            items_done: t.items_done,
            items: t.items,
            rate: t.stats.avg_rate1(),
            eta: t.stats.eta(None, "--:--"),
        };
        drop(totals);
        self.send(Event::Progress(p));
    }

    /// Propagates the items not propagated yet (deletions last), between
    /// `transportStart` and `transportFinish`.
    async fn propagate_all(&self) -> Result<(), Error> {
        let (rpc, roots) = (self.ui.rpc(), self.roots()?);
        transport::log_start(rpc)?;
        let ris: Vec<ReconItem> = {
            let mut items = self.items.borrow_mut();
            for it in items.iter_mut() {
                it.transferred = 0;
                it.to_transfer = if it.outcome.is_none() {
                    common::ri_length(&it.ri)
                } else {
                    0
                };
            }
            let length = items.iter().map(|it| it.to_transfer).sum();
            let todo = items
                .iter()
                .filter(|it| it.outcome.is_none() && !common::problematic(&it.ri))
                .count();
            *self.totals.borrow_mut() = Some(Totals {
                transferred: 0,
                length,
                items_done: 0,
                items: todo,
                stats: Stats::new(length),
                shown: None,
            });
            items.iter().map(|it| it.ri.clone()).collect()
        };
        let confirm = {
            let prefs = rpc.client_host().prefs().borrow();
            *prefs.read(&uicommon::CONFIRM_MERGE) && !*prefs.read(&globals::BATCH)
        };
        for deletions in [false, true] {
            transport::transport_items(
                rpc,
                roots,
                &ris,
                |ri| common::is_deletion(ri) == deletions,
                |i, ri| -> Task<'_, Result<(), Error>> { Box::pin(self.propagate(i, ri, confirm)) },
            )
            .await?;
        }
        transport::log_finish(rpc)
    }

    /// `uiWrapper`: propagates item `i`, unless it has been already (a
    /// merge, say); a transient error is its failure.
    async fn propagate(&self, i: usize, ri: &ReconItem, confirm: bool) -> Result<(), Error> {
        if self.items.borrow()[i].outcome.is_some() {
            return Ok(());
        }
        self.items.borrow_mut()[i].started = true;
        let (rpc, roots) = (self.ui.rpc(), self.roots()?);
        let events = self.events.clone();
        let mut show_merge = move |title: String, text: String| -> Task<'static, bool> {
            let events = events.clone();
            Box::pin(async move {
                if !confirm {
                    let _ = events.send(Event::Merge { title, text });
                    return true;
                }
                let (tx, rx) = oneshot::channel();
                let _ = events.send(Event::Ask(Question::Merge {
                    title,
                    text,
                    reply: Reply(tx),
                }));
                rx.await.unwrap_or(false)
            })
        };
        let r = transport::transport_item(rpc, roots, ri, FileId(i as i64), &mut show_merge).await;
        // Its reports first, so that what is left is right.
        self.drain_progress();
        let outcome = match r {
            Ok(()) => Outcome::Succeeded,
            Err(Error::Transient(s)) => Outcome::Failed(s),
            Err(e) => return Err(e),
        };
        // `progressItem`, then what is left (as the text interface does).
        if !common::problematic(ri) {
            self.item_done(i);
        }
        let rem = {
            let items = self.items.borrow();
            items[i].to_transfer - items[i].transferred
        };
        if rem != 0 && (self.complete_progress || matches!(outcome, Outcome::Failed(_))) {
            self.report(i, rem);
        }
        self.items.borrow_mut()[i].outcome = Some(outcome);
        self.send(Event::Item(i, self.view(i)));
        Ok(())
    }

    /// `progressItem`: one more item done.
    fn item_done(&self, i: usize) {
        if let Some(t) = self.totals.borrow_mut().as_mut() {
            t.items_done += 1;
        }
        self.report(i, 0);
    }

    /// The failures, partial transfers and skipped items, as both
    /// interfaces count them.
    fn summary(&self) -> Summary {
        let items = self.items.borrow();
        let mut s = Summary {
            aborted: self.rpc().client_host().state::<Abort>().borrow().is_all(),
            ..Summary::default()
        };
        for (i, it) in items.iter().enumerate() {
            let skip = common::problematic(&it.ri);
            match &it.ri.replicas {
                Replicas::Problem(r) => s.skipped.push((i, r.clone())),
                Replicas::Different(d) => {
                    if let Direction::Conflict(r) = &d.direction {
                        s.skipped.push((i, r.clone()));
                    }
                }
            }
            match &it.outcome {
                _ if skip => {}
                None if !it.started => s.not_started += 1,
                None => {}
                Some(Outcome::Failed(m)) => s.failed.push((i, m.clone())),
                Some(Outcome::Succeeded) => {
                    s.transferred += 1;
                    if common::partially_problematic(&it.ri) {
                        s.partial.push(i);
                    }
                }
            }
        }
        let unsynced = |i: usize| items[i].ri.path1.clone();
        s.unsynchronized = s
            .failed
            .iter()
            .map(|(i, _)| *i)
            .chain(s.partial.iter().copied())
            .chain(s.skipped.iter().map(|(i, _)| *i))
            .map(unsynced)
            .collect();
        s
    }
}

/// `root2hostname`.
fn host_name(r: &gemel_rpc::root::Root) -> String {
    match &r.host {
        gemel_rpc::root::Host::Local => "local".to_owned(),
        gemel_rpc::root::Host::Remote(h) => h.clone(),
    }
}

/// `rcToString`.
fn rc_to_string(s: &Status) -> &'static str {
    match s {
        Status::Deleted => "Deleted",
        Status::Modified => "Modified",
        Status::PropsChanged | Status::MovedOut(..) | Status::MovedIn(..) => "PropsChanged",
        Status::Created => "Created",
        Status::Unchanged => "",
    }
}

/// `filesAreDifferent`: if only props changed, there is nothing to diff.
fn files_are_different(s1: &Status, s2: &Status) -> bool {
    !matches!(
        (s1, s2),
        (Status::PropsChanged, Status::Unchanged)
            | (Status::Unchanged, Status::PropsChanged)
            | (Status::PropsChanged, Status::PropsChanged)
    )
}
