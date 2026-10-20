//! Update detection: what changed in each replica since its archive.
//!
//! Ports the update-detection half of Unison `src/update.ml` (the archives
//! themselves are in [`crate::archive`]).
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! # How a scan runs
//!
//! [`find_updates`] runs the `find` command on both roots. On each host,
//! [`find_local`] checks whether the predicates that decide what is
//! scanned have changed since the archive was made, then scans the paths
//! off the engine thread: the archive, the compiled predicates and the
//! fingerprint cache are moved to a blocking task ([`Scan`]) and come back
//! with the update items. So both roots of a local sync are scanned at the
//! same time, where OCaml scans one after the other; what the scans leave
//! behind on the host (copying hints, the temporary-file suffix) is applied
//! in the roots' canonical order, as OCaml would.
//!
//! The scan updates the archive in place: times of files whose contents are
//! unchanged, and the stamps that mark directories unchanged. Where OCaml
//! drops those updates under a path whose scan then fails, gemel keeps the
//! ones already made; they record only what the scan verified.
//!
//! # Not ported yet
//!
//! - Scanning only what a file watcher reported (`Fswatchold`, the path
//!   trees, and the `wantWatcher` and `subpaths` arguments of `find`): the
//!   watcher is phase 9, and without it OCaml scans every path.
//! - Status messages during the scan, which belong to the frontends.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Poll, Waker};

use gemel_base::case::{self, Mode};
use gemel_base::name::Name;
use gemel_base::path::{Global, Local, Path};
use gemel_base::prefs::{BoolWithDefault, Category, PrefDef, Prefs, Topic};
use gemel_base::proplist::Proplist;
use gemel_base::util::Error;
use gemel_compat::umarshal::{Reader, Writer};
use gemel_fs::fileinfo::{self, FsConfig, Info, Stamp, Typ};
use gemel_fs::fspath::Fspath;
use gemel_fs::os::{self, FullFingerprint, TempNames};
use gemel_fs::osx;
use gemel_fs::props::{self, DirChangedStamp, FEATURE_XATTR, Props, PropsConfig};
use gemel_rpc::host::Host;
use gemel_rpc::registry::{RootCmd, Rpc};
use gemel_rpc::runtime;

use crate::archive::{
    self, ACL_KEY, Archive, ArchiveProps, ArchiveVersion, DIR_STAMP_KEY, PROPS_PREDICATES_KEY,
    Predicates, PropsData, RSRC_KEY, UPDATE_PREDICATES_KEY, XATTRS_KEY,
};
use crate::common::{Codec, ContentsChange, PermChange, PrevState, UpdateContent, UpdateItem};
use crate::fpcache::{self, FpCache};
use crate::globals::{self, Ignore, Roots};
use crate::name_map::{NameMap, PathMap};
use crate::pred::{self, Compiled, Pred};
use crate::tree::Tree;
use crate::xferhint;

// ---- Preferences ----

/// `fastcheck`: whether to trust modification times, lengths and inode
/// numbers instead of reading files (`default` means yes).
pub static FAST_CHECK: PrefDef<BoolWithDefault> = PrefDef::new(
    "fastcheck",
    Category::Advanced(Topic::Syncprocess),
    BoolWithDefault::default,
    "do fast update detection (true/false/default)",
    "When this preference is set to \\verb|true|, \
     Unison will use the modification time and length of a file as a
       `pseudo inode number' \
     when scanning replicas for updates, \
     instead of reading the full contents of every file.  (This does not \
     apply to the very first run, when Unison will always scan \
     all files regardless of this switch).  Under \
     Windows, this may cause Unison to miss propagating an update \
     if the modification time and length of the \
     file are both unchanged by the update.  However, Unison will never \
     {\\em overwrite} such an update with a change from the other \
     replica, since it always does a safe check for updates just \
     before propagating a change.  Thus, it is reasonable to use \
     this switch under Windows most of the time and occasionally \
     run Unison once with {\\tt fastcheck} set to \
     \\verb|false|, if you are \
     worried that Unison may have overlooked an update. \
     For backward compatibility, \
     \\verb|yes|, \\verb|no|, and \\verb|default| can be used in place \
     of \\verb|true|, \\verb|false|, and \\verb|auto|.  See \
     \\sectionref{fastcheck}{Fast Update Detection} for more information.",
);

/// `useFastChecking`.
pub fn use_fast_checking(prefs: &Prefs) -> bool {
    *prefs.read(&FAST_CHECK) != BoolWithDefault::False
}

/// `mountpoint`: paths that must exist in each root at the end of update
/// detection.
pub static MOUNTPOINTS: PrefDef<Vec<String>> = PrefDef::new(
    "mountpoint",
    Category::Advanced(Topic::General),
    Vec::new,
    "abort if this path does not exist",
    "Including the preference \\texttt{-mountpoint PATH} causes Unison to \
     check, at the end of update detection, that \\texttt{PATH} exists \
     within each root, and abort if not.  This can avoid synchronzing when\
     removable media is not mounted.  This preference can be given more than once.  \
     See \\sectionref{mountpoints}{Mount Points and Removable Media}.",
);

/// `immutable`: directories whose children never change once created.
pub static IMMUTABLE: Pred = Pred::new(
    "immutable",
    Category::Advanced(Topic::Sync),
    "add a pattern to the immutable list",
    "This preference specifies paths for directories whose \
     immediate children are all immutable files --- i.e., once a file has been \
     created, its contents never changes.  When scanning for updates, \
     Unison does not check whether these files have been modified; \
     this can speed update detection significantly (in particular, for mail \
     directories).",
);

/// `immutablenot`: overrides `immutable`.
pub static IMMUTABLE_NOT: Pred = Pred::new(
    "immutablenot",
    Category::Advanced(Topic::Sync),
    "add a pattern to the immutablenot list",
    "This preference overrides {\\tt immutable}.",
);

fn xattr_ignore_initial() -> Vec<String> {
    vec![
        "Regex !(security|trusted)[.].*".into(),
        "Path !system.posix_acl_*".into(),
    ]
}

/// `xattrignore` (`Props.xattrIgnorePred`): extended attributes not to
/// synchronise. gemel doesn't synchronise them yet, but archives record
/// the patterns.
pub static XATTR_IGNORE: Pred = Pred::with_initial(
    "xattrignore",
    Category::Advanced(Topic::Sync),
    xattr_ignore_initial,
    "add a pattern to the xattrignore list",
    "Preference \\texttt{-xattrignore \\ARG{namespec}} causes Unison to \
     ignore extended attributes with names that match \\ARG{namespec}. \
     This can be used to exclude extended attributes that would fail \
     synchronization due to lack of permissions or technical differences \
     at replicas. The syntax of \\ARG{namespec} is the same as used \
     for path specification (described in \
     \\sectionref{pathspec}{Path Specification}); prefer the \\verb|Path| \
     and \\verb|Regex| forms over the \\verb|Name| form. The pattern is \
     applied to the {\\em name} of extended attribute, not to path. \
     {\\em On Linux}, attributes in the security and trusted namespaces \
     are ignored by default (this is achieved by pattern \\texttt{Regex \
     !(security|trusted)[.].*}); also attributes used to store POSIX ACL \
     are ignored by default (this is achieved by pattern \\texttt{Path \
     !system.posix\\_acl\\_*}). To sync attributes in one or both of \
     these namespaces, see the \\verb|xattrignorenot| preference. \
     Note that the namespace name must be prefixed with a \"!\" (applies \
     on Linux only). All names not prefixed with a \"!\" are taken \
     as strictly belonging to the user namespace and therefore the \
     \"!user.\" prefix is never used.",
);

/// `xattrignorenot` (`Props.xattrIgnorenotPred`): overrides `xattrignore`.
pub static XATTR_IGNORE_NOT: Pred = Pred::new(
    "xattrignorenot",
    Category::Advanced(Topic::Sync),
    "add a pattern to the xattrignorenot list",
    "This preference overrides the preference \\texttt{xattrignore}. \
     It gives a list of patterns (in the same format as \
     \\verb|xattrignore|) for extended attributes that should {\\em not} \
     be ignored, whether or not they happen to match one of the \
     \\verb|xattrignore| patterns. It is possible to synchronize only \
     desired attributes by ignoring all attributes (for example, by \
     setting \\verb|xattrignore| to \\texttt{Path *} and then adding \
     \\verb|xattrignorenot| for extended attributes that should be \
     synchronized. \
     {\\em On Linux}, attributes in the security and trusted namespaces \
     are ignored by default. To sync attributes in one or both of these \
     namespaces, you may add an \\verb|xattrignorenot| pattern like \
     \\texttt{Path !security.*} to sync all attributes in the \
     security namespace, or \\texttt{Path !security.selinux} to sync \
     a specific attribute in an otherwise ignored namespace. A pattern \
     like \\texttt{Path !system.posix\\_acl\\_*} can be used to sync \
     POSIX ACLs on Linux. \
     Note that the namespace name must be prefixed with a \"!\" (applies \
     on Linux only). All names not prefixed with a \"!\" are taken \
     as strictly belonging to the user namespace and therefore the \
     \"!user.\" prefix is never used.",
);

/// Registers this module's preferences.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&FAST_CHECK)?;
    prefs.register(&MOUNTPOINTS)?;
    IMMUTABLE.register(prefs)?;
    IMMUTABLE_NOT.register(prefs)?;
    XATTR_IGNORE.register(prefs)?;
    XATTR_IGNORE_NOT.register(prefs)
}

// ---- Parts of the archive ----

static NO_ARCHIVE: Archive = Archive::NoArchive;

/// `getPathInArchive`: the archive at `path`, and the path as the archive
/// spells it.
pub fn get_path_in_archive<'a, S>(
    archive: &'a Archive,
    path: &Path<S>,
) -> (Path<Local>, &'a Archive) {
    let mut here = Path::<Local>::empty();
    let mut archive = archive;
    let mut rest = path.clone();
    while let Some((name, rest2)) = rest.deconstruct() {
        let (name2, child) = match archive {
            Archive::Dir(_, children) => children
                .find_key_value(&name)
                .map_or((name.clone(), &NO_ARCHIVE), |(k, v)| (k.clone(), v)),
            _ => (name.clone(), &NO_ARCHIVE),
        };
        here = here.child(&name2);
        archive = child;
        rest = rest2;
    }
    (here, archive)
}

/// `translatePathLocal`: `path` as the root's archive spells it.
pub fn translate_path_local(
    host: &Host,
    fspath: &Fspath,
    path: &Path<Global>,
) -> Result<Path<Local>, Error> {
    let root = archive::this_roots_global_name(host, fspath)?;
    Ok(
        archive::with_archive(host, &root, |a| get_path_in_archive(a, path).0)
            .unwrap_or_else(|| get_path_in_archive(&NO_ARCHIVE, path).0),
    )
}

/// `translatePath`.
pub static TRANSLATE_PATH: RootCmd<Path<Global>, Path<Local>> = RootCmd::new(
    "translatePath",
    |p, w, _| p.marshal(w),
    |r, _| Path::unmarshal(r),
    |p, w, _| p.marshal(w),
    |r, _| Path::unmarshal(r),
    |host, fspath, path| Box::pin(async move { translate_path_local(&host, &fspath, &path) }),
);

/// `abortIfAnyMountpointsAreMissing`.
fn abort_if_any_mountpoints_are_missing(
    host: &Host,
    fspath: &Fspath,
    c: &FsConfig,
) -> Result<(), Error> {
    let mountpoints = host.prefs().borrow().read(&MOUNTPOINTS).clone();
    for s in mountpoints {
        let path = Path::<Local>::from_bytes(s.as_bytes())?;
        if !os::exists(fspath, &path, c)? {
            return Err(Error::Fatal(format!(
                "Path {fspath}/{} is designated as a mountpoint, but points to nothing on host {}\n",
                String::from_utf8_lossy(path.as_bytes()),
                host.canonical_host_name()
            )));
        }
    }
    Ok(())
}

// ---- Scanning ----

/// `scanInfo`: how a scan goes.
#[derive(Debug, Clone, Copy)]
pub struct ScanInfo {
    /// Trust times, lengths and inodes (`fastcheck`).
    pub fast_check: bool,
    /// Skip reading directories marked unchanged (not on Windows, whose
    /// FAT filesystems don't update directory times).
    pub dir_fast_check: bool,
    /// The stamp of this archive's unchanged directories.
    pub dir_stamp: DirChangedStamp,
    /// Read every file's props again (what decides them has changed).
    pub rescan_props: bool,
}

/// What [`find_local`] returns for each path: the path as the archive
/// spells it, the updates, and the props of the directories leading to it.
pub type Found = (Path<Local>, UpdateItem, Vec<Props>);

/// A child's status (`getChildren`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Abs,
    Dup,
    BadEnc,
    BadName,
}

/// `oldInfoOf`: the previous state of a path, from its archive.
pub fn old_info_of(archive: &Archive) -> PrevState {
    match archive {
        Archive::Dir(desc, _) => PrevState::Dir(desc.clone()),
        Archive::File(desc, fp, stamp, ress) => {
            PrevState::File(desc.clone(), fp.clone(), *stamp, ress.clone())
        }
        Archive::Symlink(_) => PrevState::Symlink,
        Archive::NoArchive => PrevState::New,
    }
}

/// `noChildChange`: whether no child changed type, appeared, disappeared or
/// failed (so the directory's list of children may be unchanged).
fn no_child_change(child_updates: &[(Name, UpdateItem)]) -> bool {
    child_updates.iter().all(|(_, ui)| {
        matches!(
            ui,
            UpdateItem::Updates(UpdateContent::File(..), PrevState::File(..))
                | UpdateItem::Updates(UpdateContent::Dir(..), PrevState::Dir(_))
                | UpdateItem::Updates(UpdateContent::Symlink(_), PrevState::Symlink)
        )
    })
}

fn dup_error(path: &Path<Local>) -> UpdateItem {
    UpdateItem::Error(format!(
        "Two or more files on a case-sensitive system have names identical except for \
         case.  They cannot be synchronized to a file system being treated as \
         case-insensitive.  (File '{}')",
        String::from_utf8_lossy(path.as_bytes())
    ))
}

fn bad_enc_error(path: &Path<Local>) -> UpdateItem {
    UpdateItem::Error(format!(
        "The file name is not encoded in Unicode.  (File '{}')",
        String::from_utf8_lossy(path.as_bytes())
    ))
}

fn bad_name_error(path: &Path<Local>) -> UpdateItem {
    UpdateItem::Error(format!(
        "The name of this Unix file is not allowed under Windows.  (File '{}')",
        String::from_utf8_lossy(path.as_bytes())
    ))
}

/// A scan of one root, with everything it needs, so that it can run off
/// the engine thread.
pub struct Scan {
    fspath: Fspath,
    info: ScanInfo,
    fs: FsConfig,
    mode: Mode,
    ignore: Ignore,
    follow: Arc<Compiled>,
    atomic: Arc<Compiled>,
    immutable: Arc<Compiled>,
    immutable_not: Arc<Compiled>,
    some_host_is_running_windows: bool,
    xfer_by_copying: bool,
    faster_check_unsafe: bool,
    temp_names: TempNames,
    fpcache: FpCache,
    hints: Vec<xferhint::Entry>,
}

/// The paths to scan, as the archive spells them, and whether each is
/// ignored.
type Translated = Vec<(Path<Global>, Path<Local>, bool)>;

/// What [`Scan::build_update_children`] finds: whether the archive changed,
/// the children's updates, whether the directory was emptied, and whether
/// it has ignored children in the archive.
type ChildrenScan = (bool, Vec<(Name, UpdateItem)>, bool, bool);

/// The props settings scans use on `host`.
fn scan_props(host: &Host) -> gemel_fs::props::PropsConfig {
    let prefs = host.prefs().borrow();
    gemel_fs::props::PropsConfig::from_prefs(&prefs).with_features(&host.features().borrow())
}

impl Scan {
    /// A scan of the root at `fspath` with `host`'s settings.
    fn new(
        host: &Host,
        fspath: &Fspath,
        info: ScanInfo,
        temp_names: TempNames,
        fpcache: FpCache,
    ) -> Result<Scan, Error> {
        let prefs = host.prefs().borrow();
        let mode = case::mode(&prefs);
        let mut fs = FsConfig::from_prefs(&prefs);
        fs.props = fs.props.with_features(&host.features().borrow());
        Ok(Scan {
            fspath: fspath.clone(),
            info,
            fs,
            mode,
            ignore: Ignore::new(&prefs, mode)?,
            follow: pred::FOLLOW.compiled(&prefs, mode)?,
            atomic: globals::ATOMIC.compiled(&prefs, mode)?,
            immutable: IMMUTABLE.compiled(&prefs, mode)?,
            immutable_not: IMMUTABLE_NOT.compiled(&prefs, mode)?,
            some_host_is_running_windows: *prefs.read(&globals::SOME_HOST_IS_RUNNING_WINDOWS),
            xfer_by_copying: *prefs.read(&xferhint::XFER_BY_COPYING),
            faster_check_unsafe: *prefs.read(&fpcache::FASTER_CHECK_UNSAFE),
            temp_names,
            fpcache,
            hints: Vec::new(),
        })
    }

    fn follow_link(&self) -> impl Fn(&Path<Local>) -> bool + '_ {
        |p| self.follow.test(p.as_bytes())
    }

    /// `Xferhint.insertEntry`, recorded to be applied on the host.
    fn hint(&mut self, path: &Path<Local>, fp: &FullFingerprint) {
        if self.xfer_by_copying && !fp.is_pseudo() {
            self.hints
                .push((self.fspath.clone(), path.clone(), fp.clone()));
        }
    }

    fn get_info(&self, path: &Path<Local>) -> Result<Info, Error> {
        fileinfo::get(true, &self.fspath, path, &self.follow_link(), &self.fs)
    }

    /// `checkFilename`.
    fn check_filename(&self, name: &Name) -> Status {
        if name.bad_encoding(self.mode) {
            Status::BadEnc
        } else if self.some_host_is_running_windows && name.bad_file() {
            Status::BadName
        } else {
            Status::Ok
        }
    }

    /// `getChildren`: the directory's children in order, each with its
    /// status; names equal under the case mode are duplicates.
    fn get_children(&self, path: &Path<Local>) -> Result<Vec<(Name, Status)>, Error> {
        let mut children = os::children_of(&self.fspath, path, &self.temp_names, &self.fs)?;
        // Descending, stably, as OCaml sorts before reversing.
        children.sort_by(|a, b| b.compare_in(a, self.mode));
        let mut out = Vec::new();
        let mut it = children.into_iter();
        if let Some(mut nm) = it.next() {
            let mut count = 1;
            for nm2 in it {
                if nm.eq_in(&nm2, self.mode) {
                    count += 1;
                } else {
                    let st = self.child_status(&nm, count);
                    out.push((std::mem::replace(&mut nm, nm2), st));
                    count = 1;
                }
            }
            let st = self.child_status(&nm, count);
            out.push((nm, st));
        }
        out.reverse();
        Ok(out)
    }

    fn child_status(&self, nm: &Name, count: usize) -> Status {
        if count > 1 {
            Status::Dup
        } else {
            self.check_filename(nm)
        }
    }

    /// `checkPropChange`.
    fn check_prop_change(&self, desc: &Props, archive: &Archive, arch_desc: &Props) -> UpdateItem {
        if desc.similar(arch_desc, &self.fs.props) {
            UpdateItem::NoUpdates
        } else {
            UpdateItem::Updates(
                UpdateContent::File(desc.clone(), ContentsChange::Same),
                old_info_of(archive),
            )
        }
    }

    /// `checkContentsChange`: whether a file in the archive changed, and
    /// the archive with its times brought up to date if only those did.
    fn check_contents_change(
        &mut self,
        path: &Path<Local>,
        info: &Info,
        archive: &mut Archive,
    ) -> Result<(bool, UpdateItem), Error> {
        let Archive::File(arch_desc, arch_fp, arch_stamp, arch_ress) = &*archive else {
            unreachable!("checkContentsChange on a file");
        };
        let (arch_desc, arch_fp, arch_stamp, arch_ress) = (
            arch_desc.clone(),
            arch_fp.clone(),
            *arch_stamp,
            arch_ress.clone(),
        );
        let dummy = Props::dummy();
        let rescan_props = self.info.rescan_props;
        // When props changed during a props rescan, the change time is
        // reset so that it is read again until the sync completes.
        let reset_ctime_at_rescan = |a: &Archive| {
            if !rescan_props || arch_desc.same_ctime(&dummy) {
                None
            } else {
                let Archive::File(_, fp, stamp, ress) = a else {
                    unreachable!()
                };
                Some(Archive::File(
                    arch_desc.reset_ctime(&dummy),
                    fp.clone(),
                    *stamp,
                    ress.clone(),
                ))
            }
        };
        let fast = self.info.fast_check;
        let data_clearly_unchanged = fpcache::data_clearly_unchanged(
            fast,
            path,
            info,
            &arch_desc,
            arch_stamp,
            &self.fs.props,
        );
        let ress_clearly_unchanged =
            fpcache::ress_clearly_unchanged(fast, info, &arch_ress, data_clearly_unchanged);
        if data_clearly_unchanged && ress_clearly_unchanged {
            self.hint(path, &arch_fp);
            let props_updates = self.check_prop_change(&info.desc, archive, &arch_desc);
            // The change time is updated only if nothing is to be
            // propagated (or as above): if propagation then failed, the
            // changes might be missed by the next scan.
            let new = if props_updates != UpdateItem::NoUpdates {
                reset_ctime_at_rescan(archive)
            } else if info.desc.same_ctime(&arch_desc) {
                None
            } else {
                Some(Archive::File(
                    arch_desc.set_time(&info.desc),
                    arch_fp,
                    arch_stamp,
                    arch_ress,
                ))
            };
            return Ok(replace(archive, new, props_updates));
        }
        let known = data_clearly_unchanged.then_some(&arch_fp);
        let fspath = self.fspath.clone();
        let follow = self.follow.clone();
        let (new_desc, new_fp, new_stamp, new_ress) = self.fpcache.fingerprint(
            false,
            fast,
            self.faster_check_unsafe,
            &fspath,
            path,
            info,
            known,
            &|p| follow.test(p.as_bytes()),
            &self.fs,
        )?;
        self.hint(path, &new_fp);
        if arch_fp == new_fp {
            let props_updates = self.check_prop_change(&new_desc, archive, &arch_desc);
            let new = if props_updates != UpdateItem::NoUpdates {
                reset_ctime_at_rescan(archive)
            } else {
                Some(Archive::File(
                    arch_desc.set_time(&new_desc),
                    arch_fp,
                    new_stamp,
                    new_ress,
                ))
            };
            Ok(replace(archive, new, props_updates))
        } else {
            Ok((
                false,
                UpdateItem::Updates(
                    UpdateContent::File(
                        new_desc,
                        ContentsChange::Updated(new_fp, new_stamp, new_ress),
                    ),
                    old_info_of(archive),
                ),
            ))
        }
    }

    /// `directoryCheckContentUnchanged`: marks the directory unchanged (for
    /// the next scan) if no child changed type and the directory itself
    /// didn't change during the scan; else changed.
    fn directory_check_content_unchanged(
        &self,
        path: &Path<Local>,
        info: &Info,
        arch_desc: &Props,
        child_updates: &[(Name, UpdateItem)],
    ) -> Result<(Props, bool), Error> {
        if no_child_change(child_updates) {
            let (_, data_unchanged, _) =
                fileinfo::unchanged(&self.fspath, path, info, &self.follow_link(), &self.fs)?;
            if data_unchanged {
                let inode = inode_of(info, &self.fs);
                return Ok(arch_desc.set_dir_change_flag(self.info.dir_stamp, inode));
            }
        }
        Ok(arch_desc.set_dir_change_flag(props::CHANGED_DIR_STAMP, 0))
    }

    /// `dirContentsClearlyUnchanged`: whether the directory's list of
    /// children is clearly as in the archive.
    fn dir_contents_clearly_unchanged(&self, info: &Info, arch_desc: &Props) -> bool {
        self.info.dir_fast_check
            && arch_desc.dir_marked_unchanged(self.info.dir_stamp, inode_of(info, &self.fs))
            && info.desc.same_time(arch_desc, &self.fs.props)
            // The root of a FAT filesystem has no modification time; stat
            // gives one far in the past.
            && arch_desc.time() >= 631_152_000. // 1 January 1990
    }

    /// `buildUpdateChildren`: the updates of a directory's children
    /// (`children`, updated in place), and whether the archive changed, the
    /// directory was emptied, and it has ignored children in the archive.
    fn build_update_children(
        &mut self,
        path: &Path<Local>,
        children: &mut NameMap<Archive>,
        unchanged_children: bool,
    ) -> Result<ChildrenScan, Error> {
        let p = path.as_bytes();
        let skip = self.immutable.test(p) && !self.immutable_not.test(p);
        let mut updates = Vec::new();
        let mut arch_updated = false;
        if unchanged_children {
            if skip {
                if self.xfer_by_copying {
                    let files: Vec<_> = children
                        .iter()
                        .filter_map(|(nm, a)| match a {
                            Archive::File(_, fp, _, _) => Some((path.child(nm), fp.clone())),
                            _ => None,
                        })
                        .collect();
                    for (p, fp) in files {
                        self.hint(&p, &fp);
                    }
                }
                return Ok((false, updates, false, false));
            }
            for (nm, archive) in children.iter_mut() {
                let path2 = path.child(nm);
                // Ignored paths may still be in the archive.
                if self.ignore.test(&path2) {
                    continue;
                }
                let (changed, ui) = self.build_update_rec(archive, &path2)?;
                arch_updated |= changed;
                if ui != UpdateItem::NoUpdates {
                    updates.push((nm.clone(), ui));
                }
            }
            return Ok((arch_updated, updates, false, false));
        }
        let mut cur = std::collections::VecDeque::from(self.get_children(path)?);
        let emptied = !children.is_empty() && cur.is_empty();
        let mut has_ignored_children = false;
        // Matches the archive's children with the directory's, in order, as
        // NameMap.mapii does; a name may be respelled to the directory's.
        let mode = children.mode();
        let mut bindings = std::mem::replace(children, NameMap::new(mode)).into_bindings();
        let mut handle = |scan: &mut Scan,
                          nm: &Name,
                          archive: &mut Archive,
                          status: Status|
         -> Result<bool, Error> {
            let path2 = path.child(nm);
            if scan.ignore.test(&path2) {
                has_ignored_children |= *archive != Archive::NoArchive;
                return Ok(false);
            }
            match status {
                Status::Ok | Status::Abs => {
                    if skip && *archive != Archive::NoArchive && status != Status::Abs {
                        if let Archive::File(_, fp, _, _) = &*archive {
                            let fp = fp.clone();
                            scan.hint(&path2, &fp);
                        }
                        return Ok(false);
                    }
                    let (changed, ui) = scan.build_update_rec(archive, &path2)?;
                    if ui != UpdateItem::NoUpdates {
                        updates.push((nm.clone(), ui));
                    }
                    Ok(changed)
                }
                Status::Dup => {
                    updates.push((nm.clone(), dup_error(&path2)));
                    Ok(false)
                }
                Status::BadEnc => {
                    updates.push((nm.clone(), bad_enc_error(&path2)));
                    Ok(false)
                }
                Status::BadName => {
                    updates.push((nm.clone(), bad_name_error(&path2)));
                    Ok(false)
                }
            }
        };
        let result = (|| -> Result<(), Error> {
            for (nm, archive) in bindings.iter_mut() {
                loop {
                    let Some((nm2, _)) = cur.front() else {
                        arch_updated |= handle(self, nm, archive, Status::Abs)?;
                        break;
                    };
                    match nm.compare_in(nm2, self.mode) {
                        std::cmp::Ordering::Less => {
                            arch_updated |= handle(self, nm, archive, Status::Abs)?;
                            break;
                        }
                        std::cmp::Ordering::Equal => {
                            let (nm2, st) = cur.pop_front().expect("front exists");
                            if *nm != nm2 {
                                arch_updated = true;
                                *nm = nm2;
                            }
                            arch_updated |= handle(self, nm, archive, st)?;
                            break;
                        }
                        std::cmp::Ordering::Greater => {
                            let (nm2, st) = cur.pop_front().expect("front exists");
                            // New on disk: it stays out of the archive.
                            handle(self, &nm2, &mut Archive::NoArchive, st)?;
                        }
                    }
                }
            }
            for (nm, st) in cur.drain(..) {
                handle(self, &nm, &mut Archive::NoArchive, st)?;
            }
            Ok(())
        })();
        *children = NameMap::from_bindings(mode, bindings);
        result?;
        Ok((arch_updated, updates, emptied, has_ignored_children))
    }

    /// `buildUpdateRec`: the updates of `path` against its archive, which is
    /// brought up to date where only times changed; and whether it was.
    /// Transient errors become the path's update.
    pub fn build_update_rec(
        &mut self,
        archive: &mut Archive,
        path: &Path<Local>,
    ) -> Result<(bool, UpdateItem), Error> {
        match self.build_update_rec_inner(archive, path) {
            Err(Error::Transient(s)) => Ok((false, UpdateItem::Error(s))),
            r => r,
        }
    }

    fn build_update_rec_inner(
        &mut self,
        archive: &mut Archive,
        path: &Path<Local>,
    ) -> Result<(bool, UpdateItem), Error> {
        let info = self.get_info(path)?;
        match (info.typ, &*archive) {
            (Typ::Absent, Archive::NoArchive) => Ok((false, UpdateItem::NoUpdates)),
            (Typ::Absent, _) => Ok((
                false,
                UpdateItem::Updates(UpdateContent::Absent, old_info_of(archive)),
            )),
            (Typ::File, Archive::File(..)) => self.check_contents_change(path, &info, archive),
            (Typ::File, _) => {
                let fspath = self.fspath.clone();
                let follow = self.follow.clone();
                let (desc, fp, stamp, ress) = self.fpcache.fingerprint(
                    true,
                    self.info.fast_check,
                    self.faster_check_unsafe,
                    &fspath,
                    path,
                    &info,
                    None,
                    &|p| follow.test(p.as_bytes()),
                    &self.fs,
                )?;
                self.hint(path, &fp);
                Ok((
                    false,
                    UpdateItem::Updates(
                        UpdateContent::File(desc, ContentsChange::Updated(fp, stamp, ress)),
                        old_info_of(archive),
                    ),
                ))
            }
            (Typ::Symlink, Archive::Symlink(prev)) => {
                let l = os::read_link(&self.fspath, path)?;
                if l == *prev {
                    Ok((false, UpdateItem::NoUpdates))
                } else {
                    Ok((
                        false,
                        UpdateItem::Updates(UpdateContent::Symlink(l), PrevState::Symlink),
                    ))
                }
            }
            (Typ::Symlink, _) => {
                let l = os::read_link(&self.fspath, path)?;
                Ok((
                    false,
                    UpdateItem::Updates(UpdateContent::Symlink(l), old_info_of(archive)),
                ))
            }
            (Typ::Directory, Archive::Dir(arch_desc, _)) => {
                let arch_desc = arch_desc.clone();
                let old_info = old_info_of(archive);
                // A directory treated atomically gets its real time;
                // otherwise touching a file inside doesn't modify it.
                let (permchange, desc) = if info.desc.similar(&arch_desc, &self.fs.props) {
                    if self.atomic.test(path.as_bytes()) {
                        (PermChange::Same, info.desc.clone())
                    } else {
                        (PermChange::Same, arch_desc.clone())
                    }
                } else {
                    (PermChange::Updated, info.desc.clone())
                };
                let unchanged = self.dir_contents_clearly_unchanged(&info, &arch_desc);
                let Archive::Dir(_, children) = archive else {
                    unreachable!()
                };
                let (children_changed, child_updates, emptied, has_ignored_children) =
                    self.build_update_children(path, children, unchanged)?;
                // An archive with ignored children can't tell whether the
                // directory's list of children changed.
                let (new_desc, updated) = if has_ignored_children {
                    (arch_desc.clone(), true)
                } else {
                    self.directory_check_content_unchanged(path, &info, &arch_desc, &child_updates)?
                };
                // Times are updated only if the directory itself has nothing
                // to propagate (except as for files, the change time).
                let (new_desc, updated) = if permchange != PermChange::Same {
                    let dummy = Props::dummy();
                    if !self.info.rescan_props || new_desc.same_ctime(&dummy) {
                        (new_desc, updated)
                    } else {
                        (new_desc.reset_ctime(&dummy), true)
                    }
                } else {
                    let updated = updated
                        || !info.desc.same_time(&new_desc, &self.fs.props)
                        || !info.desc.same_ctime(&new_desc);
                    (new_desc.set_time(&info.desc), updated)
                };
                let changed = children_changed || updated;
                if changed && let Archive::Dir(d, _) = archive {
                    *d = new_desc;
                }
                let ui = if !child_updates.is_empty() || permchange == PermChange::Updated {
                    UpdateItem::Updates(
                        UpdateContent::Dir(desc, child_updates, permchange, emptied),
                        old_info,
                    )
                } else {
                    UpdateItem::NoUpdates
                };
                Ok((changed, ui))
            }
            (Typ::Directory, _) => {
                let mut empty = NameMap::new(self.mode);
                let (_, child_updates, _, _) =
                    self.build_update_children(path, &mut empty, false)?;
                Ok((
                    false,
                    UpdateItem::Updates(
                        UpdateContent::Dir(info.desc, child_updates, PermChange::Updated, false),
                        old_info_of(archive),
                    ),
                ))
            }
        }
    }

    /// `buildUpdate`: the updates for `fullpath`, reached by going down
    /// `rest` from `here`. Also returns the path as the archive spells it,
    /// and the props of the directories on the way (for creating them on
    /// the other side). `translated` is `fullpath` translated against the
    /// archive as loaded.
    fn build_update(
        &mut self,
        archive: &mut Archive,
        fullpath: &Path<Global>,
        here: Path<Local>,
        rest: &Path<Global>,
        translated: &Path<Local>,
    ) -> Result<(UpdateItem, Path<Local>, Vec<Props>), Error> {
        let Some((name, rest2)) = rest.deconstruct() else {
            // `buildUpdatePathTree` with a leaf.
            let (_, ui) = self.build_update_rec(archive, &here)?;
            return Ok((ui, here, Vec::new()));
        };
        let info = self.get_info(&here)?;
        let s = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        let fail = |e: String| Ok((UpdateItem::Error(e), translated.clone(), Vec::new()));
        if info.typ != Typ::Directory && info.typ != Typ::Absent {
            return fail(if here.is_empty() {
                format!(
                    "path {} is not valid because the root of one of the replicas is not a \
                     directory",
                    s(fullpath.as_bytes())
                )
            } else {
                format!(
                    "path {} is not valid because {} is not a directory in one of the replicas",
                    s(fullpath.as_bytes()),
                    s(here.as_bytes())
                )
            });
        }
        let (name2, status) = if info.typ == Typ::Absent {
            (name.clone(), self.check_filename(&name))
        } else {
            self.get_children(&here)?
                .into_iter()
                .find(|(n, _)| name.eq_in(n, self.mode))
                .unwrap_or_else(|| (name.clone(), self.check_filename(&name)))
        };
        match status {
            Status::BadEnc => {
                return fail(format!(
                    "The filename {} in path {} is not encoded in Unicode",
                    s(name.as_bytes()),
                    s(fullpath.as_bytes())
                ));
            }
            Status::BadName => {
                return fail(format!(
                    "The filename {} in path {} is not allowed under Windows",
                    s(name.as_bytes()),
                    s(fullpath.as_bytes())
                ));
            }
            Status::Dup => {
                return fail(format!(
                    "The path {} is ambiguous at filename {} (i.e., the name of this path is \
                     the same, modulo capitalization, as another path in a case-sensitive \
                     filesystem, and you are synchronizing this filesystem with a filesystem \
                     being treated as case-insensitive.",
                    s(fullpath.as_bytes()),
                    s(name.as_bytes())
                ));
            }
            Status::Ok | Status::Abs => {}
        }
        let here2 = here.child(&name2);
        let (ui, local, mut dirs) = match archive {
            Archive::Dir(_, children) => {
                let mut child = children.remove(&name).unwrap_or(Archive::NoArchive);
                let r = self.build_update(&mut child, fullpath, here2, &rest2, translated);
                if child != Archive::NoArchive {
                    children.add(name2, child);
                }
                r?
            }
            _ => {
                let mut none = Archive::NoArchive;
                let r = self.build_update(&mut none, fullpath, here2, &rest2, translated)?;
                debug_assert_eq!(none, Archive::NoArchive);
                r
            }
        };
        if info.typ != Typ::Absent {
            dirs.insert(0, info.desc);
        } else {
            dirs.clear();
        }
        Ok((ui, local, dirs))
    }

    /// Scans the paths (with their translations), last first as OCaml's
    /// `fold_right` does, updating `archive`.
    fn run(&mut self, archive: &mut Archive, paths: &Translated) -> Result<Vec<Found>, Error> {
        let mut found = Vec::with_capacity(paths.len());
        for (path, translated, ignored) in paths.iter().rev() {
            if *ignored {
                found.push((translated.clone(), UpdateItem::NoUpdates, Vec::new()));
            } else {
                let (ui, local, dirs) =
                    self.build_update(archive, path, Path::empty(), path, translated)?;
                found.push((local, ui, dirs));
            }
        }
        found.reverse();
        Ok(found)
    }
}

/// Replaces `archive` by `new`, if any (the `optArch` of
/// `checkContentsChange`).
fn replace(archive: &mut Archive, new: Option<Archive>, ui: UpdateItem) -> (bool, UpdateItem) {
    match new {
        Some(a) => {
            *archive = a;
            (true, ui)
        }
        None => (false, ui),
    }
}

/// The inode in a file's stamp, or 0.
fn inode_of(info: &Info, c: &FsConfig) -> i64 {
    match info.stamp(c) {
        Stamp::Inode(i) => i,
        _ => 0,
    }
}

// ---- Whether the predicates changed ----

/// `updatePredicates`: the predicates that decide which files are
/// scanned, with their patterns (`Pred.extern`).
fn update_predicates(prefs: &Prefs) -> Predicates {
    let preds: [(&str, &Pred); 5] = [
        ("immutable", &IMMUTABLE),
        ("immutablenot", &IMMUTABLE_NOT),
        ("ignore", &globals::IGNORE),
        ("ignorenot", &globals::IGNORE_NOT),
        ("follow", &pred::FOLLOW),
    ];
    preds
        .iter()
        .map(|(n, p)| extern_pred(n, p, prefs))
        .collect()
}

fn extern_pred(name: &str, p: &Pred, prefs: &Prefs) -> (Vec<u8>, Vec<Vec<u8>>) {
    (
        name.as_bytes().to_vec(),
        p.patterns(prefs)
            .into_iter()
            .map(String::into_bytes)
            .collect(),
    )
}

/// What decides which files and props are scanned, on a host.
struct ScanPreds {
    update: Predicates,
    rsrc: bool,
    props: Predicates,
    xattrs: Option<bool>,
    acl: Option<bool>,
}

impl ScanPreds {
    fn new(host: &Host) -> ScanPreds {
        let prefs = host.prefs().borrow();
        let features = host.features().borrow();
        let xattr_enabled = features.enabled_by_name(FEATURE_XATTR);
        let acl_enabled = features.enabled_by_name(props::FEATURE_ACL);
        ScanPreds {
            update: update_predicates(&prefs),
            rsrc: *prefs.read(&osx::RSRC_AUX),
            // `propsPredicates`, when xattrs are enabled.
            props: if xattr_enabled {
                vec![
                    extern_pred("xattrignore", &XATTR_IGNORE, &prefs),
                    extern_pred("xattrignorenot", &XATTR_IGNORE_NOT, &prefs),
                ]
            } else {
                Vec::new()
            },
            // The `xattrs` and `acl` preferences aren't ported, so they
            // are off.
            xattrs: xattr_enabled.then_some(false),
            acl: acl_enabled.then_some(false),
        }
    }

    /// `updatePredicateChanged`: the new props if the update predicates
    /// changed since `props` were recorded.
    fn update_predicate_changed(&self, props: &Proplist) -> Option<Proplist> {
        let old = props.find(&UPDATE_PREDICATES_KEY);
        let old_rsrc = props.find(&RSRC_KEY);
        if old.map_or(self.update.is_empty(), |o| *o == self.update) && old_rsrc == Some(&self.rsrc)
        {
            return None;
        }
        let mut props = props.clone();
        props.add(&RSRC_KEY, self.rsrc);
        props.add(&UPDATE_PREDICATES_KEY, self.update.clone());
        Some(props)
    }

    /// `mustRescanProps`: if the props predicates or preferences changed
    /// since `props` were recorded, the new props, and whether every
    /// file's props must be read again.
    fn must_rescan_props(&self, props: &Proplist) -> (bool, Option<Proplist>) {
        let old = props.find(&PROPS_PREDICATES_KEY);
        if old.map_or(self.props.is_empty(), |o| *o == self.props)
            && props.find(&XATTRS_KEY).copied() == self.xattrs
            && props.find(&ACL_KEY).copied() == self.acl
        {
            return (false, None);
        }
        let mut props = props.clone();
        if let Some(x) = self.acl {
            props.add(&ACL_KEY, x);
        }
        if let Some(x) = self.xattrs {
            props.add(&XATTRS_KEY, x);
        }
        if !self.props.is_empty() {
            props.add(&PROPS_PREDICATES_KEY, self.props.clone());
        }
        (
            self.xattrs == Some(true) || self.acl == Some(true),
            Some(props),
        )
    }
}

/// `extractOldStyleProps`: the per-path keys of an archive from Unison ≤
/// 2.53.3, which kept them for all paths together.
fn extract_old_style_props(props: &Proplist) -> Proplist {
    let mut out = Proplist::new();
    if let Some(v) = props.find(&UPDATE_PREDICATES_KEY) {
        out.add(&UPDATE_PREDICATES_KEY, v.clone());
    }
    if let Some(v) = props.find(&RSRC_KEY) {
        out.add(&RSRC_KEY, *v);
    }
    if let Some(v) = props.find(&PROPS_PREDICATES_KEY) {
        out.add(&PROPS_PREDICATES_KEY, v.clone());
    }
    if let Some(v) = props.find(&XATTRS_KEY) {
        out.add(&XATTRS_KEY, *v);
    }
    if let Some(v) = props.find(&ACL_KEY) {
        out.add(&ACL_KEY, *v);
    }
    out
}

/// `isParent`: whether `p` is a proper prefix of `c`, name by name.
fn is_parent(p: &Path<Global>, c: &Path<Global>, mode: Mode) -> bool {
    match (p.deconstruct(), c.deconstruct()) {
        (None, Some(_)) => true,
        (Some((p, px)), Some((c, cx))) => p.eq_in(&c, mode) && is_parent(&px, &cx, mode),
        _ => false,
    }
}

/// `purgeArchivePropsOverriddenChildren`: drops the props of paths under
/// the synchronised ones (unless ignored there), which would otherwise be
/// scanned twice with different props.
fn purge_overridden_children(
    paths_props: &mut PathMap<Proplist>,
    paths: &[Path<Global>],
    ignore: &Ignore,
) {
    let mode = paths_props.mode();
    for path in paths {
        let kept: Vec<_> = std::mem::replace(paths_props, PathMap::new(mode))
            .into_bindings()
            .into_iter()
            .filter(|(k, _)| !is_parent(path, k, mode) || ignore.test(k))
            .collect();
        *paths_props = PathMap::from_bindings(mode, kept);
    }
}

/// `purgePropsForPaths`: drops the props of paths no longer in the
/// archive.
pub fn purge_props_for_paths(archive: &Archive, props: &mut ArchiveProps) {
    let mode = props.paths.mode();
    let kept: Vec<_> = std::mem::replace(&mut props.paths, PathMap::new(mode))
        .into_bindings()
        .into_iter()
        .filter(|(k, _)| *get_path_in_archive(archive, k).1 != Archive::NoArchive)
        .collect();
    props.paths = PathMap::from_bindings(mode, kept);
}

/// `checkNoUpdatePredicateChange`: records the predicates and preferences
/// that decide what is scanned for each of `paths`, and returns whether
/// every file's props must be read again and the stamp of directories
/// left unchanged since the last scan (a fresh one if anything changed,
/// which makes every directory be read).
pub fn check_no_update_predicate_change(
    host: &Host,
    this_root: &str,
    paths: &[Path<Global>],
) -> Result<(bool, DirChangedStamp), Error> {
    let mode = case::mode(&host.prefs().borrow());
    let ignore = Ignore::new(&host.prefs().borrow(), mode)?;
    let preds = ScanPreds::new(host);
    let mut props =
        archive::archive_props(host, this_root).unwrap_or_else(|| ArchiveProps::empty(mode));
    // Archives from Unison ≤ 2.53.3 have no per-path props. (A present
    // "paths" property is never empty: per-path props are only added.)
    let old_props = if props.paths.is_empty() {
        props.props.clone()
    } else {
        Proplist::new()
    };
    // Also kept for all paths together, for Unison ≤ 2.53.3.
    if let (_, Some(p)) = preds.must_rescan_props(&props.props) {
        props.props = p;
    }
    if let Some(p) = preds.update_predicate_changed(&props.props) {
        props.props = p;
    }
    let props_for_path = |props: &mut ArchiveProps, path: &Path<Global>| -> Proplist {
        match props.paths.find(path) {
            Some(p) if !p.is_empty() => p.clone(),
            _ => {
                let p = extract_old_style_props(&old_props);
                props.paths.add(path.clone(), p.clone());
                p
            }
        }
    };
    let mut rescan_props = false;
    for path in paths {
        let p = props_for_path(&mut props, path);
        let (rescan, new) = preds.must_rescan_props(&p);
        if let Some(new) = new {
            props.paths.add(path.clone(), new);
        }
        rescan_props |= rescan;
    }
    let mut preds_changed = false;
    for path in paths {
        let p = props_for_path(&mut props, path);
        if let Some(new) = preds.update_predicate_changed(&p) {
            props.paths.add(path.clone(), new);
            preds_changed = true;
        }
    }
    purge_overridden_children(&mut props.paths, paths, &ignore);
    // If the files scanned change, previously ignored files may already be
    // in the archive, so all their props must be read too.
    let rescan_props = rescan_props || preds_changed;
    let stored = props.props.find(&DIR_STAMP_KEY).copied();
    let dir_stamp = match stored {
        Some(s) if !rescan_props => s,
        _ => {
            // Kept for all paths together: per path it would be too
            // fragile.
            let s = props::fresh_dir_stamp();
            props.props.add(&DIR_STAMP_KEY, s);
            s
        }
    };
    archive::set_archive_props_local(host, this_root, props);
    Ok((rescan_props, dir_stamp))
}

// ---- Finding updates on a host ----

/// The order in which concurrent scans on a host apply what they leave
/// behind: the order the scans started in, which is the roots' canonical
/// order.
#[derive(Default)]
struct ScanOrder {
    next: u64,
    done: u64,
    finished: BTreeSet<u64>,
    wakers: Vec<Waker>,
}

/// A place in a host's [`ScanOrder`]; giving it up lets later ones go.
struct Turn {
    order: Rc<RefCell<ScanOrder>>,
    ticket: u64,
}

impl Turn {
    fn take(host: &Host) -> Turn {
        let order = host.state::<ScanOrder>();
        let ticket = {
            let mut o = order.borrow_mut();
            o.next += 1;
            o.next - 1
        };
        Turn { order, ticket }
    }

    async fn wait(&self) {
        std::future::poll_fn(|cx| {
            let mut o = self.order.borrow_mut();
            if o.done == self.ticket {
                Poll::Ready(())
            } else {
                o.wakers.push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await;
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        let wakers = {
            let mut o = self.order.borrow_mut();
            o.finished.insert(self.ticket);
            loop {
                let next = o.done;
                if !o.finished.remove(&next) {
                    break;
                }
                o.done += 1;
            }
            std::mem::take(&mut o.wakers)
        };
        for w in wakers {
            w.wake();
        }
    }
}

/// The paths a file watcher reported: unsynchronised and blacklisted ones.
pub type Subpaths = Option<(Vec<Path<Global>>, Vec<Path<Global>>)>;

/// The arguments of `find`: whether to start a file watcher, the paths, and
/// the paths a watcher reported.
pub type FindArgs = (bool, Vec<Path<Global>>, Subpaths);

/// `findLocal`: the updates for each path in the root at `fspath`, against
/// its archive, which is brought up to date for files whose contents
/// didn't change.
pub async fn find_local(host: &Host, fspath: &Fspath, args: FindArgs) -> Result<Vec<Found>, Error> {
    let turn = Turn::take(host);
    let (_want_watcher, path_list, _subpaths) = args;
    let arch_hash = archive::archive_hash(host, fspath)?;
    let mut temp_names = TempNames::default();
    temp_names.include(&arch_hash);
    let (_, this_root) = archive::archive_name(host, fspath, ArchiveVersion::MainArch)?;
    let (rescan_props, dir_stamp) = check_no_update_predicate_change(host, &this_root, &path_list)?;
    let fast_check = use_fast_checking(&host.prefs().borrow());
    let info = ScanInfo {
        fast_check,
        dir_fast_check: fast_check && cfg!(unix),
        dir_stamp,
        rescan_props,
    };
    let fpcache = {
        let (cache_name, _) = archive::archive_name(host, fspath, ArchiveVersion::FpCache)?;
        let cache_file = gemel_fs::system::os_path(&host.file_in_unison_dir(&cache_name));
        let ignore_archives = *host.prefs().borrow().read(&archive::IGNORE_ARCHIVES);
        FpCache::init(fast_check, ignore_archives, &cache_file, scan_props(host))
    };
    let mut scan = Scan::new(host, fspath, info, temp_names.clone(), fpcache)?;
    // Without a watcher, every path is scanned. Errors and ignored paths
    // are reported against the archive as loaded.
    let paths: Translated = {
        let ignore = &scan.ignore;
        path_list
            .iter()
            .map(|p| {
                Ok((
                    p.clone(),
                    translate_path_local(host, fspath, p)?,
                    ignore.test(p),
                ))
            })
            .collect::<Result<_, Error>>()?
    };
    let mut archive = archive::take_archive(host, &this_root);
    let (archive, scan, found) = runtime::blocking(move || {
        let found = scan.run(&mut archive, &paths);
        (archive, scan, found)
    })
    .await;
    archive::set_archive_local(host, &this_root, archive);
    let Scan {
        fpcache, hints, fs, ..
    } = scan;
    fpcache.finish();
    turn.wait().await;
    for h in hints {
        xferhint::insert_entry(host, h);
    }
    *host.state::<TempNames>().borrow_mut() = temp_names;
    drop(turn);
    let found = found?;
    abort_if_any_mountpoints_are_missing(host, fspath, &fs)?;
    Ok(found)
}

fn write_paths(paths: &[Path<Global>], w: &mut Writer) {
    w.list(paths, |w, p| p.marshal(w));
}

fn read_paths(r: &mut Reader<'_>) -> gemel_compat::umarshal::Result<Vec<Path<Global>>> {
    r.list(Path::unmarshal)
}

/// `findOnRoot`.
pub static FIND: RootCmd<FindArgs, Vec<Found>> = RootCmd::new(
    "find",
    |(watcher, paths, subpaths), w, _| {
        w.bool(*watcher);
        write_paths(paths, w);
        w.option(subpaths.as_ref(), |w, (a, b)| {
            write_paths(a, w);
            write_paths(b, w);
        });
    },
    |r, _| {
        Ok((
            r.bool()?,
            read_paths(r)?,
            r.option(|r| Ok((read_paths(r)?, read_paths(r)?)))?,
        ))
    },
    |found, w, f| {
        let c = Codec::new(f);
        w.list(found, |w, (path, ui, props)| {
            path.marshal(w);
            ui.marshal(w, &c);
            w.list(props, |w, p| p.marshal(w, &c.props));
        });
    },
    |r, f| {
        let c = Codec::new(f);
        r.list(|r| {
            Ok((
                Path::unmarshal(r)?,
                UpdateItem::unmarshal(r, &c)?,
                r.list(|r| Props::unmarshal(r, &c.props))?,
            ))
        })
    },
    |host, fspath, args| Box::pin(async move { find_local(&host, &fspath, args).await }),
);

fn write_props_data(d: &PropsData, w: &mut Writer) {
    w.list(d, |w, (k, l)| {
        w.string(k);
        w.list(l, |w, (a, b)| {
            w.string(a);
            w.string(b);
        });
    });
}

fn read_props_data(r: &mut Reader<'_>) -> gemel_compat::umarshal::Result<PropsData> {
    r.list(|r| Ok((r.string()?, r.list(|r| Ok((r.string()?, r.string()?)))?)))
}

/// `mergePropsdataOnRoot`: merges the client's new shared props data into
/// the host's, and returns the host's new data.
pub static PROPS_DATA: RootCmd<PropsData, PropsData> = RootCmd::new(
    "propsdata",
    |d, w, _| write_props_data(d, w),
    |r, _| read_props_data(r),
    |d, w, _| write_props_data(d, w),
    |r, _| read_props_data(r),
    |host, _, data| {
        Box::pin(async move {
            archive::merge_props_data(&host, &data)?;
            Ok(archive::new_props_data(&host))
        })
    },
);

/// The updates of one path on both roots, in the user's order.
pub type PathUpdates = (Found, Found);

/// `findUpdatesOnPaths`: loads the archives, then finds the updates for
/// each path on both roots.
pub async fn find_updates_on_paths(
    rpc: &Rpc,
    roots: &Roots,
    want_watcher: bool,
    path_list: Vec<Path<Global>>,
    subpaths: Subpaths,
) -> Result<Vec<PathUpdates>, Error> {
    archive::load_archives_safely(rpc, roots).await?;
    let args: FindArgs = (want_watcher, path_list, subpaths);
    let updates = roots.map(|r| rpc.call_root(&FIND, r, args.clone())).await?;
    // Shared props data, with hosts other than the client's.
    let enabled = {
        let f = rpc.client_host().features().borrow();
        f.enabled_by_name(FEATURE_XATTR) || f.enabled_by_name(props::FEATURE_ACL)
    };
    if enabled {
        roots
            .iter(|r| async move {
                if r.is_local() && !rpc.is_loopback() {
                    return Ok(());
                }
                let client = rpc.client_host();
                let data = rpc
                    .call_root(&PROPS_DATA, r, archive::new_props_data(client))
                    .await?;
                archive::merge_props_data(client, &data)
            })
            .await?;
    }
    let mut it = updates.into_iter();
    let (Some(u1), Some(u2), None) = (it.next(), it.next(), it.next()) else {
        return Err(Error::Fatal("Internal error: two roots expected".into()));
    };
    if u1.len() != u2.len() {
        return Err(Error::Fatal(
            "Internal error: the roots found updates for different paths".into(),
        ));
    }
    Ok(u1.into_iter().zip(u2).collect())
}

/// `findUpdates`: finds the updates for the `path` preference's paths,
/// leaving out those under another and those ignored.
pub async fn find_updates(
    rpc: &Rpc,
    roots: &Roots,
    want_watcher: bool,
    subpaths: Subpaths,
) -> Result<Vec<PathUpdates>, Error> {
    let paths = {
        let prefs = rpc.client_host().prefs().borrow();
        let mode = case::mode(&prefs);
        let ignore = Ignore::new(&prefs, mode)?;
        filter_paths(prefs.read(&globals::PATHS).clone(), mode, &ignore)
    };
    find_updates_on_paths(rpc, roots, want_watcher, paths, subpaths).await
}

/// The paths to scan: in reverse order, without those under another, and
/// without ignored ones.
pub fn filter_paths(
    mut paths: Vec<Path<Global>>,
    mode: Mode,
    ignore: &Ignore,
) -> Vec<Path<Global>> {
    let is_prefix = |p: &Path<Global>, c: &Path<Global>| {
        let (mut p, mut c) = (p.clone(), c.clone());
        loop {
            match (p.deconstruct(), c.deconstruct()) {
                (None, _) => return true,
                (Some(_), None) => return false,
                (Some((n1, p1)), Some((n2, c1))) if n1.compare_in(&n2, mode).is_eq() => {
                    p = p1;
                    c = c1;
                }
                _ => return false,
            }
        }
    };
    paths.sort_by(|a, b| b.compare_in(a, mode));
    let mut acc: Vec<Path<Global>> = Vec::new();
    for p in paths {
        // keepPrefix: drops the paths p is a prefix of.
        while acc.last().is_some_and(|hd| is_prefix(&p, hd)) {
            acc.pop();
        }
        acc.push(p);
    }
    // acc is OCaml's list in reverse.
    acc.reverse();
    acc.retain(|p| !ignore.test(p));
    acc
}

// ---- Changing the archive ----

fn internal(what: &str) -> Error {
    Error::Fatal(format!("Internal error: {what}"))
}

/// What [`update_path_in_archive`] does to the archive at a path, given
/// the path as the archive spells it.
pub type ArchiveAction<'a> = dyn FnMut(&mut Archive, &Path<Local>) -> Result<(), Error> + 'a;

/// `updatePathInArchive`: runs `action` on the archive at `rest` (below
/// `here`), creating the directories on the way. A directory made up on
/// the way (with dummy props, where the archive had something else) goes
/// away again if it ends up empty. If `action` fails, the archive is as it
/// was.
pub fn update_path_in_archive<S>(
    archive: &mut Archive,
    here: &Path<Local>,
    rest: &Path<S>,
    mode: Mode,
    action: &mut ArchiveAction<'_>,
) -> Result<(), Error> {
    let Some((name, rest2)) = rest.deconstruct() else {
        return action(archive, here);
    };
    match archive {
        Archive::Dir(desc, children) => {
            // The archive's spelling of the name.
            let (name2, mut child) = match children.find_key_value(&name).map(|(k, _)| k.clone()) {
                Some(k) => {
                    let c = children.remove(&k).expect("just found");
                    (k, c)
                }
                None => (name.clone(), Archive::NoArchive),
            };
            let r = update_path_in_archive(&mut child, &here.child(&name2), &rest2, mode, action);
            if child != Archive::NoArchive {
                children.add(name2, child);
            } else if r.is_ok() && children.is_empty() && *desc == Props::dummy() {
                // OCaml compares with Props.dummy physically: only the
                // directories it made up have exactly these props.
                *archive = Archive::NoArchive;
            }
            r
        }
        _ => {
            let mut child = Archive::NoArchive;
            update_path_in_archive(&mut child, &here.child(&name), &rest2, mode, action)?;
            *archive = if child == Archive::NoArchive {
                Archive::NoArchive
            } else {
                Archive::Dir(
                    Props::dummy(),
                    NameMap::from_bindings(mode, [(name, child)]),
                )
            };
            Ok(())
        }
    }
}

/// `updateArchiveRec`: the archive after applying `ui` to `archive`.
pub fn update_archive_rec(ui: &UpdateItem, archive: Archive, mode: Mode) -> Result<Archive, Error> {
    let uc = match ui {
        UpdateItem::NoUpdates => return Ok(archive),
        UpdateItem::Error(_) => return Ok(Archive::NoArchive),
        UpdateItem::Updates(uc, _) => uc,
    };
    Ok(match uc {
        UpdateContent::Absent => Archive::NoArchive,
        UpdateContent::File(desc, ContentsChange::Same) => match archive {
            Archive::File(_, fp, stamp, ress) => Archive::File(desc.clone(), fp, stamp, ress),
            _ => return Err(internal("unchanged contents without a file in the archive")),
        },
        UpdateContent::File(desc, ContentsChange::Updated(fp, stamp, ress)) => {
            Archive::File(desc.clone(), fp.clone(), *stamp, ress.clone())
        }
        UpdateContent::Symlink(l) => Archive::Symlink(l.clone()),
        UpdateContent::Dir(desc, children, _, _) => {
            let mut ch = match archive {
                Archive::Dir(_, ch) => ch,
                _ => NameMap::new(mode),
            };
            // fold_right: the last child first.
            for (nm, ui) in children.iter().rev() {
                let child = ch.remove(nm).unwrap_or(Archive::NoArchive);
                let child = update_archive_rec(ui, child, mode)?;
                if child != Archive::NoArchive {
                    ch.add(nm.clone(), child);
                }
            }
            Archive::Dir(desc.clone(), ch)
        }
    })
}

/// `makeArchive`: the archive `ui` makes from nothing.
pub fn make_archive(ui: &UpdateItem, mode: Mode) -> Result<Archive, Error> {
    update_archive_rec(ui, Archive::NoArchive, mode)
}

/// `stripArchive`: the archive without ignored paths, and with only the
/// props that are synchronised.
pub fn strip_archive(
    path: &Path<Local>,
    archive: &Archive,
    ignore: &Ignore,
    c: &PropsConfig,
) -> Archive {
    if ignore.test(path) {
        return Archive::NoArchive;
    }
    match archive {
        Archive::Dir(desc, children) => {
            let kept: Vec<_> = children
                .iter()
                .map(|(nm, a)| (nm.clone(), strip_archive(&path.child(nm), a, ignore, c)))
                .filter(|(_, a)| *a != Archive::NoArchive)
                .collect();
            Archive::Dir(desc.strip(c), NameMap::from_bindings(children.mode(), kept))
        }
        Archive::File(desc, fp, stamp, ress) => {
            Archive::File(desc.strip(c), fp.clone(), *stamp, ress.clone())
        }
        Archive::Symlink(_) | Archive::NoArchive => archive.clone(),
    }
}

/// `updateArchive`: the root's archive at `path`, stripped, with `ui`
/// applied.
pub fn update_archive(
    host: &Host,
    fspath: &Fspath,
    path: &Path<Local>,
    ui: &UpdateItem,
) -> Result<Archive, Error> {
    let root = archive::this_roots_global_name(host, fspath)?;
    let (mode, ignore) = {
        let prefs = host.prefs().borrow();
        let mode = case::mode(&prefs);
        (mode, Ignore::new(&prefs, mode)?)
    };
    let c = scan_props(host);
    let stripped = archive::with_archive(host, &root, |a| {
        strip_archive(path, get_path_in_archive(a, path).1, &ignore, &c)
    })
    .unwrap_or(Archive::NoArchive);
    update_archive_rec(ui, stripped, mode)
}

/// `stashCurrentVersion`: backs up the current version of a file (set by
/// the stasher).
pub type StashFn = Rc<dyn Fn(&Host, &Fspath, &Path<Local>) -> Result<(), Error>>;

#[derive(Default)]
struct Stasher(Option<StashFn>);

/// `setStasherFun`.
pub fn set_stasher_fun(host: &Host, f: StashFn) {
    host.state::<Stasher>().borrow_mut().0 = Some(f);
}

/// `markEqualLocal`: records in the root's archive that the paths, changed
/// in the same way on both roots, are now as `paths` says (backing up their
/// current versions first). If a backup fails, the archive is unchanged.
pub async fn mark_equal_local(
    host: &Host,
    fspath: &Fspath,
    paths: &Tree<Name, UpdateContent>,
) -> Result<(), Error> {
    let root = archive::this_roots_global_name(host, fspath)?;
    let mode = case::mode(&host.prefs().borrow());
    let entries = paths.flatten(Path::<Local>::empty(), &|p, n| p.child(n));
    // OCaml installs the stasher's function when it loads; here it is
    // the default.
    let stash = host.state::<Stasher>().borrow().0.clone();
    for (path, _) in &entries {
        let local = archive::with_archive(host, &root, |a| get_path_in_archive(a, path).0)
            .unwrap_or_else(|| path.clone());
        match &stash {
            Some(stash) => stash(host, fspath, &local)?,
            None => crate::stasher::stash_current_version(host, fspath, &local, None).await?,
        }
    }
    archive::with_archive_mut(host, &root, |archive| {
        for (path, uc) in &entries {
            let ui = UpdateItem::Updates((*uc).clone(), PrevState::New);
            update_path_in_archive(archive, &Path::empty(), path, mode, &mut |a, _| {
                *a = update_archive_rec(&ui, std::mem::replace(a, Archive::NoArchive), mode)?;
                Ok(())
            })?;
        }
        Ok(())
    })
}

/// `markEqualOnRoot`.
pub static MARK_EQUAL: RootCmd<Tree<Name, UpdateContent>, ()> = RootCmd::new(
    "markEqual",
    |t, w, f| {
        let c = Codec::new(f);
        t.marshal(w, &|w, n: &Name| n.marshal(w), &|w, uc: &UpdateContent| {
            uc.marshal(w, &c)
        });
    },
    |r, f| {
        let c = Codec::new(f);
        Tree::unmarshal(r, &Name::unmarshal, &|r| UpdateContent::unmarshal(r, &c))
    },
    |(), _, _| {},
    |_, _| Ok(()),
    |host, fspath, paths| Box::pin(async move { mark_equal_local(&host, &fspath, &paths).await }),
);

/// `markEqual`: records on both roots that these paths changed in the same
/// way (each node: the name and the contents on each root).
pub async fn mark_equal(
    rpc: &Rpc,
    roots: &Roots,
    equals: &Tree<(Name, Name), (UpdateContent, UpdateContent)>,
) -> Result<(), Error> {
    if equals.is_empty() {
        return Ok(());
    }
    let sides = vec![
        equals.map(&|(n, _)| n.clone(), &|(u, _)| u.clone()),
        equals.map(&|(_, n)| n.clone(), &|(_, u)| u.clone()),
    ];
    roots
        .iter2(
            |r, t| async move { rpc.call_root(&MARK_EQUAL, r, t).await },
            sides,
        )
        .await
}

/// `replaceArchiveLocal`: puts `new` at `path` in the root's archive.
pub fn replace_archive_local<S>(
    host: &Host,
    fspath: &Fspath,
    path: &Path<S>,
    new: Archive,
) -> Result<(), Error> {
    let root = archive::this_roots_global_name(host, fspath)?;
    let mode = case::mode(&host.prefs().borrow());
    let mut new = Some(new);
    archive::with_archive_mut(host, &root, |archive| {
        update_path_in_archive(archive, &Path::empty(), path, mode, &mut |a, _| {
            *a = new.take().expect("one path");
            Ok(())
        })
    })
}

/// `replaceArchiveOnRoot`. The archive is decoded in the order it was sent
/// and sorted under the host's case mode (codecs have no mode).
pub static REPLACE_ARCHIVE: RootCmd<(Path<Global>, Archive), ()> = RootCmd::new(
    "replaceArchive",
    |(p, a), w, f| {
        p.marshal(w);
        a.marshal(w, &Codec::new(f).props);
    },
    |r, f| {
        Ok((
            Path::unmarshal(r)?,
            Archive::unmarshal(r, &Codec::new(f).props, Mode::Sensitive)?,
        ))
    },
    |(), _, _| {},
    |_, _| Ok(()),
    |host, fspath, (path, archive)| {
        Box::pin(async move {
            let mode = case::mode(&host.prefs().borrow());
            replace_archive_local(&host, &fspath, &path, archive.with_mode(mode))
        })
    },
);

/// `replaceArchive`.
pub async fn replace_archive(
    rpc: &Rpc,
    root: &gemel_rpc::root::Root,
    path: Path<Global>,
    archive: Archive,
) -> Result<(), Error> {
    rpc.call_root(&REPLACE_ARCHIVE, root, (path, archive)).await
}

/// `doUpdateProps`: brings `archive` up to the state `ui` saw on disk,
/// with the props propagated from the other replica (`props`), if any. On
/// an (internal) error, the archive is unchanged.
pub fn do_update_props(
    archive: &mut Archive,
    props: Option<&Props>,
    ui: &UpdateItem,
    mode: Mode,
    c: &PropsConfig,
) -> Result<(), Error> {
    let shape_ok = match ui {
        UpdateItem::Updates(UpdateContent::File(_, ContentsChange::Same), _) => {
            matches!(archive, Archive::File(..))
        }
        UpdateItem::Updates(UpdateContent::File(..) | UpdateContent::Dir(..), _) => true,
        UpdateItem::NoUpdates => {
            props.is_none() || matches!(archive, Archive::File(..) | Archive::Dir(..))
        }
        UpdateItem::Updates(..) | UpdateItem::Error(_) => false,
    };
    if !shape_ok {
        return Err(internal("doUpdateProps on an update it can't apply"));
    }
    match ui {
        UpdateItem::Updates(UpdateContent::File(desc, ContentsChange::Same), _) => {
            if let Archive::File(d, ..) = archive {
                *d = desc.clone();
            }
        }
        UpdateItem::Updates(
            UpdateContent::File(desc, ContentsChange::Updated(fp, stamp, ress)),
            _,
        ) => {
            *archive = Archive::File(desc.clone(), fp.clone(), *stamp, ress.clone());
        }
        UpdateItem::Updates(UpdateContent::Dir(desc, ..), _) => match archive {
            Archive::Dir(d, _) => *d = desc.clone(),
            _ => *archive = Archive::Dir(desc.clone(), NameMap::new(mode)),
        },
        _ => {}
    }
    if let Some(p) = props
        && let Archive::File(d, ..) | Archive::Dir(d, _) = archive
    {
        *d = d.override_with(p, c);
    }
    Ok(())
}

/// `updateProps`: records in the root's archive the props at `path` after
/// propagation.
pub fn update_props(
    host: &Host,
    fspath: &Fspath,
    path: &Path<Local>,
    props: Option<&Props>,
    ui: &UpdateItem,
) -> Result<(), Error> {
    let root = archive::this_roots_global_name(host, fspath)?;
    let mode = case::mode(&host.prefs().borrow());
    let c = scan_props(host);
    archive::with_archive_mut(host, &root, |archive| {
        update_path_in_archive(archive, &Path::empty(), path, mode, &mut |a, _| {
            do_update_props(a, props, ui, mode, &c)
        })
    })
}

// ---- Checking that nothing changed ----

/// `fastCheckMiss`: whether fast checking could have missed this change
/// (same time and length).
fn fast_check_miss(
    use_fast_check: bool,
    path: &Path<Local>,
    desc: &Props,
    ress: &gemel_fs::osx::RessStamp,
    old_desc: &Props,
    old_ress: &gemel_fs::osx::RessStamp,
    c: &PropsConfig,
) -> bool {
    use_fast_check
        && desc.same_time(old_desc, c)
        && desc.length() == old_desc.length()
        && !fpcache::excel_file(path)
        && osx::ress_unchanged(old_ress, ress, None, true)
}

fn do_mark_possibly_updated(archive: &mut Archive) {
    match archive {
        Archive::File(_, _, stamp, _) => *stamp = Stamp::Rescan,
        Archive::Dir(desc, _) => *desc = desc.set_dir_change_flag(props::CHANGED_DIR_STAMP, 0).0,
        // Shouldn't happen.
        Archive::Symlink(_) | Archive::NoArchive => {}
    }
}

/// `markPossiblyUpdated`: makes the next scan read `path` again.
pub fn mark_possibly_updated(
    host: &Host,
    fspath: &Fspath,
    path: &Path<Local>,
) -> Result<(), Error> {
    let root = archive::this_roots_global_name(host, fspath)?;
    let mode = case::mode(&host.prefs().borrow());
    archive::with_archive_mut(host, &root, |archive| {
        update_path_in_archive(archive, &Path::empty(), path, mode, &mut |a, _| {
            do_mark_possibly_updated(a);
            Ok(())
        })
    })
}

fn mark_possibly_updated_rec(
    host: &Host,
    fspath: &Fspath,
    path: &Path<Local>,
    ui: &UpdateItem,
    use_fast_check: bool,
    c: &PropsConfig,
) -> Result<(), Error> {
    match ui {
        UpdateItem::Updates(
            UpdateContent::File(desc, ContentsChange::Updated(_, _, ress)),
            PrevState::File(old_desc, _, _, old_ress),
        ) => {
            if fast_check_miss(use_fast_check, path, desc, ress, old_desc, old_ress, c) {
                mark_possibly_updated(host, fspath, path)?;
            }
            Ok(())
        }
        UpdateItem::Updates(UpdateContent::Dir(_, children, _, _), _) => {
            for (nm, ui) in children {
                mark_possibly_updated_rec(host, fspath, &path.child(nm), ui, use_fast_check, c)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn report_update(warn_fast_check: bool, explanation: String) -> Result<(), Error> {
    let mut msg = format!("Destination updated during synchronization\n{explanation}");
    if warn_fast_check {
        msg.push_str(
            "  (if this happens repeatedly on a file that has not been changed, \n  \
             try running once with 'fastcheck' set to false)",
        );
    }
    Err(Error::Transient(msg))
}

/// `explainUpdate`: fails with what changed, if anything did.
fn explain_update(
    path: &Path<Local>,
    ui: &UpdateItem,
    use_fast_check: bool,
    c: &PropsConfig,
) -> Result<(), Error> {
    let p = String::from_utf8_lossy(path.as_bytes()).into_owned();
    match ui {
        UpdateItem::NoUpdates => Ok(()),
        UpdateItem::Error(err) => Err(Error::Transient(format!(
            "Could not check destination:\n{err}"
        ))),
        UpdateItem::Updates(uc, prev) => match (uc, prev) {
            (UpdateContent::Absent, _) => {
                report_update(false, format!("The file {p} has been deleted\n"))
            }
            (UpdateContent::File(_, ContentsChange::Same), _) => report_update(
                false,
                format!("The properties of file {p} have been modified\n"),
            ),
            (
                UpdateContent::File(desc, ContentsChange::Updated(_, _, ress)),
                PrevState::File(old_desc, old_fp, _, old_ress),
            ) => {
                if old_fp.is_pseudo() {
                    Ok(())
                } else {
                    report_update(
                        fast_check_miss(use_fast_check, path, desc, ress, old_desc, old_ress, c),
                        format!("The contents of file {p} have been modified\n"),
                    )
                }
            }
            (UpdateContent::File(_, ContentsChange::Updated(..)), _) => {
                report_update(false, format!("The file {p} has been created\n"))
            }
            (UpdateContent::Symlink(_), PrevState::Symlink) => {
                report_update(false, format!("The symlink {p} has been modified\n"))
            }
            (UpdateContent::Symlink(_), _) => {
                report_update(false, format!("The symlink {p} has been created\n"))
            }
            (UpdateContent::Dir(_, _, PermChange::Updated, _), PrevState::Dir(_)) => report_update(
                false,
                format!("The properties of directory {p} have been modified\n"),
            ),
            (UpdateContent::Dir(_, _, PermChange::Updated, _), _) => {
                report_update(false, format!("The directory {p} has been created\n"))
            }
            (UpdateContent::Dir(_, children, PermChange::Same, _), _) => {
                for (nm, ui) in children {
                    explain_update(&path.child(nm), ui, use_fast_check, c)?;
                }
                Ok(())
            }
        },
    }
}

/// `checkNoUpdates`: checks that the replica at `path_in_archive` is as the
/// root's archive with `ui` applied says (before something is written
/// there), and returns that archive. A file whose change fast checking may
/// have missed is marked to be read again by the next scan; then the
/// change is reported as a transient error.
pub async fn check_no_updates(
    host: &Host,
    fspath: &Fspath,
    path_in_archive: &Path<Local>,
    ui: &UpdateItem,
    fast_check: bool,
) -> Result<Archive, Error> {
    let root = archive::this_roots_global_name(host, fspath)?;
    let mode = case::mode(&host.prefs().borrow());
    let (local_path, sub) = archive::with_archive(host, &root, |a| {
        let (p, s) = get_path_in_archive(a, path_in_archive);
        (p, s.clone())
    })
    .unwrap_or_else(|| (path_in_archive.clone(), Archive::NoArchive));
    // What we believe is on disk...
    let expected = update_archive_rec(ui, sub, mode)?;
    // ...checked against what is there.
    let info = ScanInfo {
        fast_check,
        dir_fast_check: false,
        dir_stamp: props::CHANGED_DIR_STAMP,
        rescan_props: true,
    };
    let c = scan_props(host);
    let temp_names = host.state::<TempNames>().borrow().clone();
    let mut scan = Scan::new(host, fspath, info, temp_names, FpCache::disabled(c))?;
    let mut copy = expected.clone();
    let (scan, ui_new) = runtime::blocking(move || {
        let r = scan.build_update_rec(&mut copy, &local_path);
        (scan, r)
    })
    .await;
    for h in scan.hints {
        xferhint::insert_entry(host, h);
    }
    let (_, ui_new) = ui_new?;
    let use_fast = use_fast_checking(&host.prefs().borrow());
    mark_possibly_updated_rec(host, fspath, path_in_archive, &ui_new, use_fast, &c)?;
    explain_update(path_in_archive, &ui_new, use_fast, &c)?;
    Ok(expected)
}

// ---- Sizes ----

fn file_size(desc: &Props, ress: &gemel_fs::osx::RessStamp) -> (usize, props::Filesize) {
    (1, desc.length().wrapping_add(osx::ress_length(ress)))
}

fn size_add(a: (usize, props::Filesize), b: (usize, props::Filesize)) -> (usize, props::Filesize) {
    (a.0 + b.0, a.1.wrapping_add(b.1))
}

/// `archiveSize`: the number of items and bytes in an archive.
pub fn archive_size(archive: &Archive) -> (usize, props::Filesize) {
    match archive {
        Archive::NoArchive => (0, 0),
        Archive::Dir(_, children) => children
            .iter()
            .fold((1, 0), |size, (_, a)| size_add(size, archive_size(a))),
        Archive::File(desc, _, _, ress) => file_size(desc, ress),
        Archive::Symlink(_) => (1, 0),
    }
}

/// `updateSizeRec`: the number of items and bytes after `ui`.
pub fn update_size_rec(
    archive: &Archive,
    ui: &UpdateItem,
) -> Result<(usize, props::Filesize), Error> {
    let uc = match ui {
        UpdateItem::NoUpdates => return Ok(archive_size(archive)),
        UpdateItem::Error(_) => return Ok((0, 0)),
        UpdateItem::Updates(uc, _) => uc,
    };
    Ok(match uc {
        UpdateContent::Absent => (0, 0),
        UpdateContent::File(desc, ContentsChange::Same) => match archive {
            Archive::File(_, _, _, ress) => file_size(desc, ress),
            _ => return Err(internal("unchanged contents without a file in the archive")),
        },
        UpdateContent::File(desc, ContentsChange::Updated(_, _, ress)) => file_size(desc, ress),
        UpdateContent::Symlink(_) => (1, 0),
        UpdateContent::Dir(_, children, _, _) => match archive {
            Archive::Dir(_, arc_ch) => {
                let mut size = (1, 0);
                for (nm, a) in arc_ch.iter() {
                    if !children.iter().any(|(n, _)| n.eq_in(nm, arc_ch.mode())) {
                        size = size_add(size, archive_size(a));
                    }
                }
                for (nm, ui) in children {
                    let a = arc_ch.find(nm).unwrap_or(&NO_ARCHIVE);
                    size = size_add(size, update_size_rec(a, ui)?);
                }
                size
            }
            _ => children.iter().try_fold((1, 0), |size, (_, ui)| {
                Ok::<_, Error>(size_add(size, update_size_rec(&NO_ARCHIVE, ui)?))
            })?,
        },
    })
}

/// The local root's archive in the cache, which the client reads during
/// reconciliation (OCaml's `getSubArchiveLocal` and `updateSize` look it
/// up each time; this looks it up once and lends it out rather than
/// copying it).
pub struct LocalArchive {
    host: Rc<Host>,
    root: String,
}

impl LocalArchive {
    /// The archive of the local root.
    pub fn new(rpc: &Rpc, roots: &Roots) -> Result<LocalArchive, Error> {
        let local = roots
            .local_root()
            .ok_or_else(|| internal("no roots are installed"))?;
        let host = rpc.local_host().clone();
        let root = archive::this_roots_global_name(&host, &local.fspath)?;
        Ok(LocalArchive { host, root })
    }

    /// Runs `f` on the archive at `path` (`NoArchive` if there is none).
    pub fn with<R>(&self, path: &Path<Global>, f: impl FnOnce(&Archive) -> R) -> R {
        let mut f = Some(f);
        archive::with_archive(&self.host, &self.root, |a| {
            (f.take().expect("called once"))(get_path_in_archive(a, path).1)
        })
        .unwrap_or_else(|| (f.take().expect("called once"))(&NO_ARCHIVE))
    }

    /// `updateSize`: the number of items and bytes at `path` after `ui`.
    pub fn update_size(
        &self,
        path: &Path<Global>,
        ui: &UpdateItem,
    ) -> Result<(usize, props::Filesize), Error> {
        self.with(path, |a| update_size_rec(a, ui))
    }
}

/// `getSubArchiveLocal`: the archive at `path` of the local root.
pub fn get_sub_archive_local(
    rpc: &Rpc,
    roots: &Roots,
    path: &Path<Global>,
) -> Result<Archive, Error> {
    Ok(LocalArchive::new(rpc, roots)?.with(path, Archive::clone))
}

/// `updateSize`: the number of items and bytes at `path` after `ui`, by
/// the local root's archive.
pub fn update_size(
    rpc: &Rpc,
    roots: &Roots,
    path: &Path<Global>,
    ui: &UpdateItem,
) -> Result<(usize, props::Filesize), Error> {
    LocalArchive::new(rpc, roots)?.update_size(path, ui)
}

/// `iterFiles`: runs `f` on each file in an archive.
pub fn iter_files(
    fspath: &Fspath,
    path: &Path<Local>,
    archive: &Archive,
    f: &mut dyn FnMut(&Fspath, &Path<Local>, &FullFingerprint),
) {
    match archive {
        Archive::Dir(_, children) => {
            for (nm, a) in children.iter() {
                iter_files(fspath, &path.child(nm), a, f);
            }
        }
        Archive::File(_, fp, _, _) => f(fspath, path, fp),
        _ => {}
    }
}

// ---- Committing ----

/// `prepareCommitLocal`: writes the root's archive (with the magic) to the
/// scratch archive, and returns its hash.
pub fn prepare_commit_local(
    host: &Host,
    fspath: &Fspath,
    magic: &[u8],
) -> Result<Option<i64>, Error> {
    let (name, root) = archive::archive_name(host, fspath, ArchiveVersion::ScratchArch)?;
    let file = host.file_in_unison_dir(&name);
    let mode = case::mode(&host.prefs().borrow());
    let c = gemel_fs::props::PropsConfig::default().with_features(&host.features().borrow());
    let mut props =
        archive::archive_props(host, &root).unwrap_or_else(|| ArchiveProps::empty(mode));
    archive::with_archive(host, &root, |a| {
        let hash = archive::check_archive(a, mode, &c)?;
        purge_props_for_paths(a, &mut props);
        archive::extern_archive_propsdata(host, a, &mut props.props);
        archive::store_archive_ref(
            host,
            &file,
            &root,
            archive::ArchiveRef {
                archive: a,
                hash,
                magic,
                props: &props,
            },
        )?;
        Ok(Some(hash))
    })
    .unwrap_or_else(|| Err(internal(&format!("no archive loaded for {root}"))))
}

/// `prepareCommitOnRoot`.
pub static PREPARE_COMMIT: RootCmd<Vec<u8>, Option<i64>> = RootCmd::new(
    "prepareCommit",
    |m, w, _| w.string(m),
    |r, _| r.string(),
    |h, w, _| w.option(h.as_ref(), |w, h| w.int(*h)),
    |r, _| r.option(Reader::int),
    |host, fspath, magic| Box::pin(async move { prepare_commit_local(&host, &fspath, &magic) }),
);

/// `commitUpdates`: writes both roots' archives, with a joint commit (see
/// [`archive::do_archive_crash_recovery`]): each host writes a scratch
/// archive, and only if they have the same hash are they made the new and
/// then the main archives. All under the archive locks.
pub async fn commit_updates(rpc: &Rpc, roots: &Roots) -> Result<(), Error> {
    archive::lock_archives(rpc, roots).await?;
    let r = async {
        let mode = case::mode(&rpc.client_host().prefs().borrow());
        let magic = format!(
            "{}\0{:.0}.{}",
            mode.desc(),
            gemel_fs::system::gettimeofday(),
            std::process::id()
        )
        .into_bytes();
        let checksums = roots
            .map(|r| rpc.call_root(&PREPARE_COMMIT, r, magic.clone()))
            .await?;
        if archive::archives_identical(&checksums) {
            roots
                .iter(|r| rpc.call_root(&archive::COMMIT_ARCHIVE, r, ()))
                .await?;
            roots
                .iter(|r| rpc.call_root(&archive::POST_COMMIT_ARCHIVE, r, ()))
                .await?;
            roots
                .iter(|r| rpc.call_root(&archive::REMOVE_ARCHIVE, r, ArchiveVersion::NewArch))
                .await
        } else {
            // OCaml also dumps the archives here (`dumpArchive`, not
            // ported yet).
            archive::unlock_archives(rpc, roots).await?;
            Err(Error::Fatal(
                "Internal error: New archives are not identical.\nRetaining original \
                 archives.  Please run Unison again to bring them up to date.\n"
                    .into(),
            ))
        }
    }
    .await;
    // Remote.Thread.unwindProtect, then the unlock after.
    archive::unlock_archives(rpc, roots).await?;
    r
}

/// Registers this module's commands.
pub fn register_commands(rpc: &mut Rpc) -> Result<(), Error> {
    let r = rpc.registry_mut();
    r.register_root(&TRANSLATE_PATH)?;
    r.register_root(&FIND)?;
    r.register_root(&PROPS_DATA)?;
    r.register_root(&MARK_EQUAL)?;
    r.register_root(&REPLACE_ARCHIVE)?;
    r.register_root(&PREPARE_COMMIT)
}
