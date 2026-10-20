//! Move and rename detection.
//!
//! Ports Unison `src/moves.ml`.
//!
//! Derived from Unison, Copyright 2021-2023, Tõivo Leedjärv.
//!
//! Reconciliation hints each difference that deletes or creates a file or
//! directory ([`Moves::add_hint`]); [`Moves::detect`] then pairs deletes
//! with creates of the same contents, and [`Moves::r`] replaces each pair
//! by a single move item. OCaml keeps the candidates in global tables reset
//! around each reconciliation; here they are a [`Moves`] value that lives
//! for one.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use gemel_base::case::Mode;
use gemel_base::features::{Features, Id};
use gemel_base::name::Name;
use gemel_base::path::{Global, Path};
use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
use gemel_base::util::Error;
use gemel_compat::ocaml_hash::{self, Value};
use gemel_fs::fileinfo::{Stamp, Typ};
use gemel_fs::os::FullFingerprint;
use gemel_fs::props::Props;

use crate::archive::Archive;
use crate::common::{
    ContentsChange, Difference, Direction, FEATURE_PREV_STATE2, PrevState, ReconItem,
    ReplicaContent, Replicas, Status, UpdateContent, UpdateItem,
};
use crate::update::{self, LocalArchive};

/// The feature for detecting moves (`featMoves`).
pub const FEATURE_MOVES: &str = "Sync: Moves";

/// Registers this module's features.
pub fn register_features(features: &mut Features) -> Result<(), Error> {
    features.register(
        FEATURE_MOVES,
        false,
        Some(Box::new(|feats: &[Id], enabled_this, prefs: &Prefs| {
            (*prefs.read(&DETECT_MOVES)
                && !(enabled_this && feats.iter().any(|f| f == FEATURE_PREV_STATE2)))
            .then(|| {
                "You have requested detection and propagation of moves/renames \
                 (the \"moves\" preference) but the server does not support this."
                    .to_owned()
            })
        })),
    )?;
    Ok(())
}

/// `Moves.enabled`: whether the feature was negotiated.
pub fn enabled(features: &Features) -> bool {
    features.enabled_by_name(FEATURE_MOVES)
}

/// `moves-experimental` (`detectmoves`).
pub static DETECT_MOVES: PrefDef<bool> = PrefDef::new(
    "moves-experimental",
    Category::Advanced(Topic::Sync),
    || false,
    "optimize transfers by detecting renames and moves",
    "When this preference is set, Unison will try to avoid transferring \
     file contents across the network, or making a local copy, by \
     recognizing when a file or a directory has been renamed or moved to \
     a new location.  This usually allows to propagate only the rename, \
     without transferring or copying any data.  The default value is \
     \\texttt{false}.\n\n\
     This feature is currently experimental and may change in incompatible \
     ways in future versions.",
)
.local()
.send(|_, features| enabled(features));

/// Registers this module's preferences.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&DETECT_MOVES)
}

/// A replica, for the candidates kept for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rep {
    One,
    Two,
}

impl Rep {
    fn index(self) -> usize {
        match self {
            Rep::One => 0,
            Rep::Two => 1,
        }
    }

    /// `getRc`.
    fn rc(self, d: &Difference) -> &ReplicaContent {
        match self {
            Rep::One => &d.rc1,
            Rep::Two => &d.rc2,
        }
    }

    /// `getOtherRc`.
    fn other_rc(self, d: &Difference) -> &ReplicaContent {
        match self {
            Rep::One => &d.rc2,
            Rep::Two => &d.rc1,
        }
    }
}

/// `moveCand`: a created or deleted path, with its difference.
#[derive(Debug, Clone)]
struct Cand {
    path: Path<Global>,
    dir: Path<Global>,
    name: Name,
    differ: Difference,
}

/// `moveCands`: the creates and the deletes with the same contents. Both
/// are in OCaml's list order (most recently added first).
#[derive(Debug, Default)]
struct Cands {
    adds: Vec<Cand>,
    deletes: Vec<Cand>,
}

impl Cands {
    fn add(&mut self, added: bool, c: Cand) {
        if added {
            self.adds.insert(0, c);
        } else {
            self.deletes.insert(0, c);
        }
    }
}

/// The move candidates and the moves found in one reconciliation.
#[derive(Debug, Default)]
pub struct Moves {
    /// `detectmoves`.
    detect: bool,
    /// The move items, by the path that keeps one.
    moves: HashMap<Path<Global>, Replicas>,
    /// The paths whose items a move item replaces.
    hides: HashSet<Path<Global>>,
    /// File candidates for each replica, by fingerprint.
    files: [HashMap<FullFingerprint, Cands>; 2],
    /// Directory candidates for each replica, by contents hash.
    dirs: [HashMap<i64, Cands>; 2],
}

fn internal(what: &str) -> Error {
    Error::Fatal(format!("Internal error: moves: {what}"))
}

/// The time [`Moves::detect`] may take before giving up.
const DETECT_TIME_LIMIT: f64 = 15.;

impl Moves {
    /// `Moves.reset`: no candidates; moves are detected if `detect`.
    pub fn new(detect: bool) -> Moves {
        Moves {
            detect,
            ..Moves::default()
        }
    }

    /// `Moves.r`: the move item for `ri`'s path, `ri` itself if it has
    /// none, or `None` if a move item on another path replaces it.
    pub fn r(&mut self, ri: ReconItem) -> Option<ReconItem> {
        match ri.replicas {
            Replicas::Different(_) if self.detect => match self.moves.remove(&ri.path1) {
                Some(replicas) => Some(ReconItem { replicas, ..ri }),
                None => (!self.hides.contains(&ri.path1)).then_some(ri),
            },
            _ => Some(ri),
        }
    }

    /// `Moves.addHint`: records `rplc` at `path` as a move candidate if it
    /// may be one.
    pub fn add_hint(
        &mut self,
        archive: &LocalArchive,
        mode: Mode,
        path: &Path<Global>,
        rplc: &Replicas,
    ) -> Result<(), Error> {
        match rplc {
            Replicas::Different(differ) if self.detect => {
                self.process_hint(archive, mode, Rep::One, path, differ, &differ.rc1)?;
                self.process_hint(archive, mode, Rep::Two, path, differ, &differ.rc2)
            }
            _ => Ok(()),
        }
    }

    /// `processHint`. For a created path, the previous state may be
    /// something else: its deletion is shadowed by the creation, and not
    /// detected as a move (see OCaml's comment).
    fn process_hint(
        &mut self,
        archive: &LocalArchive,
        mode: Mode,
        rep: Rep,
        path: &Path<Global>,
        differ: &Difference,
        rc: &ReplicaContent,
    ) -> Result<(), Error> {
        use UpdateContent::{Absent, Dir, File};
        let UpdateItem::Updates(uc, prev) = &rc.ui else {
            return Ok(());
        };
        match (&rc.status, uc, prev) {
            // A new file: a possible move target.
            (Status::Created, File(_, ContentsChange::Updated(fp, _, _)), _) => {
                if rc.size.1 != 0 {
                    self.add_file_candidate(true, rep, path, fp, differ);
                }
            }
            // A deleted file: a possible move source.
            (Status::Deleted, Absent, PrevState::File(_, fp, _, _)) => {
                self.add_file_candidate(false, rep, path, fp, differ);
            }
            // A new directory: a possible move target.
            (Status::Created, Dir(_, children, _, _), _) => {
                if !children.is_empty() {
                    let hash = hash_archive(&update::make_archive(&rc.ui, mode)?, 0, mode);
                    self.add_dir_candidate(true, rep, path, hash, differ);
                }
            }
            // A deleted directory: a possible move source. Its contents
            // hash the same in both replicas' archives, so the local one
            // will do.
            (Status::Deleted, Absent, PrevState::Dir(_)) => {
                let hash = archive.with(path, |a| hash_archive(a, 0, mode));
                self.add_dir_candidate(false, rep, path, hash, differ);
            }
            _ => {}
        }
        Ok(())
    }

    /// `addFileCandidate`.
    fn add_file_candidate(
        &mut self,
        added: bool,
        rep: Rep,
        path: &Path<Global>,
        fp: &FullFingerprint,
        differ: &Difference,
    ) {
        if fp.is_pseudo() {
            return;
        }
        if let Some(c) = candidate(path, differ) {
            self.files[rep.index()]
                .entry(fp.clone())
                .or_default()
                .add(added, c);
        }
    }

    /// `addDirCandidate`.
    fn add_dir_candidate(
        &mut self,
        added: bool,
        rep: Rep,
        path: &Path<Global>,
        hash: i64,
        differ: &Difference,
    ) {
        if let Some(c) = candidate(path, differ) {
            self.dirs[rep.index()]
                .entry(hash)
                .or_default()
                .add(added, c);
        }
    }

    /// `Moves.detect`: pairs deletes with creates of the same contents,
    /// and prepares the move items. Gives up on the rest after 15 seconds.
    pub fn detect(&mut self, archive: &LocalArchive, mode: Mode) -> Result<(), Error> {
        let start = Instant::now();
        for rep in [Rep::One, Rep::Two] {
            let i = rep.index();
            // Each path is a candidate at most once per replica, so the
            // order of the groups doesn't matter.
            let files: Vec<Cands> = self.files[i].drain().map(|(_, c)| c).collect();
            for c in files {
                if !self.matches(archive, mode, start, rep, c, true)? {
                    return Ok(());
                }
            }
            let dirs: Vec<Cands> = self.dirs[i].drain().map(|(_, c)| c).collect();
            for c in dirs {
                if !self.matches(archive, mode, start, rep, c, false)? {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// `matches`: each delete in turn takes its best match among the
    /// creates left (by [`file_pref`] for files, [`common_pref`] for
    /// directories). False if it ran out of time.
    fn matches(
        &mut self,
        archive: &LocalArchive,
        mode: Mode,
        start: Instant,
        rep: Rep,
        cands: Cands,
        files: bool,
    ) -> Result<bool, Error> {
        let mut adds = cands.adds;
        for del in &cands.deletes {
            if start.elapsed().as_secs_f64() > DETECT_TIME_LIMIT {
                return Ok(false);
            }
            let mut best = None::<usize>;
            let mut best_score = 128;
            for (i, add) in adds.iter().enumerate() {
                if files && same_inode(rep, del, add)? {
                    best = Some(i);
                    break;
                }
                let s = common_score(rep, del, add, mode)?;
                if s < best_score {
                    best = Some(i);
                    best_score = s;
                }
            }
            if let Some(i) = best {
                let add = adds.remove(i);
                self.prep_move(archive, rep, del, &add)?;
            }
        }
        Ok(true)
    }

    /// `prepMove`: makes a move item of the delete `a` and the create `b`
    /// unless both conflict (or either is to be merged). It goes on the
    /// path that conflicts if one does; otherwise on the old path, unless
    /// the new one overwrites something.
    fn prep_move(
        &mut self,
        archive: &LocalArchive,
        rep: Rep,
        a: &Cand,
        b: &Cand,
    ) -> Result<(), Error> {
        let merge =
            a.differ.direction == Direction::Merge || b.differ.direction == Direction::Merge;
        let conflict_a = a.differ.direction.is_conflict() || merge;
        let conflict_b = b.differ.direction.is_conflict() || merge;
        let deleted_a = rep.rc(&a.differ).status == Status::Deleted;
        let deleted_b = rep.rc(&b.differ).status == Status::Deleted;
        // A creation can shadow the deletion of something else at its path.
        let shadowed_a = rep.other_rc(&a.differ).typ != Typ::Absent;
        let shadowed_b = rep.other_rc(&b.differ).typ != Typ::Absent;
        let (to, hide) = match (conflict_a, conflict_b) {
            (false, false) if deleted_a && !shadowed_b => (a, b),
            (false, false) if deleted_a => (b, a),
            (false, false) if deleted_b && !shadowed_a => (b, a),
            (false, false) | (true, false) => (a, b),
            (false, true) => (b, a),
            // Moves conflicting on both sides aren't supported.
            (true, true) => return Ok(()),
        };
        // Nor are moves in both replicas onto the same path.
        if !self.moves.contains_key(&to.path) {
            let m = move_differ(archive, rep, to, hide)?;
            self.moves.insert(to.path.clone(), m);
            self.hides.insert(hide.path.clone());
        }
        Ok(())
    }
}

/// `addCandidate`'s element, if `path` isn't the root.
fn candidate(path: &Path<Global>, differ: &Difference) -> Option<Cand> {
    let (name, dir) = path.clone().force_local().deconstruct_rev()?;
    Some(Cand {
        path: path.clone(),
        dir: dir.make_global(),
        name,
        differ: differ.clone(),
    })
}

/// `hashArchive`: a hash of an archive's names and contents (but not
/// props).
fn hash_archive(archive: &Archive, h: i64, mode: Mode) -> i64 {
    match archive {
        // NameMap.fold: the children in descending order.
        Archive::Dir(_, children) => children.iter_desc().fold(h, |h, (n, a)| {
            ocaml_hash::hash2(n.hash_in(mode), hash_archive(a, h, mode))
        }),
        Archive::File(_, dig, _, _) => {
            let dig = Value::tuple([
                Value::string(dig.data.0.clone()),
                Value::string(dig.ress.0.clone()),
            ]);
            ocaml_hash::hash2(ocaml_hash::hash(&dig), h)
        }
        Archive::Symlink(content) => ocaml_hash::hash2(ocaml_hash::hash_string(content), h),
        Archive::NoArchive => 135,
    }
}

/// `extractStamp`: the inode stamp of a created or deleted file.
fn extract_stamp(rc: &ReplicaContent) -> Result<Stamp, Error> {
    match &rc.ui {
        UpdateItem::Updates(
            UpdateContent::Absent | UpdateContent::File(_, ContentsChange::Same),
            PrevState::File(_, _, stamp, _),
        )
        | UpdateItem::Updates(UpdateContent::File(_, ContentsChange::Updated(_, stamp, _)), _) => {
            Ok(*stamp)
        }
        _ => Err(internal("no stamp for a move candidate")),
    }
}

/// `extractProps`: the props of a created path, or of a deleted one before.
fn extract_props(rc: &ReplicaContent) -> Result<&Props, Error> {
    match &rc.ui {
        UpdateItem::Updates(
            UpdateContent::Absent,
            PrevState::File(props, ..) | PrevState::Dir(props),
        )
        | UpdateItem::Updates(UpdateContent::File(props, _) | UpdateContent::Dir(props, ..), _) => {
            Ok(props)
        }
        _ => Err(internal("no props for a move candidate")),
    }
}

/// `filePref`'s exact match: the contents and the inode are the same.
fn same_inode(rep: Rep, a: &Cand, b: &Cand) -> Result<bool, Error> {
    Ok(matches!(
        (extract_stamp(rep.rc(&a.differ))?, extract_stamp(rep.rc(&b.differ))?),
        (Stamp::Inode(i), Stamp::Inode(j)) if i == j
    ))
}

/// `commonPref`'s score of a pair (lower is better): with the same
/// mtime, a rename (the same parent) beats a move (the same name), and
/// either beats neither.
fn common_score(rep: Rep, a: &Cand, b: &Cand, mode: Mode) -> Result<i32, Error> {
    // Not Props.same_time, which doesn't always compare the synced times.
    let time_same =
        extract_props(rep.rc(&a.differ))?.time() - extract_props(rep.rc(&b.differ))?.time() == 0.;
    let dir_same = a.dir.compare_in(&b.dir, mode).is_eq();
    let name_same = a.name.eq_in(&b.name, mode);
    Ok(match (time_same, dir_same, name_same) {
        (true, true, false) => 1,
        (true, false, true) => 2,
        (false, true, false) => 3,
        (false, false, true) => 4,
        (true, false, false) => 5,
        (false, false, false) => 6,
        (_, true, true) => return Err(internal("a move onto the same path")),
    })
}

/// `moveRc`: `rc` as moved out to `path'` (if deleted) or in from it (if
/// created), carrying both replicas' contents there.
fn move_rc(
    archive: &LocalArchive,
    rc: &ReplicaContent,
    path2: &Path<Global>,
    rc2: &ReplicaContent,
    other_rc2: &ReplicaContent,
) -> Result<ReplicaContent, Error> {
    let carried = || (Box::new(rc2.clone()), Box::new(other_rc2.clone()));
    match rc.status {
        Status::Deleted => {
            let (this, other) = carried();
            let (n, size) = archive.update_size(path2, &rc2.ui)?;
            Ok(ReplicaContent {
                typ: rc2.typ,
                status: Status::MovedOut(path2.clone(), this, other),
                size: (n as i64, size),
                ..rc.clone()
            })
        }
        Status::Created => {
            let (this, other) = carried();
            Ok(ReplicaContent {
                status: Status::MovedIn(path2.clone(), this, other),
                ..rc.clone()
            })
        }
        _ => Err(internal("a move of a path neither created nor deleted")),
    }
}

/// `moveDiffer`: `a`'s difference with `a` moved to or from `b`, and the
/// deep errors of both.
fn move_differ(archive: &LocalArchive, rep: Rep, a: &Cand, b: &Cand) -> Result<Replicas, Error> {
    let rc = move_rc(
        archive,
        rep.rc(&a.differ),
        &b.path,
        rep.rc(&b.differ),
        rep.other_rc(&b.differ),
    )?;
    let mut diff = a.differ.clone();
    match rep {
        Rep::One => diff.rc1 = rc,
        Rep::Two => diff.rc2 = rc,
    }
    diff.errors1.extend(b.differ.errors1.iter().cloned());
    diff.errors2.extend(b.differ.errors2.iter().cloned());
    Ok(Replicas::Different(diff))
}

/// `Moves.originalRc`: the contents before being made a move.
pub fn original_rc(rc: &ReplicaContent) -> ReplicaContent {
    match rc.status {
        Status::MovedOut(..) => ReplicaContent {
            typ: Typ::Absent,
            status: Status::Deleted,
            ..rc.clone()
        },
        Status::MovedIn(..) => ReplicaContent {
            status: Status::Created,
            ..rc.clone()
        },
        _ => rc.clone(),
    }
}
