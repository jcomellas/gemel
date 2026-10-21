//! Profile handling, formatting and statistics shared by all UIs.
//!
//! Ports Unison `src/uicommon.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! The startup sequence (profiles, `initPrefs`, `connectRoots`) is in
//! [`startup`], and the progress statistics in [`stats`].
//! `transportStart`, `transportFinish` and `transportItems` are in
//! `gemel_engine::transport`, which the engine's tests use too.
//!
//! Paths and root names are shown with invalid UTF-8 replaced, where OCaml
//! prints their bytes.

pub mod startup;
pub mod stats;

use gemel_base::case::{self, Mode};
use gemel_base::path::{Global, Path};
use gemel_base::prefs::{
    Category, DecodeError, Input, Internal, Kind, PrefDef, PrefValue, Prefs, Topic,
};
use gemel_base::util::{self, Error, FileId};
use gemel_compat::umarshal::{Reader, Writer};
use gemel_engine::common::{Direction, PrevState, ReconItem, ReplicaContent, Replicas, Status};
use gemel_engine::files;
use gemel_engine::globals::{self, Roots};
use gemel_fs::fileinfo::Typ;
use gemel_fs::props::PropsConfig;
use gemel_fs::system;
use gemel_rpc::registry::Rpc;
use gemel_rpc::root::{Host, Root};

// ---- Preferences ----

/// `auto`.
pub static AUTO: PrefDef<bool> = PrefDef::new(
    "auto",
    Category::Basic(Topic::Syncprocess_CLI),
    || false,
    "automatically accept default (nonconflicting) actions",
    "When set to {\\tt true}, this flag causes the user interface to skip \
     asking for confirmations on non-conflicting changes.  (More precisely, \
     when the user interface is done setting the propagation direction for \
     one entry and is about to move to the next, it will skip over all \
     non-conflicting entries and go directly to the next conflict.)",
);

/// `height` (`mainWindowHeight`). Here rather than in a graphical
/// interface because it is sent to the server.
pub static MAIN_WINDOW_HEIGHT: PrefDef<i64> = PrefDef::new(
    "height",
    Category::Advanced(Topic::GUI),
    || 15,
    "height (in lines) of main window in graphical interface",
    "Used to set the height (in lines) of the main window in the graphical \
     user interface.",
);

/// `expert`.
pub static EXPERT: PrefDef<bool> = PrefDef::new(
    "expert",
    Category::Internal(Internal::Devel),
    || false,
    "*Enable some developers-only functionality in the UI",
    "",
);

/// `label` (`profileLabel`).
pub static PROFILE_LABEL: PrefDef<String> = PrefDef::new(
    "label",
    Category::Advanced(Topic::General),
    String::new,
    "provide a descriptive string label for this profile",
    "Used in a profile to provide a descriptive string documenting its \
     settings.  (This is useful for users that switch between several \
     profiles, especially using the `fast switch' feature of the graphical \
     user interface.)",
);

/// `key` (`profileKey`). Nothing reads it (profiles are scanned for their
/// keys instead), but profiles may set it.
pub static PROFILE_KEY: PrefDef<String> = PrefDef::new(
    "key",
    Category::Advanced(Topic::General),
    String::new,
    "define a keyboard shortcut for this profile (in some UIs)",
    "Used in a profile to define a numeric key (0-9) that can be used in \
     the user interface to switch immediately to this profile.",
);

/// `contactquietly`.
pub static CONTACT_QUIETLY: PrefDef<bool> = PrefDef::new(
    "contactquietly",
    Category::Advanced(Topic::General),
    || false,
    "suppress the 'contacting server' message during startup",
    "If this flag is set, Unison will skip displaying the `Contacting \
     server' message (which some users find annoying) during startup.",
);

/// `contactingServerMsg`.
pub fn contacting_server_msg() -> String {
    format!(
        "{}: Contacting server...",
        util::capitalize_ascii(util::MY_NAME_AND_VERSION)
    )
}

/// The value of `repeat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Repeat {
    /// Synchronise once.
    NoRepeat,
    /// Again after this many seconds.
    Interval(i64),
    /// Whenever the file watcher reports a change.
    Watch,
    /// Both.
    WatchAndInterval(i64),
    /// A setting that couldn't be parsed, and why.
    Invalid(String, String),
}

impl Repeat {
    /// `parseRepeat`.
    pub fn parse(s: &str) -> Repeat {
        let parse_time = |ts: &str| {
            util::int_of_string(ts).ok_or_else(|| {
                format!(
                    "Value of 'repeat' preference ({s}) should be either a number, \
                     'watch' or 'watch+<number>'"
                )
            })
        };
        let words: Vec<String> = s
            .split('+')
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();
        let r = match words.iter().map(String::as_str).collect::<Vec<_>>()[..] {
            [] => Ok(Repeat::NoRepeat),
            ["watch"] => Ok(Repeat::Watch),
            ["watch", i] | [i, "watch"] => parse_time(i).map(Repeat::WatchAndInterval),
            _ => parse_time(s).map(Repeat::Interval),
        };
        r.unwrap_or_else(|e| Repeat::Invalid(s.to_owned(), e))
    }

    /// `externRepeat`.
    pub fn extern_(&self) -> String {
        match self {
            Repeat::NoRepeat | Repeat::Invalid(..) => String::new(),
            Repeat::Watch => "watch".to_owned(),
            Repeat::WatchAndInterval(i) => format!("watch+{i}"),
            Repeat::Interval(i) => i.to_string(),
        }
    }

    /// `repeatWatcher`: whether the file watcher is used.
    pub fn watcher(&self) -> bool {
        matches!(self, Repeat::Watch | Repeat::WatchAndInterval(_))
    }
}

impl PrefValue for Repeat {
    const KIND: Kind = Kind::Custom;
    fn apply(&self, input: Input<'_>) -> Result<Self, String> {
        match input {
            Input::Str(s) => Ok(Repeat::parse(s)),
            _ => Err("repeat takes a string".to_owned()),
        }
    }
    fn print(&self) -> Vec<String> {
        vec![self.extern_()]
    }
    /// `sum1 string externRepeat parseRepeat`.
    fn marshal(&self, w: &mut Writer) {
        w.string(self.extern_().as_bytes());
    }
    fn unmarshal(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let s = String::from_utf8(r.string()?).map_err(|_| DecodeError::NotUtf8)?;
        Ok(Repeat::parse(&s))
    }
}

/// `repeat`.
pub static REPEAT: PrefDef<Repeat> = PrefDef::new(
    "repeat",
    Category::Advanced(Topic::Syncprocess_CLI),
    || Repeat::NoRepeat,
    "synchronize repeatedly (text interface only)",
    "Setting this preference causes the text-mode interface to synchronize \
     repeatedly, rather than doing it just once and stopping.  If the \
     argument is a number, Unison will pause for that many seconds before \
     beginning again. When the argument is \\verb|watch|, Unison relies on \
     an external file monitoring process to synchronize whenever a change \
     happens.  You can combine the two with a \\verb|+| character to use \
     file monitoring and also do a full scan every specified number of \
     seconds.  For example, \\verb|watch+3600| will react to changes \
     immediately and additionally do a full scan every hour.",
);

/// `retry`.
pub static RETRY: PrefDef<i64> = PrefDef::new(
    "retry",
    Category::Advanced(Topic::Syncprocess_CLI),
    || 0,
    "re-try failed synchronizations N times (text ui only)",
    "Setting this preference causes the text-mode interface to try again to \
     synchronize updated paths where synchronization fails.  Each such path \
     will be tried N times.",
);

/// `confirmmerge`.
pub static CONFIRM_MERGE: PrefDef<bool> = PrefDef::new(
    "confirmmerge",
    Category::Advanced(Topic::Syncprocess),
    || false,
    "ask for confirmation before committing results of a merge",
    "Setting this preference causes both the text and graphical interfaces \
     to ask the user if the results of a merge command may be committed  to \
     the replica or not. Since the merge command works on temporary files, \
     the user can then cancel all the effects of applying the merge if it \
     turns out that the result is not satisfactory.  In  batch-mode, this \
     preference has no effect.  Default is false.",
);

/// `runTestsPrefName`.
pub const RUN_TESTS_PREF_NAME: &str = "selftest";

/// `selftest` (`runtests`). The self-tests aren't ported.
pub static RUN_TESTS: PrefDef<bool> = PrefDef::new(
    RUN_TESTS_PREF_NAME,
    Category::Expert,
    || false,
    "run internal tests and exit",
    "Run internal tests and exit.  This option is mostly for developers and \
     must be used carefully: in particular, it will delete the contents of \
     both roots, so that it can install its own files for testing.  This \
     flag only makes sense on the command line.  When it is provided, no \
     preference file is read: all preferences must be specified on \
     thecommand line.  Also, since the self-test procedure involves \
     overwriting the roots and backup directory, the names of the roots and \
     of the backupdir preference must include the string \"test\" or else \
     the tests will be aborted.  (If these are not given on the command \
     line, dummy subdirectories in the current directory will be created \
     automatically.)",
)
.cli_only();

/// `showprev`.
pub static SHOW_PREV: PrefDef<bool> = PrefDef::new(
    "showprev",
    Category::Internal(Internal::Devel),
    || false,
    "*Show previous properties, if they differ from current",
    "",
);

/// `rest` (`anonymousArgs`): the roots or profile name on the command line.
pub static ANONYMOUS_ARGS: PrefDef<Vec<String>> = PrefDef::new(
    "rest",
    Category::Internal(Internal::Other),
    Vec::new,
    "*roots or profile name",
    "",
);

/// `testserver`.
pub static TEST_SERVER: PrefDef<bool> = PrefDef::new(
    "testserver",
    Category::Advanced(Topic::Remote),
    || false,
    "exit immediately after the connection to the server",
    "Setting this flag on the command line causes Unison to attempt to \
     connect to the remote server and, if successful, print a message and \
     immediately exit.  Useful for debugging installation problems. Should \
     not be set in preference files.",
)
.cli_only();

/// Registers this module's preferences, in OCaml's order.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&AUTO)?;
    prefs.register(&MAIN_WINDOW_HEIGHT)?;
    prefs.register(&EXPERT)?;
    prefs.register(&PROFILE_LABEL)?;
    prefs.register(&PROFILE_KEY)?;
    prefs.register(&CONTACT_QUIETLY)?;
    prefs.register(&REPEAT)?;
    prefs.register(&RETRY)?;
    prefs.register(&CONFIRM_MERGE)?;
    prefs.register(&RUN_TESTS)?;
    prefs.register(&SHOW_PREV)?;
    prefs.register(&ANONYMOUS_ARGS)?;
    prefs.register(&TEST_SERVER)?;
    // For backward compatibility.
    prefs.alias(&TEST_SERVER, "testServer")
}

// ---- Formatting ----

/// What the formatters read from the session: `Update.foundArchives`
/// (when no archives were found, every file is new, so "new" is left out),
/// the props configuration and `showprev`.
#[derive(Debug, Clone, Copy)]
pub struct Format {
    /// `Update.foundArchives`.
    pub found_archives: bool,
    /// How props are shown.
    pub props: PropsConfig,
    /// `showprev`.
    pub show_prev: bool,
    /// The case mode, for comparing names.
    pub mode: Mode,
}

impl Format {
    /// The client's settings.
    pub fn of(rpc: &Rpc) -> Format {
        let client = rpc.client_host();
        let prefs = client.prefs().borrow();
        Format {
            found_archives: gemel_engine::archive::found_archives(rpc),
            props: PropsConfig::from_prefs(&prefs).with_features(&client.features().borrow()),
            show_prev: *prefs.read(&SHOW_PREV),
            mode: case::mode(&prefs),
        }
    }

    /// `choose`.
    fn choose<'a>(&self, s1: &'a str, s2: &'a str) -> &'a str {
        if self.found_archives { s1 } else { s2 }
    }

    /// `prevProps`: the previous props, with `showprev`.
    fn prev_props(&self, rc: &ReplicaContent) -> String {
        if !self.show_prev {
            return String::new();
        }
        use gemel_engine::common::UpdateItem;
        match &rc.ui {
            UpdateItem::NoUpdates
            | UpdateItem::Error(_)
            | UpdateItem::Updates(_, PrevState::Symlink) => String::new(),
            UpdateItem::Updates(_, PrevState::New) => " (new)".to_owned(),
            UpdateItem::Updates(_, PrevState::File(old, ..) | PrevState::Dir(old)) => {
                format!(
                    " (was: {})",
                    old.to_string(&self.props, &system::time2string)
                )
            }
        }
    }

    /// `replicaContentDesc`.
    fn desc(&self, rc: &ReplicaContent) -> String {
        rc.desc
            .set_length(rc.size.1)
            .to_string(&self.props, &system::time2string)
    }

    /// `replicaContent2string`: the contents of a path in one replica, the
    /// description after `sep`.
    pub fn replica_content(&self, rc: &ReplicaContent, sep: &str) -> String {
        let d = |s: &str| format!("{s}{sep}{}{}", self.desc(rc), self.prev_props(rc));
        let m = |s: &str, s2: &str, rc: &ReplicaContent| format!("{s}{sep}{} {s2}", self.desc(rc));
        let new_name = |n: &Path<Global>| {
            format!(
                "||--> new name: {}{}",
                path_string(n),
                overwrite_warning(rc)
            )
        };
        let previous_name = |n: &Path<Global>| format!("||<-- previous name: {}", path_string(n));
        match (rc.typ, &rc.status) {
            (Typ::Absent, Status::Unchanged) => "absent".to_owned(),
            (typ, Status::Unchanged) => format!(
                "unchanged {}{sep}{}",
                util::padto(7, &util::truncate_string(typ.as_str().as_bytes(), 7)),
                self.desc(rc)
            ),
            (Typ::Absent, Status::Deleted) => "deleted".to_owned(),
            (Typ::File, Status::Created) => {
                d(self.choose("new file         ", "file             "))
            }
            (Typ::File, Status::Modified) => d("changed file     "),
            (Typ::File, Status::PropsChanged) => d("changed props    "),
            (Typ::File, Status::MovedOut(n, p, _)) => m("moved/renamed fil", &new_name(n), p),
            (Typ::File, Status::MovedIn(n, _, _)) => m("moved/renamed fil", &previous_name(n), rc),
            (Typ::Symlink, Status::Created) => {
                d(self.choose("new symlink      ", "symlink          "))
            }
            (Typ::Symlink, Status::Modified) => d("changed symlink  "),
            (Typ::Directory, Status::Created) => {
                d(self.choose("new dir          ", "dir              "))
            }
            (Typ::Directory, Status::Modified) => d("changed dir      "),
            (Typ::Directory, Status::PropsChanged) => d("dir props changed"),
            (Typ::Directory, Status::MovedOut(n, p, _)) => m("moved/renamed dir", &new_name(n), p),
            (Typ::Directory, Status::MovedIn(n, _, _)) => {
                m("moved/renamed dir", &previous_name(n), rc)
            }
            other => unreachable!("impossible replica content {other:?}"),
        }
    }

    /// `replicaContent2shortString`.
    pub fn replica_content_short(&self, rc: &ReplicaContent) -> &'static str {
        match (rc.typ, &rc.status) {
            (_, Status::Unchanged) => "        ",
            (Typ::Absent, Status::Deleted) => "deleted ",
            (Typ::File, Status::Created) => self.choose("new file", "file    "),
            (Typ::File, Status::Modified) => "changed ",
            (Typ::File, Status::PropsChanged) => "props   ",
            (Typ::File, Status::MovedOut(..) | Status::MovedIn(..)) => "renamed ",
            (Typ::Symlink, Status::Created) => self.choose("new link", "link    "),
            (Typ::Symlink, Status::Modified) => "chgd lnk",
            (Typ::Directory, Status::Created) => self.choose("new dir ", "dir     "),
            (Typ::Directory, Status::Modified) => "chgd dir",
            (Typ::Directory, Status::PropsChanged) => "props   ",
            (Typ::Directory, Status::MovedOut(..) | Status::MovedIn(..)) => "rnmd dir",
            other => unreachable!("impossible replica content {other:?}"),
        }
    }

    /// `details2string`: both replicas' contents, each under its root's
    /// name.
    pub fn details(&self, roots: (&Root, &Root), ri: &ReconItem, sep: &str) -> String {
        match &ri.replicas {
            Replicas::Problem(s) => format!("Error: {s}\n"),
            Replicas::Different(d) => {
                let (n1, n2) = roots_to_nice_strings(12, roots);
                format!(
                    "{} : {}\n{} : {}",
                    util::padto(12, &n1),
                    self.replica_content(&d.rc1, sep),
                    util::padto(12, &n2),
                    self.replica_content(&d.rc2, sep)
                )
            }
        }
    }

    /// `reconItem2stringList`: the two replicas' short statuses, the action
    /// and the path (with its new or previous name, for a move). Only a
    /// problem's path is shown relative to `old_path` (`displayPath`).
    pub fn recon_item_parts(
        &self,
        old_path: &Path<Global>,
        ri: &ReconItem,
    ) -> (&'static str, Action, &'static str, String) {
        let d = match &ri.replicas {
            Replicas::Problem(_) => {
                return (
                    "        ",
                    Action::Error,
                    "        ",
                    display_path(self.mode, old_path, &ri.path1),
                );
            }
            Replicas::Different(d) => d,
        };
        let partial = !d.errors1.is_empty() || !d.errors2.is_empty();
        let moved =
            |rc: &ReplicaContent| matches!(rc.status, Status::MovedOut(..) | Status::MovedIn(..));
        let onemoved = if moved(&d.rc1) && moved(&d.rc2) {
            String::new()
        } else {
            match (&d.rc1.status, &d.rc2.status) {
                (Status::MovedOut(n, _, _), _) => {
                    format!(
                        " (--> new name: {}){}",
                        path_string(n),
                        overwrite_warning(&d.rc1)
                    )
                }
                (_, Status::MovedOut(n, _, _)) => {
                    format!(
                        " (--> new name: {}){}",
                        path_string(n),
                        overwrite_warning(&d.rc2)
                    )
                }
                (Status::MovedIn(n, _, _), _) | (_, Status::MovedIn(n, _, _)) => {
                    format!(" (<-- previous name: {})", path_string(n))
                }
                _ => String::new(),
            }
        };
        (
            self.replica_content_short(&d.rc1),
            Action::of_direction(partial, &d.direction),
            self.replica_content_short(&d.rc2),
            path_string(&ri.path1) + &onemoved,
        )
    }

    /// `reconItem2string`: an item on one line, with `status` before the
    /// path.
    pub fn recon_item(&self, old_path: &Path<Global>, ri: &ReconItem, status: &str) -> String {
        let (r1, action, r2, path) = self.recon_item_parts(old_path, ri);
        format!("{r1} {} {r2} {status} {path}", action.nice_string())
    }
}

/// `overwriteWarning`: what a move would overwrite.
fn overwrite_warning(rc: &ReplicaContent) -> String {
    match &rc.status {
        Status::MovedOut(_, _, other) if other.typ != Typ::Absent => format!(
            " <will overwrite a {} in the other replica>",
            other.typ.as_str()
        ),
        _ => String::new(),
    }
}

/// `Path.toString`, for display.
pub fn path_string(p: &Path<Global>) -> String {
    String::from_utf8_lossy(p.as_bytes()).into_owned()
}

/// `roots2niceStrings`: short names for the two roots, at most `length`
/// characters: the ends of their paths that differ, or `local` and the
/// host's name.
pub fn roots_to_nice_strings(length: i64, (r1, r2): (&Root, &Root)) -> (String, String) {
    let t = |s: &[u8]| util::truncate_string(s, length);
    match (&r1.host, &r2.host) {
        (Host::Local, Host::Local) => {
            let (n1, n2) = r1.fspath.different_suffix(&r2.fspath);
            (t(&n1), t(&n2))
        }
        (Host::Local, Host::Remote(h)) => (t(b"local"), t(h.as_bytes())),
        (Host::Remote(h), Host::Local) => (t(h.as_bytes()), t(b"local")),
        (Host::Remote(_), Host::Remote(_)) => unreachable!("two remote roots"),
    }
}

/// `roots2string`: the roots' names, as headings of the columns.
pub fn roots_to_string(roots: (&Root, &Root)) -> String {
    let (r1, r2) = roots_to_nice_strings(12, roots);
    format!("{}   {}       ", util::padto(12, &r1), util::padto(12, &r2))
}

/// `displayPath`: `path`, with the leading names it shares with
/// `previous` replaced by three spaces each.
pub fn display_path(mode: Mode, previous: &Path<Global>, path: &Path<Global>) -> String {
    let names = |p: &Path<Global>| p.to_names().unwrap_or_default();
    let (previous, names) = (names(previous), names(path));
    if names.is_empty() {
        return "/".to_owned();
    }
    let level = previous
        .iter()
        .zip(&names)
        .take_while(|(a, b)| a.compare_in(b, mode).is_eq())
        .count();
    let suffix = names[level..]
        .iter()
        .fold(Path::<Global>::empty(), |p, n| p.child(n));
    format!("{}{}", " ".repeat(level * 3), path_string(&suffix))
}

/// `Uicommon.action`: what will be done with an item, for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A problem found during update detection.
    Error,
    /// A conflict, skipped; partial if there were errors deep inside.
    Skip(bool),
    /// From the first replica to the second.
    LtoR(bool),
    /// From the second replica to the first.
    RtoL(bool),
    /// Merged.
    Merge,
}

impl Action {
    /// `direction2action`.
    pub fn of_direction(partial: bool, dir: &Direction) -> Action {
        match dir {
            Direction::Conflict(_) => Action::Skip(partial),
            Direction::Replica1ToReplica2 => Action::LtoR(partial),
            Direction::Replica2ToReplica1 => Action::RtoL(partial),
            Direction::Merge => Action::Merge,
        }
    }

    /// `action2niceString`.
    pub fn nice_string(self) -> &'static str {
        match self {
            Action::Error => "error",
            Action::Skip(_) => "<-?->",
            Action::LtoR(false) => "---->",
            Action::LtoR(true) => "--?->",
            Action::RtoL(false) => "<----",
            Action::RtoL(true) => "<-?--",
            Action::Merge => "<-M->",
        }
    }
}

/// `exn2string`: an error for display. (OCaml also describes the
/// exceptions that are bugs; here those are panics.)
pub fn exn2string(e: &Error) -> String {
    e.to_string()
}

/// `showDiffs`: runs the `diff` command on an item's two files (the files
/// they were moved from, for a move) and shows the result with `printer`;
/// a reason it can't with `errprinter`.
pub async fn show_diffs(
    rpc: &Rpc,
    roots: &Roots,
    ri: &ReconItem,
    printer: &mut dyn FnMut(&str, &str),
    errprinter: &mut dyn FnMut(&str),
    id: FileId,
) -> Result<(), Error> {
    let d = match &ri.replicas {
        Replicas::Problem(_) => {
            errprinter("Can't diff files: there was a problem during update detection");
            return Ok(());
        }
        Replicas::Different(d) if d.rc1.typ == Typ::File && d.rc2.typ == Typ::File => d,
        Replicas::Different(_) => {
            errprinter("Can't diff: path doesn't refer to a file in both replicas");
            return Ok(());
        }
    };
    let source = |rc: &ReplicaContent, path| match &rc.status {
        Status::MovedOut(n, moved, _) => (n.clone(), moved.ui.clone()),
        _ => (path, rc.ui.clone()),
    };
    let (path1, ui1) = source(&d.rc1, ri.path1.clone());
    let (path2, ui2) = source(&d.rc2, ri.path2.clone());
    let (root1, root2) = roots.pair();
    match files::diff(rpc, root1, &path1, &ui1, root2, &path2, &ui2, printer, id).await {
        Err(Error::Transient(e)) => {
            errprinter(&e);
            Ok(())
        }
        r => r,
    }
}

// ---- Messages ----

/// `dangerousPathMsg`: the warning about paths emptied in one replica.
pub fn dangerous_path_msg(paths: &[Path<Global>]) -> String {
    if paths.len() == 1 && paths[0].as_bytes().is_empty() {
        return "The root of one of the replicas has been completely emptied.\n\
                Unison may delete everything in the other replica.  (Set the \n\
                'confirmbigdel' preference to false to disable this check.)\n"
            .to_owned();
    }
    let paths: Vec<String> = paths
        .iter()
        .map(|p| format!("'{}'", path_string(p)))
        .collect();
    format!(
        "The following paths have been completely emptied in one replica:\n  {}\n\
         Unison may delete everything below these paths in the other replica.\n\
         \n       (Set the 'confirmbigdel' preference to false to disable this check.)\n",
        paths.join("\n  ")
    )
}

// ---- Patterns for ignoring paths ----

/// `globx_quote`: `s` as a glob that matches only itself.
fn globx_quote(s: &str) -> String {
    let mut out = String::with_capacity(2 * s.len() + 2);
    out.push('{');
    for c in s.chars() {
        if matches!(c, '*' | '?' | '[' | '{' | '}' | ',' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('}');
    out
}

/// `quote`: `s` quoted as a glob, with `->` (which separates a pattern
/// from its value) escaped as `[-]>`.
pub fn quote(s: &str) -> String {
    let sep = util::trim_whitespace(gemel_engine::pred::MAP_SEPARATOR);
    let esc = format!("[{}]{}", &sep[..1], &sep[1..]);
    let mut s = globx_quote(s);
    loop {
        let e = util::split_into_words_by_string(&s, sep).join(&esc);
        if e == s {
            return e;
        }
        s = e;
    }
}

/// `ignorePath`: the pattern that ignores `path`.
pub fn ignore_path(path: &Path<Global>) -> String {
    format!("Path {}", quote(&path_string(path)))
}

/// `ignoreName`: the pattern that ignores every file named as `path` is.
///
/// # Panics
///
/// If `path` is the root.
pub fn ignore_name(path: &Path<Global>) -> String {
    let name = path.final_name().expect("ignoreName of the root");
    format!("Name {}", quote(&String::from_utf8_lossy(name.as_bytes())))
}

/// `ignoreExt`: the pattern that ignores every file with `path`'s
/// extension (or its name, if it has none).
///
/// # Panics
///
/// If `path` is the root.
pub fn ignore_ext(path: &Path<Global>) -> String {
    let name = path.final_name().expect("ignoreExt of the root");
    let s = String::from_utf8_lossy(name.as_bytes());
    match s.rfind('.') {
        Some(pos) => format!("Name {{,.}}*{}", quote(&s[pos..])),
        None => format!("Name {}", quote(&s)),
    }
}

/// `addIgnorePattern`: ignores `re` from now on, adds it to the profile,
/// and sends the new preferences to the servers.
pub async fn add_ignore_pattern(rpc: &Rpc, roots: &Roots, re: &str) -> Result<(), Error> {
    if re == "Path " {
        return Err(Error::Transient("Can't ignore the root path!".to_owned()));
    }
    let line = {
        let mut prefs = rpc.client_host().prefs().borrow_mut();
        globals::add_regexp_to_ignore(&mut prefs, re);
        prefs.add("ignore", re)?
    };
    gemel_engine::with_trace(rpc, |t, p| t.status(p, &line))?;
    // So that the servers ignore the same paths (when rescanning, say).
    globals::propagate_prefs(rpc, roots).await
}

// ---- Usage ----

/// `coreUsageMsg`.
fn core_usage_msg() -> String {
    let n = util::MY_NAME;
    format!(
        "Usage: {n} [options]\n    or {n} root1 root2 [options]\n    or {n} profilename [options]\n"
    )
}

/// `shortUsageMsg`.
pub fn short_usage_msg() -> String {
    let n = util::MY_NAME;
    format!(
        "{}\nFor a list of options, type \"{n} -help\".\n\
         For a tutorial on basic usage, type \"{n} -doc tutorial\".\n\
         For other documentation, type \"{n} -doc topics\".\n",
        core_usage_msg()
    )
}

/// `usageMsg`.
pub fn usage_msg() -> String {
    core_usage_msg()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(s: &str) -> Path<Global> {
        Path::from_bytes(s.as_bytes()).unwrap()
    }

    #[test]
    fn repeat_is_parsed_like_ocaml() {
        assert_eq!(Repeat::parse(""), Repeat::NoRepeat);
        assert_eq!(Repeat::parse(" + "), Repeat::NoRepeat);
        assert_eq!(Repeat::parse("watch"), Repeat::Watch);
        assert_eq!(Repeat::parse(" Watch "), Repeat::Watch);
        assert_eq!(Repeat::parse("watch+3600"), Repeat::WatchAndInterval(3600));
        assert_eq!(Repeat::parse("60 + WATCH"), Repeat::WatchAndInterval(60));
        assert_eq!(Repeat::parse("30"), Repeat::Interval(30));
        assert_eq!(Repeat::parse("0x10"), Repeat::Interval(16));
        let bad = "Value of 'repeat' preference (soon) should be either a number, \
                   'watch' or 'watch+<number>'";
        assert_eq!(
            Repeat::parse("soon"),
            Repeat::Invalid("soon".into(), bad.into())
        );
        // A number is the whole setting, untrimmed.
        assert!(matches!(Repeat::parse(" 30"), Repeat::Invalid(..)));
        assert!(matches!(Repeat::parse("watch+1+2"), Repeat::Invalid(..)));
        assert_eq!(Repeat::WatchAndInterval(5).extern_(), "watch+5");
        assert_eq!(Repeat::Invalid("x".into(), String::new()).extern_(), "");
        assert!(Repeat::Watch.watcher() && !Repeat::Interval(1).watcher());
    }

    #[test]
    fn repeat_travels_as_its_setting() {
        let r = Repeat::WatchAndInterval(7);
        let mut w = Writer::new();
        r.marshal(&mut w);
        let bytes = w.into_payload();
        assert_eq!(Repeat::unmarshal(&mut Reader::new(&bytes)).unwrap(), r);
    }

    #[test]
    fn paths_are_shown_after_the_previous_one() {
        let m = Mode::Sensitive;
        assert_eq!(display_path(m, &path("a/b"), &Path::empty()), "/");
        assert_eq!(display_path(m, &Path::empty(), &path("a/b")), "a/b");
        assert_eq!(display_path(m, &path("a/b"), &path("a/c/d")), "   c/d");
        assert_eq!(display_path(m, &path("a/b"), &path("a/b/c")), "      c");
        assert_eq!(display_path(m, &path("A/b"), &path("a/c")), "a/c");
        assert_eq!(
            display_path(Mode::Insensitive, &path("A/b"), &path("a/c")),
            "   c"
        );
    }

    #[test]
    fn ignore_patterns_match_only_their_path() {
        assert_eq!(ignore_path(&path("a/b.c")), "Path {a/b.c}");
        assert_eq!(ignore_name(&path("a/b.c")), "Name {b.c}");
        assert_eq!(ignore_ext(&path("a/b.tar.gz")), "Name {,.}*{.gz}");
        assert_eq!(ignore_ext(&path("a/README")), "Name {README}");
        assert_eq!(ignore_path(&path("x->y")), "Path {x[-]>y}");
        let mode = Mode::Sensitive;
        for (p, other) in [
            ("a/x*y", "a/xzy"),
            ("{a,b}", "a"),
            ("x->y", "x"),
            ("q?", "qq"),
            ("[ab]", "a"),
            ("back\\slash", "backslash"),
        ] {
            let mut prefs = crate::new_prefs().unwrap();
            globals::add_regexp_to_ignore(&mut prefs, &ignore_path(&path(p)));
            assert!(
                globals::should_ignore(&prefs, mode, &path(p)).unwrap(),
                "{p}"
            );
            assert!(
                !globals::should_ignore(&prefs, mode, &path(other)).unwrap(),
                "{p} {other}"
            );
        }
    }

    #[test]
    fn dangerous_paths_are_listed() {
        assert!(dangerous_path_msg(&[Path::empty()]).starts_with("The root of one"));
        assert_eq!(
            dangerous_path_msg(&[path("a"), path("b/c")]),
            "The following paths have been completely emptied in one replica:\n  \
             'a'\n  'b/c'\nUnison may delete everything below these paths in the other \
             replica.\n\n       (Set the 'confirmbigdel' preference to false to disable \
             this check.)\n"
        );
    }

    #[test]
    fn actions_have_arrows() {
        let c = Direction::Conflict(String::new());
        assert_eq!(Action::of_direction(true, &c).nice_string(), "<-?->");
        let r = Direction::Replica1ToReplica2;
        assert_eq!(Action::of_direction(false, &r).nice_string(), "---->");
        assert_eq!(Action::of_direction(true, &r).nice_string(), "--?->");
        let l = Direction::Replica2ToReplica1;
        assert_eq!(Action::of_direction(true, &l).nice_string(), "<-?--");
        assert_eq!(Action::of_direction(true, &Direction::Merge), Action::Merge);
    }

    #[test]
    fn roots_are_named_by_what_differs() {
        use gemel_fs::fspath::Fspath;
        let local = |s: &str| Root::local(Fspath::from_canonical(s.as_bytes().to_vec()));
        let (a, b) = (local("/home/me/docs"), local("/backup/me/docs"));
        assert_eq!(
            roots_to_string((&a, &b)),
            "home/me/docs   backup/me...       "
        );
        let remote = Root {
            host: Host::Remote("server.example.com".into()),
            fspath: Fspath::from_canonical(b"/x".to_vec()),
        };
        assert_eq!(
            roots_to_nice_strings(12, (&a, &remote)),
            ("local".to_owned(), "server.ex...".to_owned())
        );
    }
}
