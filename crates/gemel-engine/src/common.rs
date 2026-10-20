//! Types shared by update detection, reconciliation and transport.
//!
//! Ports Unison `src/common.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Roots (`Common.root`) are in `gemel_rpc::root`. Update items travel
//! between hosts, so they have codecs; their encoding depends on the
//! negotiated features ([`Codec`]). Replica contents, differences and recon
//! items stay on the client and have none.

use gemel_base::features::Features;
use gemel_base::name::Name;
use gemel_base::path::{Global, Path};
use gemel_base::util::Error;
use gemel_compat::umarshal::{self, Reader, Writer};
use gemel_fs::fileinfo::{Stamp, Typ};
use gemel_fs::os::FullFingerprint;
use gemel_fs::osx::{RessInfo, RessStamp};
use gemel_fs::props::{Filesize, Props, PropsConfig};

/// The feature for the current encoding of [`PrevState`]. It doesn't
/// change the archive format.
pub const FEATURE_PREV_STATE2: &str = "prevState2";

/// Registers this module's features.
pub fn register_features(features: &mut Features) -> Result<(), Error> {
    features.register(FEATURE_PREV_STATE2, false, None)?;
    Ok(())
}

/// What the encodings of update items depend on: the negotiated features.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Codec {
    /// For [`Props`] (only the feature flags matter).
    pub props: PropsConfig,
    /// Feature `prevState2`: [`PrevState`] is sent as is rather than in the
    /// older form.
    pub prev_state2: bool,
}

impl Codec {
    /// The encoding for these features.
    pub fn new(features: &Features) -> Codec {
        Codec {
            props: PropsConfig::default().with_features(features),
            prev_state2: features.enabled_by_name(FEATURE_PREV_STATE2),
        }
    }
}

/// `Common.oneperpath`: one element for each path being synchronised (the
/// `path` preference), to document functions that return such lists.
#[derive(Debug, Clone, PartialEq)]
pub struct OnePerPath<T>(pub Vec<T>);

// ---- Update items ----

/// `Common.prevState`: what the archive recorded for a path.
#[derive(Debug, Clone, PartialEq)]
pub enum PrevState {
    /// A directory.
    Dir(Props),
    /// A file: its props, fingerprint, stamp and resource fork stamp.
    File(Props, FullFingerprint, Stamp, RessStamp),
    /// A symbolic link.
    Symlink,
    /// Nothing (the path is new).
    New,
}

impl PrevState {
    /// `Common.mprevState`.
    pub fn marshal(&self, w: &mut Writer, c: &Codec) {
        if c.prev_state2 {
            self.marshal2(w, c);
        } else {
            self.marshal1(w, c);
        }
    }

    /// `Common.mprevState`.
    pub fn unmarshal(r: &mut Reader<'_>, c: &Codec) -> umarshal::Result<PrevState> {
        if c.prev_state2 {
            PrevState::unmarshal2(r, c)
        } else {
            PrevState::unmarshal1(r, c)
        }
    }

    /// `mprevState2`.
    fn marshal2(&self, w: &mut Writer, c: &Codec) {
        match self {
            PrevState::Dir(desc) => {
                w.tag(0);
                desc.marshal(w, &c.props);
            }
            PrevState::File(desc, fp, stamp, ress) => {
                w.tag(1);
                desc.marshal(w, &c.props);
                fp.marshal(w);
                stamp.marshal(w);
                ress.marshal_stamp(w);
            }
            PrevState::Symlink => w.tag(2),
            PrevState::New => w.tag(3),
        }
    }

    fn unmarshal2(r: &mut Reader<'_>, c: &Codec) -> umarshal::Result<PrevState> {
        Ok(match r.tag(4)? {
            0 => PrevState::Dir(Props::unmarshal(r, &c.props)?),
            1 => PrevState::File(
                Props::unmarshal(r, &c.props)?,
                FullFingerprint::unmarshal(r)?,
                Stamp::unmarshal(r)?,
                RessStamp::unmarshal_stamp(r)?,
            ),
            2 => PrevState::Symlink,
            _ => PrevState::New,
        })
    }

    /// `mprevState1`, through `prev_to_old`: `Previous (typ, desc, fp,
    /// ress)` or `New`.
    fn marshal1(&self, w: &mut Writer, c: &Codec) {
        let previous = |w: &mut Writer, typ: Typ, desc: &Props, fp: &FullFingerprint, ress| {
            w.tag(0);
            typ.marshal(w);
            desc.marshal(w, &c.props);
            fp.marshal(w);
            RessInfo::marshal_stamp(ress, w);
        };
        match self {
            PrevState::Dir(desc) => previous(
                w,
                Typ::Directory,
                desc,
                &FullFingerprint::DUMMY,
                &RessInfo::NoRess,
            ),
            PrevState::File(desc, fp, _, ress) => previous(w, Typ::File, desc, fp, ress),
            PrevState::Symlink => previous(
                w,
                Typ::Symlink,
                &Props::dummy(),
                &FullFingerprint::DUMMY,
                &RessInfo::NoRess,
            ),
            PrevState::New => w.tag(1),
        }
    }

    /// `mprevState1`, through `prev_of_old` (which loses the stamp).
    fn unmarshal1(r: &mut Reader<'_>, c: &Codec) -> umarshal::Result<PrevState> {
        if r.tag(2)? == 1 {
            return Ok(PrevState::New);
        }
        let typ = Typ::unmarshal(r)?;
        let desc = Props::unmarshal(r, &c.props)?;
        let fp = FullFingerprint::unmarshal(r)?;
        let ress = RessStamp::unmarshal_stamp(r)?;
        Ok(match typ {
            Typ::Directory => PrevState::Dir(desc),
            Typ::File => PrevState::File(desc, fp, Stamp::NoStamp, ress),
            Typ::Symlink => PrevState::Symlink,
            Typ::Absent => PrevState::New,
        })
    }
}

/// `Common.contentschange`: whether a file's contents changed, as a hint to
/// transport.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentsChange {
    /// The same contents.
    Same,
    /// New contents: their fingerprint, stamp and resource fork stamp.
    Updated(FullFingerprint, Stamp, RessStamp),
}

impl ContentsChange {
    /// `Common.mcontentschange`.
    pub fn marshal(&self, w: &mut Writer) {
        match self {
            ContentsChange::Same => w.tag(0),
            ContentsChange::Updated(fp, stamp, ress) => {
                w.tag(1);
                fp.marshal(w);
                stamp.marshal(w);
                ress.marshal_stamp(w);
            }
        }
    }

    /// `Common.mcontentschange`.
    pub fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<ContentsChange> {
        Ok(match r.tag(2)? {
            0 => ContentsChange::Same,
            _ => ContentsChange::Updated(
                FullFingerprint::unmarshal(r)?,
                Stamp::unmarshal(r)?,
                RessStamp::unmarshal_stamp(r)?,
            ),
        })
    }
}

/// `Common.permchange`: whether a directory's props changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermChange {
    /// The same props.
    Same,
    /// Changed props.
    Updated,
}

impl PermChange {
    /// `Common.mpermchange`.
    pub fn marshal(self, w: &mut Writer) {
        w.tag(match self {
            PermChange::Same => 0,
            PermChange::Updated => 1,
        });
    }

    /// `Common.mpermchange`.
    pub fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<PermChange> {
        Ok(match r.tag(2)? {
            0 => PermChange::Same,
            _ => PermChange::Updated,
        })
    }
}

/// `Common.updateItem`: how a path differs from what the archive recorded.
// Not boxed, so that items can be matched in depth as in OCaml.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateItem {
    /// Not changed.
    NoUpdates,
    /// Changed in this replica: the new state and the old one.
    Updates(UpdateContent, PrevState),
    /// Update detection failed.
    Error(String),
}

/// `Common.updateContent`: what a changed path now is.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateContent {
    /// Nothing.
    Absent,
    /// A file: its props, and whether the contents changed.
    File(Props, ContentsChange),
    /// A directory: its props, its changed children (sorted by name, for
    /// reconciliation), whether its props changed, and whether it is now
    /// empty.
    Dir(Props, Vec<(Name, UpdateItem)>, PermChange, bool),
    /// A symbolic link, with its text.
    Symlink(Vec<u8>),
}

impl UpdateItem {
    /// `Common.mupdateItem`.
    pub fn marshal(&self, w: &mut Writer, c: &Codec) {
        match self {
            UpdateItem::NoUpdates => w.tag(0),
            UpdateItem::Updates(uc, prev) => {
                w.tag(1);
                uc.marshal(w, c);
                prev.marshal(w, c);
            }
            UpdateItem::Error(m) => {
                w.tag(2);
                w.string(m.as_bytes());
            }
        }
    }

    /// `Common.mupdateItem`.
    pub fn unmarshal(r: &mut Reader<'_>, c: &Codec) -> umarshal::Result<UpdateItem> {
        Ok(match r.tag(3)? {
            0 => UpdateItem::NoUpdates,
            1 => UpdateItem::Updates(UpdateContent::unmarshal(r, c)?, PrevState::unmarshal(r, c)?),
            _ => UpdateItem::Error(read_text(r)?),
        })
    }
}

impl UpdateContent {
    /// `Common.mupdateContent`.
    pub fn marshal(&self, w: &mut Writer, c: &Codec) {
        match self {
            UpdateContent::Absent => w.tag(0),
            UpdateContent::File(desc, change) => {
                w.tag(1);
                desc.marshal(w, &c.props);
                change.marshal(w);
            }
            UpdateContent::Dir(desc, children, perm, empty) => {
                w.tag(2);
                desc.marshal(w, &c.props);
                w.list(children, |w, (n, ui)| {
                    n.marshal(w);
                    ui.marshal(w, c);
                });
                perm.marshal(w);
                w.bool(*empty);
            }
            UpdateContent::Symlink(text) => {
                w.tag(3);
                w.string(text);
            }
        }
    }

    /// `Common.mupdateContent`.
    pub fn unmarshal(r: &mut Reader<'_>, c: &Codec) -> umarshal::Result<UpdateContent> {
        Ok(match r.tag(4)? {
            0 => UpdateContent::Absent,
            1 => UpdateContent::File(
                Props::unmarshal(r, &c.props)?,
                ContentsChange::unmarshal(r)?,
            ),
            2 => UpdateContent::Dir(
                Props::unmarshal(r, &c.props)?,
                r.list(|r| Ok((Name::unmarshal(r)?, UpdateItem::unmarshal(r, c)?)))?,
                PermChange::unmarshal(r)?,
                r.bool()?,
            ),
            _ => UpdateContent::Symlink(r.string()?),
        })
    }
}

/// An error message (OCaml strings are bytes; messages are shown as text).
fn read_text(r: &mut Reader<'_>) -> umarshal::Result<String> {
    Ok(String::from_utf8_lossy(&r.string()?).into_owned())
}

// ---- Reconciliation ----

/// `Common.status`: what happened to a path in one replica.
///
/// For the moves, see the comment on `status` in OCaml's `common.mli`:
/// [`Status::MovedOut`] is on the old path and carries both replicas'
/// contents of the new path; [`Status::MovedIn`] is on the new path and
/// carries those of the old one.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Deleted.
    Deleted,
    /// Contents changed.
    Modified,
    /// Only the props changed.
    PropsChanged,
    /// Created.
    Created,
    /// Moved to another path: the new path, and its contents in this
    /// replica and in the other.
    MovedOut(Path<Global>, Box<ReplicaContent>, Box<ReplicaContent>),
    /// Moved from another path: the old path, and its contents in this
    /// replica and in the other.
    MovedIn(Path<Global>, Box<ReplicaContent>, Box<ReplicaContent>),
    /// Unchanged.
    Unchanged,
}

impl Status {
    /// Unchanged, or only the props changed.
    fn is_unchanged_or_props(&self) -> bool {
        matches!(self, Status::Unchanged | Status::PropsChanged)
    }
}

/// `Common.replicaContent`: a path in one replica, for reconciliation and
/// the user interface.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplicaContent {
    /// Its type.
    pub typ: Typ,
    /// What happened to it.
    pub status: Status,
    /// Its props, for the user interface.
    pub desc: Props,
    /// Its update item.
    pub ui: UpdateItem,
    /// The number of items and their total size.
    pub size: (i64, Filesize),
    /// The props of its parent directories.
    pub props: Vec<Props>,
}

/// `Common.direction`: what to do with a difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Direction {
    /// Nothing, because of this conflict.
    Conflict(String),
    /// Run the merge program.
    Merge,
    /// Copy from the first replica to the second.
    Replica1ToReplica2,
    /// Copy from the second replica to the first.
    Replica2ToReplica1,
}

impl Direction {
    /// `Common.direction2string`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Direction::Conflict(_) => "conflict",
            Direction::Merge => "merge",
            Direction::Replica1ToReplica2 => "replica1 to replica2",
            Direction::Replica2ToReplica1 => "replica2 to replica1",
        }
    }

    /// `Common.isConflict`.
    pub fn is_conflict(&self) -> bool {
        matches!(self, Direction::Conflict(_))
    }
}

/// `Common.difference`: how the replicas differ at a path.
#[derive(Debug, Clone, PartialEq)]
pub struct Difference {
    /// The first replica's contents.
    pub rc1: ReplicaContent,
    /// The second replica's contents.
    pub rc2: ReplicaContent,
    /// Errors deeper in the first replica.
    pub errors1: Vec<String>,
    /// Errors deeper in the second replica.
    pub errors2: Vec<String>,
    /// What to do (the user may change it).
    pub direction: Direction,
    /// What reconciliation proposed.
    pub default_direction: Direction,
}

/// `Common.replicas`.
// Nearly all are `Different`, so boxing it would only add indirection.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Replicas {
    /// Update detection failed.
    Problem(String),
    /// The replicas differ.
    Different(Difference),
}

/// `Common.reconItem`: a path to synchronise, as spelt in each replica.
#[derive(Debug, Clone, PartialEq)]
pub struct ReconItem {
    /// The path in the first replica.
    pub path1: Path<Global>,
    /// The path in the second replica.
    pub path2: Path<Global>,
    /// How they differ.
    pub replicas: Replicas,
}

/// What propagating a replica's contents does (`riAction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Delete the other side.
    Delete,
    /// Only set props.
    SetProps,
    /// Copy contents.
    Copy,
}

/// `Common.ucLength`.
pub fn uc_length(uc: &UpdateContent) -> Filesize {
    match uc {
        UpdateContent::File(desc, _) | UpdateContent::Dir(desc, ..) => desc.length(),
        _ => 0,
    }
}

/// `Common.uiLength`.
pub fn ui_length(ui: &UpdateItem) -> Filesize {
    match ui {
        UpdateItem::Updates(uc, _) => uc_length(uc),
        _ => 0,
    }
}

/// `Common.riAction`: what propagating `rc` over `rc2` does.
pub fn ri_action(rc: &ReplicaContent, rc2: &ReplicaContent) -> Action {
    match (&rc.status, &rc2.status) {
        (Status::Deleted, _) => Action::Delete,
        (a, b) if a.is_unchanged_or_props() && b.is_unchanged_or_props() => Action::SetProps,
        _ => Action::Copy,
    }
}

/// `Common.rcLength`: how much propagating `rc` over `rc2` transfers.
pub fn rc_length(rc: &ReplicaContent, rc2: &ReplicaContent) -> Filesize {
    if ri_action(rc, rc2) == Action::SetProps {
        0
    } else {
        match rc2.status {
            // A move will be reverted: count its size too.
            Status::MovedIn(..) => rc.size.1 + rc2.size.1,
            _ => rc.size.1,
        }
    }
}

/// `Common.riLength`: how much propagating an item transfers.
pub fn ri_length(ri: &ReconItem) -> Filesize {
    match &ri.replicas {
        Replicas::Different(d)
            if d.rc1.status.is_unchanged_or_props() && d.rc2.status.is_unchanged_or_props() =>
        {
            0
        }
        Replicas::Different(d) => match d.direction {
            Direction::Replica1ToReplica2 => rc_length(&d.rc1, &d.rc2),
            Direction::Replica2ToReplica1 => rc_length(&d.rc2, &d.rc1),
            // Merges are underestimated.
            Direction::Conflict(_) | Direction::Merge => 0,
        },
        Replicas::Problem(_) => 0,
    }
}

/// A file's props, fingerprint and resource fork stamp, as `fileInfos`
/// returns them for each replica.
pub type FileInfo = (Props, FullFingerprint, RessStamp);

/// `Common.fileInfos`: the old and new versions of a file changed on one
/// side or both, for diffing.
pub fn file_infos(ui1: &UpdateItem, ui2: &UpdateItem) -> Result<(FileInfo, FileInfo), Error> {
    use ContentsChange::{Same, Updated};
    use UpdateContent::File;
    use UpdateItem::{NoUpdates, Updates};
    let info = |desc: &Props, fp: &FullFingerprint, ress: &RessStamp| {
        (desc.clone(), fp.clone(), ress.clone())
    };
    match (ui1, ui2) {
        (
            Updates(File(desc1, Updated(fp1, _, ress1)), PrevState::File(desc2, fp2, _, ress2)),
            NoUpdates | Updates(File(_, Same), _),
        )
        | (
            NoUpdates | Updates(File(_, Same), _),
            Updates(File(desc2, Updated(fp2, _, ress2)), PrevState::File(desc1, fp1, _, ress1)),
        )
        | (
            Updates(File(desc1, Updated(fp1, _, ress1)), _),
            Updates(File(desc2, Updated(fp2, _, ress2)), _),
        ) => Ok((info(desc1, fp1, ress1), info(desc2, fp2, ress2))),
        _ => Err(Error::Transient("Can't diff".into())),
    }
}

/// `Common.problematic`: a problem or a conflict.
pub fn problematic(ri: &ReconItem) -> bool {
    match &ri.replicas {
        Replicas::Problem(_) => true,
        Replicas::Different(d) => d.direction.is_conflict(),
    }
}

/// `Common.partiallyProblematic`: a problem, a conflict, or errors deeper
/// down.
pub fn partially_problematic(ri: &ReconItem) -> bool {
    match &ri.replicas {
        Replicas::Problem(_) => true,
        Replicas::Different(d) => {
            d.direction.is_conflict() || !d.errors1.is_empty() || !d.errors2.is_empty()
        }
    }
}

/// `Common.isDeletion`: propagating the item deletes something.
pub fn is_deletion(ri: &ReconItem) -> bool {
    match &ri.replicas {
        Replicas::Different(d) => matches!(
            (&d.direction, d.rc1.typ, d.rc2.typ),
            (Direction::Replica1ToReplica2, Typ::Absent, _)
                | (Direction::Replica2ToReplica1, _, Typ::Absent)
        ),
        Replicas::Problem(_) => false,
    }
}

/// `Common.rcType`.
pub fn rc_type(rc: &ReplicaContent) -> &'static str {
    rc.typ.as_str()
}

/// `Common.riFileType`: the type of what the default direction propagates.
pub fn ri_file_type(ri: &ReconItem) -> &'static str {
    match &ri.replicas {
        Replicas::Different(d) => match d.default_direction {
            Direction::Replica2ToReplica1 => rc_type(&d.rc2),
            _ => rc_type(&d.rc1),
        },
        Replicas::Problem(_) => "nonexistent",
    }
}
