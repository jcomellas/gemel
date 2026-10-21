//! Reconciliation: deciding what to do with each update.
//!
//! Ports Unison `src/recon.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Reconciliation runs on the client. It compares both roots' updates at
//! each path, records the paths updated the same way on both as equal in
//! the archives ([`update::mark_equal`]), and proposes a direction for each
//! other difference; then `prefer`, `force` and the `no*` preferences
//! override the proposals.
//!
//! OCaml marks the equal paths in batches of 5000 as it goes, to save
//! memory. Marking is a remote call, so here the walk is synchronous and
//! keeps the batches, which are marked in the same order once it is done.
//! The walk reads the local archive only at paths it hasn't reached yet,
//! below none of the paths already marked (a directory's item is made
//! before its children are reconciled), so it sees what OCaml sees.

use std::collections::HashMap;
use std::sync::Arc;

use gemel_base::case::{self, Mode};
use gemel_base::name::Name;
use gemel_base::path::{Global, Path};
use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
use gemel_base::util::Error;
use gemel_fs::fileinfo::{self, FsConfig, Typ};
use gemel_fs::props::Props;
use gemel_rpc::registry::Rpc;

use crate::common::{
    ContentsChange, Difference, Direction, PermChange, PrevState, ReconItem, ReplicaContent,
    Replicas, Status, UpdateContent, UpdateItem,
};
use crate::globals::{self, Roots};
use crate::moves::{self, Moves};
use crate::pred::{Compiled, Pred};
use crate::sortri;
use crate::tree::{Builder, Tree};
use crate::update::{self, LocalArchive, PathUpdates};

// ---- Preferences ----

/// `force`.
pub static FORCE_ROOT: PrefDef<String> = PrefDef::new(
    "force",
    Category::Advanced(Topic::Sync),
    String::new,
    "force changes from this replica to the other",
    "Including the preference \\texttt{-force \\ARG{root}} causes Unison to \
     resolve all differences (including non-conflicting changes) in favor of \
     \\ARG{root}.  \
     This effectively changes Unison from a synchronizer into a mirroring \
     utility.  \n\n\
     You can also specify a unique prefix or suffix of the path of one of \
     the roots or a unique prefix of the hostname of a remote root.\n\n\
     You can also specify \\verb|-force newer| (or \\verb|-force older|) \
     to force Unison to choose the file with the later (earlier) \
     modtime.  In this case, the \\verb|-times| preference must also \
     be enabled.  If modtimes are equal in both replicas when using \
     \\verb|newer| or \\verb|older| then this preference will have no \
     effect (changes will be synced as if without this preference or \
     remain unsynced in case of a conflict).\n\n\
     This preference is overridden by the \\verb|forcepartial| preference.\n\n\
     This preference should be used only if you are {\\em sure} you \
     know what you are doing!",
);

/// `forcepartial`.
pub static FORCE_ROOT_PARTIAL: Pred = Pred::new(
    "forcepartial",
    Category::Advanced(Topic::Sync),
    "add a pattern to the forcepartial list",
    "Including the preference \\texttt{forcepartial = \\ARG{PATHSPEC} -> \\ARG{root}} causes Unison to \
     resolve all differences (even non-conflicting changes) in favor of \
     \\ARG{root} for the files in \\ARG{PATHSPEC} (see \\sectionref{pathspec}{Path Specification} \
     for more information).  \
     This effectively changes Unison from a synchronizer into a mirroring \
     utility.  \n\n\
     You can also specify a unique prefix or suffix of the path of one of \
     the roots or a unique prefix of the hostname of a remote root.\n\n\
     You can also specify \\verb|forcepartial PATHSPEC -> newer| \
     (or \\verb|forcepartial PATHSPEC -> older|) \
     to force Unison to choose the file with the later (earlier) \
     modtime.  In this case, the \\verb|-times| preference must also \
     be enabled.  If modtimes are equal in both replicas when using \
     \\verb|newer| or \\verb|older| then this preference will have no \
     effect (changes will be synced as if without this preference or \
     remain unsynced in case of a conflict).\n\n\
     This preference should be used only if you are {\\em sure} you \
     know what you are doing!",
);

/// `prefer`.
pub static PREFER_ROOT: PrefDef<String> = PrefDef::new(
    "prefer",
    Category::Advanced(Topic::Sync),
    String::new,
    "choose this replica's version for conflicting changes",
    "Including the preference \\texttt{-prefer \\ARG{root}} causes Unison always to \
     resolve conflicts in favor of \\ARG{root}, rather than asking for \
     guidance from the user, except for paths marked by the preference \
     \\texttt{merge}.  (The syntax of \\ARG{root} is the same as \
     for the \\verb|root| preference, plus the special values \
     \\verb|newer| and \\verb|older|.)  \n\n\
     You can also specify a unique prefix or suffix of the path of one of \
     the roots or a unique prefix of the hostname of a remote root.\n\n\
     This preference is overridden by the \\verb|preferpartial| preference.\n\n\
     This preference should be used only if you are {\\em sure} you \
     know what you are doing!",
);

/// `preferpartial`.
pub static PREFER_ROOT_PARTIAL: Pred = Pred::new(
    "preferpartial",
    Category::Advanced(Topic::Sync),
    "add a pattern to the preferpartial list",
    "Including the preference \\texttt{preferpartial = \\ARG{PATHSPEC} -> \\ARG{root}} \
     causes Unison always to \
     resolve conflicts in favor of \\ARG{root}, rather than asking for \
     guidance from the user, for the files in \\ARG{PATHSPEC} (see \
     \\sectionref{pathspec}{Path Specification} \
     for more information).  (The syntax of \\ARG{root} is the same as \
     for the \\verb|root| preference, plus the special values \
     \\verb|newer| and \\verb|older|.)  \n\n\
     You can also specify a unique prefix or suffix of the path of one of \
     the roots or a unique prefix of the hostname of a remote root.\n\n\
     This preference should be used only if you are {\\em sure} you \
     know what you are doing!",
);

/// `nodeletion`.
pub static NO_DELETION: PrefDef<Vec<String>> = PrefDef::new(
    "nodeletion",
    Category::Basic(Topic::Sync),
    Vec::new,
    "prevent file deletions on one replica",
    "Including the preference \\texttt{-nodeletion \\ARG{root}} prevents \
     Unison from performing any file deletion on root \\ARG{root}.\n\n\
     You can also specify a unique prefix or suffix of the path of one of \
     the roots or a unique prefix of the hostname of a remote root.\n\n\
     This preference can be included twice, once for each root, if you \
     want to prevent any deletion.",
);

/// `noupdate`.
pub static NO_UPDATE: PrefDef<Vec<String>> = PrefDef::new(
    "noupdate",
    Category::Basic(Topic::Sync),
    Vec::new,
    "prevent file updates and deletions on one replica",
    "Including the preference \\texttt{-noupdate \\ARG{root}} prevents \
     Unison from performing any file update or deletion on root \
     \\ARG{root}.\n\n\
     You can also specify a unique prefix or suffix of the path of one of \
     the roots or a unique prefix of the hostname of a remote root.\n\n\
     This preference can be included twice, once for each root, if you \
     want to prevent any update.",
);

/// `nocreation`.
pub static NO_CREATION: PrefDef<Vec<String>> = PrefDef::new(
    "nocreation",
    Category::Basic(Topic::Sync),
    Vec::new,
    "prevent file creations on one replica",
    "Including the preference \\texttt{-nocreation \\ARG{root}} prevents \
     Unison from performing any file creation on root \\ARG{root}.\n\n\
     You can also specify a unique prefix or suffix of the path of one of \
     the roots or a unique prefix of the hostname of a remote root.\n\n\
     This preference can be included twice, once for each root, if you \
     want to prevent any creation.",
);

/// `nodeletionpartial`.
pub static NO_DELETION_PARTIAL: Pred = Pred::new(
    "nodeletionpartial",
    Category::Advanced(Topic::Sync),
    "add a pattern to the nodeletionpartial list",
    "Including the preference \
     \\texttt{nodeletionpartial = \\ARG{PATHSPEC} -> \\ARG{root}} prevents \
     Unison from performing any file deletion in \\ARG{PATHSPEC} \
     on root \\ARG{root} (see \\sectionref{pathspec}{Path Specification} \
     for more information).  It is recommended to use {\\tt BelowPath} \
     patterns when selecting a directory and all its contents.",
);

/// `noupdatepartial`.
pub static NO_UPDATE_PARTIAL: Pred = Pred::new(
    "noupdatepartial",
    Category::Advanced(Topic::Sync),
    "add a pattern to the noupdatepartial list",
    "Including the preference \
     \\texttt{noupdatepartial = \\ARG{PATHSPEC} -> \\ARG{root}} prevents \
     Unison from performing any file update or deletion in \
     \\ARG{PATHSPEC} on root \\ARG{root} (see \
     \\sectionref{pathspec}{Path Specification} for more information). \
     It is recommended to use {\\tt BelowPath} \
     patterns when selecting a directory and all its contents.",
);

/// `nocreationpartial`.
pub static NO_CREATION_PARTIAL: Pred = Pred::new(
    "nocreationpartial",
    Category::Advanced(Topic::Sync),
    "add a pattern to the nocreationpartial list",
    "Including the preference \
     \\texttt{nocreationpartial = \\ARG{PATHSPEC} ->  \\ARG{root}} prevents \
     Unison from performing any file creation in \\ARG{PATHSPEC} \
     on root \\ARG{root} (see \\sectionref{pathspec}{Path Specification} \
     for more information). \
     It is recommended to use {\\tt BelowPath} \
     patterns when selecting a directory and all its contents.",
);

/// `maxsizethreshold`.
pub static MAX_SIZE_THRESHOLD: PrefDef<i64> = PrefDef::new(
    "maxsizethreshold",
    Category::Advanced(Topic::General),
    || -1,
    "prevent transfer of files bigger than this (if >=0, in Kb)",
    "A number indicating above what filesize (in kilobytes) Unison should \
     flag a conflict instead of transferring the file. \
     This conflict remains even in the presence of force or prefer options. \
     A negative number will allow every transfer independently of the size.  \
     The default is -1. ",
);

/// Registers this module's preferences, in OCaml's order.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&FORCE_ROOT)?;
    FORCE_ROOT_PARTIAL.register(prefs)?;
    prefs.register(&PREFER_ROOT)?;
    PREFER_ROOT_PARTIAL.register(prefs)?;
    prefs.register(&NO_DELETION)?;
    prefs.register(&NO_UPDATE)?;
    prefs.register(&NO_CREATION)?;
    NO_DELETION_PARTIAL.register(prefs)?;
    NO_UPDATE_PARTIAL.register(prefs)?;
    NO_CREATION_PARTIAL.register(prefs)?;
    prefs.register(&MAX_SIZE_THRESHOLD)
}

fn internal(what: &str) -> Error {
    Error::Fatal(format!("Internal error: recon: {what}"))
}

// ---- Prefer and force ----

/// A direction to set ([`set_direction`]), or what `prefer` and the like
/// name ([`root_to_direction`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// The older version (by modification time).
    Older,
    /// The newer version.
    Newer,
    /// Merge (if the path is one to merge).
    Merge,
    /// From the first replica to the second.
    Replica1ToReplica2,
    /// From the second replica to the first.
    Replica2ToReplica1,
}

/// Whether a [`Choice`] overrides only conflicts or every difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strength {
    /// Every difference (`force`).
    Force,
    /// Only conflicts (`prefer`).
    Prefer,
}

/// `setDirection`: sets the item's direction, if its default is a
/// conflict or `strength` is [`Strength::Force`]. Merging needs a path to
/// merge, and older and newer need different modification times (or a
/// deletion, which they only override in conflicts).
pub fn set_direction(
    prefs: &Prefs,
    ri: &mut ReconItem,
    dir: Choice,
    strength: Strength,
) -> Result<(), Error> {
    let merge = globals::MERGE.compiled(prefs, case::mode(prefs))?;
    set_direction_with(ri, dir, strength, &merge);
    Ok(())
}

fn set_direction_with(ri: &mut ReconItem, dir: Choice, strength: Strength, merge: &Compiled) {
    let Replicas::Different(diff) = &mut ri.replicas else {
        return;
    };
    let conflict = diff.default_direction.is_conflict();
    if strength != Strength::Force && !conflict {
        return;
    }
    match dir {
        Choice::Replica1ToReplica2 => diff.direction = Direction::Replica1ToReplica2,
        Choice::Replica2ToReplica1 => diff.direction = Direction::Replica2ToReplica1,
        Choice::Merge => {
            if merge.test(ri.path1.as_bytes()) {
                diff.direction = Direction::Merge;
            }
        }
        Choice::Older | Choice::Newer => {
            // A move compares the contents it carries.
            let (rc1, rc2) = match (&diff.rc1.status, &diff.rc2.status) {
                (Status::MovedOut(_, rc1, _), _) => (&**rc1, &diff.rc2),
                (_, Status::MovedOut(_, rc2, _)) => (&diff.rc1, &**rc2),
                _ => (&diff.rc1, &diff.rc2),
            };
            let direction = match (&rc1.status, &rc2.status) {
                (Status::Deleted, _) => conflict.then_some(Direction::Replica2ToReplica1),
                (_, Status::Deleted) => conflict.then_some(Direction::Replica1ToReplica2),
                _ => {
                    // Equal times leave the item alone.
                    let comp = rc1.desc.time() - rc2.desc.time();
                    let comp = if dir == Choice::Newer { -comp } else { comp };
                    if comp == 0. {
                        None
                    } else if comp < 0. {
                        Some(Direction::Replica1ToReplica2)
                    } else {
                        Some(Direction::Replica2ToReplica1)
                    }
                }
            };
            if let Some(d) = direction {
                diff.direction = d;
            }
        }
    }
}

/// `revertToDefaultDirection`.
pub fn revert_to_default_direction(ri: &mut ReconItem) {
    if let Replicas::Different(diff) = &mut ri.replicas {
        diff.direction = diff.default_direction.clone();
    }
}

/// `root2direction`: the direction that makes `root` the truth: `older`,
/// `newer`, a root (as given), or a unique prefix or suffix of a root's
/// path or a unique prefix of its host. `None` for the empty string.
pub fn root_to_direction(prefs: &Prefs, root: &str) -> Result<Option<Choice>, Error> {
    use gemel_base::clroot::Clroot;
    match root {
        "older" => return Ok(Some(Choice::Older)),
        "newer" => return Ok(Some(Choice::Newer)),
        "" => return Ok(None),
        _ => {}
    }
    let (r1, r2) = globals::raw_root_pair(prefs)?;
    if r1 == root {
        return Ok(Some(Choice::Replica1ToReplica2));
    }
    if r2 == root {
        return Ok(Some(Choice::Replica2ToReplica1));
    }
    let affix = |p: &str| p.starts_with(root) || p.ends_with(root);
    let partial_match = |r: &Clroot| match r {
        Clroot::ConnectLocal(None) => false,
        Clroot::ConnectLocal(Some(p)) => !p.is_empty() && affix(p),
        Clroot::ConnectByShell {
            host,
            path: Some(p),
            ..
        }
        | Clroot::ConnectBySocket {
            host,
            path: Some(p),
            ..
        } => affix(p) || host.starts_with(root),
        Clroot::ConnectByShell { host, .. } | Clroot::ConnectBySocket { host, .. } => {
            host.starts_with(root)
        }
    };
    let matches: Vec<bool> = globals::parsed_cl_raw_roots(prefs)?
        .iter()
        .map(partial_match)
        .collect();
    match matches[..] {
        [true, false] => Ok(Some(Choice::Replica1ToReplica2)),
        [false, true] => Ok(Some(Choice::Replica2ToReplica1)),
        _ => Err(Error::Fatal(format!(
            "{root}\nis not uniquely identifying one of the current roots:\n  {r1}\n  {r2}"
        ))),
    }
}

/// `prefRoot`: [`root_to_direction`], remembered (`rootDirCache`, cleared
/// for each override).
struct RootDirs<'p> {
    prefs: &'p Prefs,
    cache: HashMap<String, Option<Choice>>,
}

impl<'p> RootDirs<'p> {
    fn new(prefs: &'p Prefs) -> Self {
        RootDirs {
            prefs,
            cache: HashMap::new(),
        }
    }

    fn get(&mut self, root: &str) -> Result<Option<Choice>, Error> {
        if let Some(d) = self.cache.get(root) {
            return Ok(*d);
        }
        let d = root_to_direction(self.prefs, root)?;
        self.cache.insert(root.to_owned(), d);
        Ok(d)
    }
}

/// What propagating a difference does to the target (`actionKind`):
/// a different type that isn't absent counts as a deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionKind {
    Deletion,
    Update,
    Creation,
}

fn action_kind(from: &ReplicaContent, to: &ReplicaContent) -> ActionKind {
    if from.typ == to.typ {
        ActionKind::Update
    } else if to.typ == Typ::Absent {
        ActionKind::Creation
    } else {
        ActionKind::Deletion
    }
}

/// The overrides of one run: the root lookups and the preferences.
struct Overrides<'p> {
    roots: RootDirs<'p>,
    mode: Mode,
    merge: Arc<Compiled>,
    max_size_threshold: i64,
}

impl<'p> Overrides<'p> {
    fn new(prefs: &'p Prefs) -> Result<Self, Error> {
        let mode = case::mode(prefs);
        Ok(Overrides {
            roots: RootDirs::new(prefs),
            mode,
            merge: globals::MERGE.compiled(prefs, mode)?,
            max_size_threshold: *prefs.read(&MAX_SIZE_THRESHOLD),
        })
    }

    fn prefs(&self) -> &'p Prefs {
        self.roots.prefs
    }

    /// `lookupPreferredRoot`.
    fn preferred_root(&mut self) -> Result<(Option<Choice>, Strength), Error> {
        let prefs = self.prefs();
        let force = prefs.read(&FORCE_ROOT);
        let prefer = prefs.read(&PREFER_ROOT);
        if !force.is_empty() {
            Ok((self.roots.get(force)?, Strength::Force))
        } else if !prefer.is_empty() {
            Ok((self.roots.get(prefer)?, Strength::Prefer))
        } else {
            Ok((None, Strength::Prefer))
        }
    }

    /// `lookupPreferredRootPartial`.
    fn preferred_root_partial(
        &mut self,
        p: &Path<Global>,
    ) -> Result<(Option<Choice>, Strength), Error> {
        let s = p.as_bytes();
        for (pred, strength) in [
            (&FORCE_ROOT_PARTIAL, Strength::Force),
            (&PREFER_ROOT_PARTIAL, Strength::Prefer),
        ] {
            let c = pred.compiled(self.prefs(), self.mode)?;
            if c.test(s) {
                // OCaml's Pred.assoc fails (Not_found) for a pattern without
                // a root.
                let root = c
                    .assoc(s)
                    .ok_or_else(|| internal("a pattern without a root"))?;
                return Ok((self.roots.get(root)?, strength));
            }
        }
        Ok((None, Strength::Prefer))
    }

    /// `testCancelPref` or `testPartialCancelPref`: whether a `no*`
    /// preference protects `root` from this kind of action at `path`.
    fn cancels(&mut self, root: Choice, path: &[u8], kind: ActionKind) -> Result<bool, Error> {
        let (pref, partial) = match kind {
            ActionKind::Deletion => (&NO_DELETION, &NO_DELETION_PARTIAL),
            ActionKind::Update => (&NO_UPDATE, &NO_UPDATE_PARTIAL),
            ActionKind::Creation => (&NO_CREATION, &NO_CREATION_PARTIAL),
        };
        for r in self.prefs().read(pref) {
            if self.roots.get(r)? == Some(root) {
                return Ok(true);
            }
        }
        let partial = partial.compiled(self.prefs(), self.mode)?;
        for r in partial.assoc_all(path) {
            if self.roots.get(r)? == Some(root) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `shouldCancel`: whether propagating `from` over `to` onto `root` is
    /// forbidden, and why.
    fn should_cancel(
        &mut self,
        path: &[u8],
        from: &ReplicaContent,
        to: &ReplicaContent,
        root: Choice,
    ) -> Result<(bool, &'static str), Error> {
        const UPDATE: &str = "would update a file with noupdate or noupdatepartial set";
        const SIZE: &str = "would transfer a file of size greater than maxsizethreshold";
        let max = self.max_size_threshold;
        let too_big =
            |rc: &ReplicaContent| max >= 0 && rc.desc.length() >= 1000i64.wrapping_mul(max);
        Ok(match action_kind(from, to) {
            ActionKind::Update => {
                if self.cancels(root, path, ActionKind::Update)? {
                    (true, UPDATE)
                } else {
                    (too_big(from), SIZE)
                }
            }
            ActionKind::Deletion => {
                if self.cancels(root, path, ActionKind::Update)? {
                    (true, UPDATE)
                } else {
                    (
                        self.cancels(root, path, ActionKind::Deletion)?,
                        "would delete a file with nodeletion or nodeletionpartial set",
                    )
                }
            }
            ActionKind::Creation => {
                if self.cancels(root, path, ActionKind::Creation)? {
                    (
                        true,
                        "would create a file with nocreation or nocreationpartial set",
                    )
                } else {
                    (too_big(from), SIZE)
                }
            }
        })
    }

    /// `filterRi`: makes the item a conflict if a `no*` preference or
    /// `maxsizethreshold` forbids its direction.
    fn filter(&mut self, ri: &mut ReconItem) -> Result<(), Error> {
        let Replicas::Different(diff) = &mut ri.replicas else {
            return Ok(());
        };
        let path = ri.path1.as_bytes();
        let (cancel, reason) = match diff.direction {
            Direction::Replica1ToReplica2 => {
                self.should_cancel(path, &diff.rc1, &diff.rc2, Choice::Replica2ToReplica1)?
            }
            Direction::Replica2ToReplica1 => {
                self.should_cancel(path, &diff.rc2, &diff.rc1, Choice::Replica1ToReplica2)?
            }
            Direction::Conflict(_) | Direction::Merge => (false, ""),
        };
        if cancel {
            diff.direction = Direction::Conflict(reason.to_owned());
        }
        Ok(())
    }
}

/// `overrideReconcilerChoices`: applies `force` or `prefer`, then
/// `forcepartial` or `preferpartial`, then the `no*` preferences and
/// `maxsizethreshold`.
pub fn override_reconciler_choices(prefs: &Prefs, ris: &mut [ReconItem]) -> Result<(), Error> {
    let mut o = Overrides::new(prefs)?;
    let (dir, strength) = o.preferred_root()?;
    if let Some(dir) = dir {
        for ri in ris.iter_mut() {
            set_direction_with(ri, dir, strength, &o.merge);
        }
    }
    for ri in ris.iter_mut() {
        let (dir, strength) = o.preferred_root_partial(&ri.path1)?;
        if let Some(dir) = dir {
            set_direction_with(ri, dir, strength, &o.merge);
        }
    }
    for ri in ris.iter_mut() {
        o.filter(ri)?;
    }
    Ok(())
}

/// `checkThatPreferredRootIsValid`: checks the roots that `force`,
/// `prefer`, the `no*` preferences and their partial forms name, before
/// the run does anything long. The `no*` ones must name a root.
pub fn check_that_preferred_root_is_valid(prefs: &Prefs) -> Result<(), Error> {
    let mut roots = RootDirs::new(prefs);
    let mode = case::mode(prefs);
    let mut test_root = |explicit: bool, name: &str, value: &str| -> Result<(), Error> {
        let d = roots.get(value).map_err(|e| match e {
            Error::Fatal(err) => Error::Fatal(format!("Argument to preference '{name}': {err}")),
            e => e,
        })?;
        match d {
            None | Some(Choice::Replica1ToReplica2 | Choice::Replica2ToReplica1) => Ok(()),
            Some(Choice::Newer | Choice::Older) if explicit => Err(Error::Fatal(format!(
                "Argument to preference '{name}': {value} must not be keyword 'older' or 'newer'."
            ))),
            // As in OCaml, only older checks that times are synchronised.
            Some(Choice::Newer) => Ok(()),
            Some(Choice::Older) => {
                if *prefs.read(&gemel_fs::props::TIMES) {
                    Ok(())
                } else {
                    Err(Error::Transient(format!(
                        "The '{name}=older' preference can only be used with 'times=true'"
                    )))
                }
            }
            Some(Choice::Merge) => Err(internal("a root named merge")),
        }
    };
    for (pref, name) in [(&FORCE_ROOT, "force"), (&PREFER_ROOT, "prefer")] {
        test_root(false, name, prefs.read(pref))?;
    }
    for pred in [&FORCE_ROOT_PARTIAL, &PREFER_ROOT_PARTIAL] {
        for v in pred.compiled(prefs, mode)?.associated_strings() {
            test_root(false, pred.pref().name(), v)?;
        }
    }
    for pref in [&NO_DELETION, &NO_UPDATE, &NO_CREATION] {
        for v in prefs.read(pref) {
            test_root(true, pref.name(), v)?;
        }
    }
    for pred in [
        &NO_DELETION_PARTIAL,
        &NO_UPDATE_PARTIAL,
        &NO_CREATION_PARTIAL,
    ] {
        for v in pred.compiled(prefs, mode)?.associated_strings() {
            test_root(true, pred.pref().name(), v)?;
        }
    }
    Ok(())
}

// ---- Reconciliation ----

/// `checkForError`: the first error (not one `links = false` hides).
fn first_error<'a>(ui: &'a UpdateItem, fs: &FsConfig) -> Option<&'a str> {
    match ui {
        UpdateItem::NoUpdates => None,
        UpdateItem::Error(err) => (!fileinfo::should_ignore(err, fs)).then_some(err.as_str()),
        UpdateItem::Updates(UpdateContent::Dir(_, children, _, _), _) => {
            children.iter().find_map(|(_, ui)| first_error(ui, fs))
        }
        UpdateItem::Updates(..) => None,
    }
}

/// `collectErrors`: the errors, in order.
fn collect_errors(ui: &UpdateItem, fs: &FsConfig, errors: &mut Vec<String>) {
    match ui {
        UpdateItem::Error(err) => {
            if !fileinfo::should_ignore(err, fs) {
                errors.push(err.clone());
            }
        }
        UpdateItem::Updates(UpdateContent::Dir(_, children, _, _), _) => {
            for (_, ui) in children {
                collect_errors(ui, fs, errors);
            }
        }
        _ => {}
    }
}

/// Which side [`Recon::reconcile_no_conflict`] has the updates of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Updated {
    Rep1,
    Rep2,
}

/// The equal paths, and how many there are in the current batch
/// (`add_equal`'s counter).
type Equals = Tree<(Name, Name), (UpdateContent, UpdateContent)>;

/// How many equal paths make a batch.
const EQUAL_BATCH: usize = 5000;

/// One reconciliation walk.
struct Recon<'a> {
    allow_partial: bool,
    mode: Mode,
    fs: FsConfig,
    atomic: Arc<Compiled>,
    merge: Arc<Compiled>,
    archive: &'a LocalArchive,
    moves: Moves,
    equals: Builder<(Name, Name), (UpdateContent, UpdateContent)>,
    unequals: Builder<(Name, Name), Replicas>,
    /// The batches of equal paths so far.
    batches: Vec<Equals>,
    counter: usize,
    archive_updated: bool,
}

impl Recon<'_> {
    /// `propagateErrors`: lifts errors in the updates to a problem, or
    /// (with `allow_partial`) records them in the difference.
    fn propagate_errors(&self, rplc: Replicas) -> Replicas {
        match rplc {
            Replicas::Problem(_) => rplc,
            Replicas::Different(diff) if self.allow_partial => {
                let mut errors1 = Vec::new();
                let mut errors2 = Vec::new();
                collect_errors(&diff.rc1.ui, &self.fs, &mut errors1);
                collect_errors(&diff.rc2.ui, &self.fs, &mut errors2);
                Replicas::Different(Difference {
                    errors1,
                    errors2,
                    ..diff
                })
            }
            Replicas::Different(diff) => {
                if let Some(err) = first_error(&diff.rc1.ui, &self.fs) {
                    Replicas::Problem(format!("[root 1]: {err}"))
                } else if let Some(err) = first_error(&diff.rc2.ui, &self.fs) {
                    Replicas::Problem(format!("[root 2]: {err}"))
                } else {
                    Replicas::Different(diff)
                }
            }
        }
    }

    /// `Moves.addHint`, then adds the item at the current path.
    fn add_hinted(&mut self, path: &Path<Global>, rplc: Replicas) -> Result<(), Error> {
        self.moves.add_hint(self.archive, self.mode, path, &rplc)?;
        self.unequals.add(rplc);
        Ok(())
    }

    /// `skipIgnored`: a problem for the error `s`, unless `links = false`
    /// hides it (and the other side is a link, unchanged, or hidden too).
    fn skip_ignored(&mut self, s: &str, other: &UpdateItem) {
        if !fileinfo::should_ignore(s, &self.fs) {
            self.unequals.add(Replicas::Problem(s.to_owned()));
            return;
        }
        match other {
            UpdateItem::Error(s2) => {
                if !fileinfo::should_ignore(s2, &self.fs) {
                    self.unequals.add(Replicas::Problem(s2.clone()));
                }
            }
            UpdateItem::NoUpdates | UpdateItem::Updates(UpdateContent::Symlink(_), _) => {}
            UpdateItem::Updates(..) => self.unequals.add(Replicas::Problem(
                "Syncing symbolic links is disabled, but this path represents a \
                 symbolic link in one of the replicas and a non-link in the other \
                 replica."
                    .to_owned(),
            )),
        }
    }

    /// `update2replicaContent`: the contents `uc_new` (from `ui`) as a
    /// replica content. In a conflict, a directory whose contents changed
    /// shows as modified rather than unchanged.
    fn replica_content(
        &self,
        path: &Path<Global>,
        conflict: bool,
        ui: &UpdateItem,
        props: &[Props],
        uc_new: &UpdateContent,
        old_type: Typ,
    ) -> Result<ReplicaContent, Error> {
        let (n, size) = self.archive.update_size(path, ui)?;
        let (typ, status, desc) = match uc_new {
            UpdateContent::Absent => (Typ::Absent, Status::Deleted, Props::dummy()),
            UpdateContent::File(desc, ContentsChange::Same) => {
                (Typ::File, Status::PropsChanged, desc.clone())
            }
            UpdateContent::File(desc, _) if old_type != Typ::File => {
                (Typ::File, Status::Created, desc.clone())
            }
            UpdateContent::File(desc, ContentsChange::Updated(..)) => {
                (Typ::File, Status::Modified, desc.clone())
            }
            UpdateContent::Symlink(_) if old_type != Typ::Symlink => {
                (Typ::Symlink, Status::Created, Props::dummy())
            }
            UpdateContent::Symlink(_) => (Typ::Symlink, Status::Modified, Props::dummy()),
            UpdateContent::Dir(desc, ..) if old_type != Typ::Directory => {
                (Typ::Directory, Status::Created, desc.clone())
            }
            UpdateContent::Dir(desc, _, PermChange::Updated, _) => {
                (Typ::Directory, Status::PropsChanged, desc.clone())
            }
            UpdateContent::Dir(desc, _, PermChange::Same, _) if conflict => {
                (Typ::Directory, Status::Modified, desc.clone())
            }
            UpdateContent::Dir(desc, _, PermChange::Same, _) => {
                (Typ::Directory, Status::Unchanged, desc.clone())
            }
        };
        Ok(ReplicaContent {
            typ,
            status,
            desc,
            ui: ui.clone(),
            size: (n as i64, size),
            props: props.to_vec(),
        })
    }

    /// `describeUpdate`: the updated side's contents, and the other's,
    /// unchanged since the archive.
    fn describe_update(
        &self,
        path: &Path<Global>,
        other_props: &[Props],
        ui: &UpdateItem,
        props: &[Props],
    ) -> Result<(ReplicaContent, ReplicaContent), Error> {
        let UpdateItem::Updates(uc, prev) = ui else {
            return Err(internal("describing a path without updates"));
        };
        let typ = old_type(prev);
        let (n, size) = self.archive.update_size(path, &UpdateItem::NoUpdates)?;
        Ok((
            self.replica_content(path, false, ui, props, uc, typ)?,
            ReplicaContent {
                typ,
                status: Status::Unchanged,
                desc: old_desc(prev),
                ui: UpdateItem::NoUpdates,
                size: (n as i64, size),
                props: other_props.to_vec(),
            },
        ))
    }

    /// `reconcileNoConflict`: the items where only one side changed.
    /// `other_props` are the props of the unchanged side.
    fn reconcile_no_conflict(
        &mut self,
        path: &Path<Global>,
        other_props: &[Props],
        ui: &UpdateItem,
        props: &[Props],
        updated: Updated,
    ) -> Result<(), Error> {
        let different = |r: &Self| -> Result<Replicas, Error> {
            let (changed, unchanged) = r.describe_update(path, other_props, ui, props)?;
            Ok(Replicas::Different(match updated {
                Updated::Rep2 => Difference {
                    rc1: unchanged,
                    rc2: changed,
                    direction: Direction::Replica2ToReplica1,
                    default_direction: Direction::Replica2ToReplica1,
                    errors1: Vec::new(),
                    errors2: Vec::new(),
                },
                Updated::Rep1 => Difference {
                    rc1: changed,
                    rc2: unchanged,
                    direction: Direction::Replica1ToReplica2,
                    default_direction: Direction::Replica1ToReplica2,
                    errors1: Vec::new(),
                    errors2: Vec::new(),
                },
            }))
        };
        match ui {
            UpdateItem::NoUpdates => {}
            UpdateItem::Error(err) => self.skip_ignored(err, &UpdateItem::NoUpdates),
            UpdateItem::Updates(UpdateContent::Dir(_, children, perm, _), PrevState::Dir(_)) => {
                // The directory's own item, if its props changed (neither
                // its errors nor a move hint: it isn't created or deleted).
                if *perm != PermChange::Same {
                    let d = different(self)?;
                    self.unequals.add(d);
                }
                for (name, child) in children {
                    self.unequals.enter((name.clone(), name.clone()));
                    self.reconcile_no_conflict(&path.child(name), &[], child, &[], updated)?;
                    self.unequals.leave();
                }
            }
            UpdateItem::Updates(..) => {
                let d = different(self)?;
                let d = self.propagate_errors(d);
                self.add_hinted(path, d)?;
            }
        }
        Ok(())
    }

    /// `add_equal`: records an equal path, and starts a new batch every
    /// 5000.
    fn add_equal(&mut self, v: (UpdateContent, UpdateContent)) {
        self.equals.add(v);
        self.counter += 1;
        self.archive_updated = true;
        if self.counter == EQUAL_BATCH {
            self.counter = 0;
            let batch = self.equals.slice();
            self.batches.push(batch);
        }
    }

    /// `reconcile`: the items at `path` and below, given both sides'
    /// updates and the props of their parents.
    fn reconcile(
        &mut self,
        path: &Path<Global>,
        ui1: &UpdateItem,
        props1: &[Props],
        ui2: &UpdateItem,
        props2: &[Props],
    ) -> Result<(), Error> {
        use UpdateContent::{Absent, Dir, File, Symlink};
        use UpdateItem::{NoUpdates, Updates};
        // A conflict (made a move candidate), or a merge.
        let different = |r: &mut Self,
                         uc1: &UpdateContent,
                         uc2: &UpdateContent,
                         direction: Direction,
                         old_type: Typ|
         -> Result<(), Error> {
            let hint = direction != Direction::Merge;
            let d = Replicas::Different(Difference {
                rc1: r.replica_content(path, true, ui1, props1, uc1, old_type)?,
                rc2: r.replica_content(path, true, ui2, props2, uc2, old_type)?,
                default_direction: direction.clone(),
                direction,
                errors1: Vec::new(),
                errors2: Vec::new(),
            });
            let d = r.propagate_errors(d);
            if hint {
                r.add_hinted(path, d)
            } else {
                r.unequals.add(d);
                Ok(())
            }
        };
        let conflict = |reason: &str| Direction::Conflict(reason.to_owned());
        match (ui1, ui2) {
            (UpdateItem::Error(s), _) => self.skip_ignored(s, ui2),
            (_, UpdateItem::Error(s)) => self.skip_ignored(s, ui1),
            (NoUpdates, _) => {
                self.reconcile_no_conflict(path, props1, ui2, props2, Updated::Rep2)?
            }
            (_, NoUpdates) => {
                self.reconcile_no_conflict(path, props2, ui1, props1, Updated::Rep1)?
            }
            (Updates(Absent, _), Updates(Absent, _)) => self.add_equal((Absent, Absent)),
            (
                Updates(uc1 @ Dir(desc1, children1, perm1, _), _),
                Updates(uc2 @ Dir(desc2, children2, perm2, _), _),
            ) => {
                if self.atomic.test(path.as_bytes()) {
                    let action = conflict("atomic directory");
                    let d = Replicas::Different(Difference {
                        rc1: self.replica_content(path, true, ui1, &[], uc1, Typ::Directory)?,
                        rc2: self.replica_content(path, true, ui2, &[], uc2, Typ::Directory)?,
                        default_direction: action.clone(),
                        direction: action,
                        errors1: Vec::new(),
                        errors2: Vec::new(),
                    });
                    self.unequals.add(d);
                    return Ok(());
                }
                // The directory's own item, if its props changed.
                let changed = *perm1 != PermChange::Same || *perm2 != PermChange::Same;
                if changed && desc1.similar(desc2, &self.fs.props) {
                    self.add_equal((
                        Dir(desc1.clone(), Vec::new(), PermChange::Same, false),
                        Dir(desc2.clone(), Vec::new(), PermChange::Same, false),
                    ));
                } else if changed {
                    let action = if *perm1 == PermChange::Same {
                        Direction::Replica2ToReplica1
                    } else if *perm2 == PermChange::Same {
                        Direction::Replica1ToReplica2
                    } else {
                        conflict("properties changed on both sides")
                    };
                    let d = Replicas::Different(Difference {
                        rc1: self.replica_content(path, false, ui1, &[], uc1, Typ::Directory)?,
                        rc2: self.replica_content(path, false, ui2, &[], uc2, Typ::Directory)?,
                        default_direction: action.clone(),
                        direction: action,
                        errors1: Vec::new(),
                        errors2: Vec::new(),
                    });
                    self.unequals.add(d);
                }
                for (name1, ui1, name2, ui2) in combine_children(children1, children2, self.mode) {
                    let arc = (name1.clone(), name2.clone());
                    self.equals.enter(arc.clone());
                    self.unequals.enter(arc);
                    self.reconcile(&path.child(name1), ui1, &[], ui2, &[])?;
                    self.equals.leave();
                    self.unequals.leave();
                }
            }
            (Updates(uc1 @ File(desc1, change1), prev), Updates(uc2 @ File(desc2, change2), _)) => {
                let old = old_type(prev);
                let similar = desc1.similar(desc2, &self.fs.props);
                match (change1, change2) {
                    (ContentsChange::Updated(dig1, ..), ContentsChange::Updated(dig2, ..))
                        if dig1 == dig2 =>
                    {
                        if similar {
                            self.add_equal((uc1.clone(), uc2.clone()));
                        } else {
                            // The same new contents: show the props as
                            // changed rather than the files as new (which
                            // transport expects too).
                            let uc1 = File(desc1.clone(), ContentsChange::Same);
                            let uc2 = File(desc2.clone(), ContentsChange::Same);
                            different(
                                self,
                                &uc1,
                                &uc2,
                                conflict("properties changed on both sides"),
                                old,
                            )?;
                        }
                    }
                    (ContentsChange::Same, ContentsChange::Same) if similar => {
                        self.add_equal((uc1.clone(), uc2.clone()));
                    }
                    (ContentsChange::Same, ContentsChange::Same) => different(
                        self,
                        uc1,
                        uc2,
                        conflict("properties changed on both sides"),
                        old,
                    )?,
                    (ContentsChange::Updated(..), ContentsChange::Updated(..))
                        if self.merge.test(path.as_bytes()) =>
                    {
                        different(self, uc1, uc2, Direction::Merge, old)?;
                    }
                    _ => different(
                        self,
                        uc1,
                        uc2,
                        conflict("contents changed on both sides"),
                        old,
                    )?,
                }
            }
            (Updates(uc1 @ Symlink(l1), prev), Updates(uc2 @ Symlink(l2), _)) => {
                if l1 == l2 {
                    self.add_equal((uc1.clone(), uc2.clone()));
                } else {
                    different(
                        self,
                        uc1,
                        uc2,
                        conflict("symbolic links changed on both sides"),
                        old_type(prev),
                    )?;
                }
            }
            (Updates(uc1, prev), Updates(uc2, _)) => {
                different(
                    self,
                    uc1,
                    uc2,
                    conflict("conflicting updates"),
                    old_type(prev),
                )?;
            }
        }
        Ok(())
    }
}

/// `oldType`.
fn old_type(prev: &PrevState) -> Typ {
    match prev {
        PrevState::File(..) => Typ::File,
        PrevState::Dir(_) => Typ::Directory,
        PrevState::Symlink => Typ::Symlink,
        PrevState::New => Typ::Absent,
    }
}

/// `oldDesc`.
fn old_desc(prev: &PrevState) -> Props {
    match prev {
        PrevState::File(desc, ..) | PrevState::Dir(desc) => desc.clone(),
        PrevState::Symlink | PrevState::New => Props::dummy(),
    }
}

/// A child in [`combine_children`]: each side's name and updates.
type Combined<'a> = (&'a Name, &'a UpdateItem, &'a Name, &'a UpdateItem);

/// `combineChildren`: merges both sides' children (each sorted by name),
/// with [`UpdateItem::NoUpdates`] for a name only one side has.
fn combine_children<'a>(
    children1: &'a [(Name, UpdateItem)],
    children2: &'a [(Name, UpdateItem)],
    mode: Mode,
) -> Vec<Combined<'a>> {
    const NONE: &UpdateItem = &UpdateItem::NoUpdates;
    let mut result = Vec::with_capacity(children1.len().max(children2.len()));
    let (mut i, mut j) = (0, 0);
    while i < children1.len() && j < children2.len() {
        let (n1, ui1) = (&children1[i].0, &children1[i].1);
        let (n2, ui2) = (&children2[j].0, &children2[j].1);
        match n1.compare_in(n2, mode) {
            std::cmp::Ordering::Equal => {
                result.push((n1, ui1, n2, ui2));
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => {
                result.push((n1, ui1, n1, NONE));
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                result.push((n2, NONE, n2, ui2));
                j += 1;
            }
        }
    }
    result.extend(children1[i..].iter().map(|(n, ui)| (n, ui, n, NONE)));
    result.extend(children2[j..].iter().map(|(n, ui)| (n, NONE, n, ui)));
    result
}

/// `enterPath`: goes down both paths (equal up to case).
fn enter_path<B>(
    p1: &Path<Global>,
    p2: &Path<Global>,
    t: &mut Builder<(Name, Name), B>,
) -> Result<(), Error> {
    let (mut p1, mut p2) = (p1.clone(), p2.clone());
    loop {
        match (p1.deconstruct(), p2.deconstruct()) {
            (None, None) => return Ok(()),
            (Some((n1, r1)), Some((n2, r2))) => {
                t.enter((n1, n2));
                (p1, p2) = (r1, r2);
            }
            _ => return Err(internal("paths of different lengths")),
        }
    }
}

/// `leavePath`.
fn leave_path<B>(p: &Path<Global>, t: &mut Builder<(Name, Name), B>) {
    let mut p = p.clone();
    while let Some((_, rest)) = p.deconstruct() {
        t.leave();
        p = rest;
    }
}

/// Both replicas' paths to a child.
fn child_paths(
    (p1, p2): &(Path<Global>, Path<Global>),
    (n1, n2): &(Name, Name),
) -> (Path<Global>, Path<Global>) {
    (p1.child(n1), p2.child(n2))
}

/// `dangerousPath`: one side has been emptied but not the other.
fn dangerous_path(ui1: &UpdateItem, ui2: &UpdateItem) -> bool {
    let emptied = |ui: &UpdateItem| match ui {
        UpdateItem::Updates(UpdateContent::Absent, _) => true,
        UpdateItem::Updates(UpdateContent::Dir(_, _, _, empty), _) => *empty,
        _ => false,
    };
    emptied(ui1) != emptied(ui2)
}

/// `reconcileAll`: the items to propagate for each path's updates on both
/// roots, sorted and with the preferences' overrides; whether some path
/// was updated the same way on both roots (in the last batch of them, as
/// in OCaml); and the paths emptied on one side only, last first.
///
/// The paths updated the same way are recorded in the archives, which are
/// committed. With `allow_partial`, errors deep in a directory are kept in
/// its item rather than making it a problem.
pub async fn reconcile_all(
    rpc: &Rpc,
    roots: &Roots,
    allow_partial: bool,
    updates: Vec<PathUpdates>,
) -> Result<(Vec<ReconItem>, bool, Vec<Path<Global>>), Error> {
    crate::with_trace(rpc, |t, p| t.status(p, "Reconciling changes"))?;
    crate::debug(rpc.client_host(), "recon", || "reconcileAll\n".to_owned());
    let archive = LocalArchive::new(rpc, roots)?;
    let (unequals, batches, equals, archive_updated, mut moves, dangerous) = {
        let client = rpc.client_host();
        let prefs = client.prefs().borrow();
        let mode = case::mode(&prefs);
        let mut fs = FsConfig::from_prefs(&prefs);
        fs.props = fs.props.with_features(&client.features().borrow());
        let mut r = Recon {
            allow_partial,
            mode,
            fs,
            atomic: globals::ATOMIC.compiled(&prefs, mode)?,
            merge: globals::MERGE.compiled(&prefs, mode)?,
            archive: &archive,
            moves: Moves::new(*prefs.read(&moves::DETECT_MOVES)),
            equals: Builder::start(),
            unequals: Builder::start(),
            batches: Vec::new(),
            counter: 0,
            archive_updated: false,
        };
        drop(prefs);
        let mut dangerous = Vec::new();
        for ((path1, ui1, props1), (path2, ui2, props2)) in &updates {
            // Global, as they get names from the other replica.
            let path1 = path1.clone().make_global();
            let path2 = path2.clone().make_global();
            enter_path(&path1, &path2, &mut r.equals)?;
            enter_path(&path1, &path2, &mut r.unequals)?;
            r.reconcile(&path1, ui1, props1, ui2, props2)?;
            leave_path(&path1, &mut r.equals);
            leave_path(&path1, &mut r.unequals);
            if dangerous_path(ui1, ui2) {
                dangerous.push(path1);
            }
        }
        dangerous.reverse();
        let unequals = r.unequals.finish();
        crate::debug(client, "recon", || {
            format!("reconcile: {} results\n", unequals.size())
        });
        (
            unequals,
            r.batches,
            r.equals.finish(),
            r.archive_updated,
            r.moves,
            dangerous,
        )
    };
    for batch in &batches {
        update::mark_equal(rpc, roots, batch).await?;
    }
    update::mark_equal(rpc, roots, &equals).await?;
    // Commit the archive updates so far.
    if archive_updated {
        update::commit_updates(rpc, roots).await?;
    }
    let flat = unequals.into_flatten((Path::empty(), Path::empty()), &child_paths);
    let prefs = rpc.client_host().prefs().borrow();
    moves.detect(&archive, case::mode(&prefs))?;
    let mut items: Vec<ReconItem> = flat
        .into_iter()
        .filter_map(|((path1, path2), replicas)| {
            moves.r(ReconItem {
                path1,
                path2,
                replicas,
            })
        })
        .collect();
    drop(moves);
    sortri::sort_recon_items(&prefs, &mut items)?;
    override_reconciler_choices(&prefs, &mut items)?;
    Ok((items, !equals.is_empty(), dangerous))
}
