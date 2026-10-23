//! Copying file contents between replicas.
//!
//! Ports Unison `src/copy.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! The local half: copying a file between local roots (cloning it where
//! the filesystem can, else copying in the kernel, else reading and
//! writing), checking the copy and the source, and copying trees. The
//! copy itself is synchronous, with callbacks for progress and abort
//! checks, so that [`file`] runs it off the engine thread.
//!
//! The remote half: transferring a file between hosts (`transferFile`),
//! whose destination asks the source to `compress` it, as it is or as
//! rsync deltas ([`crate::transfer`]), and decodes the chunks streamed back
//! (`processTransferInstruction`).
//!
//! A destination that already has a file with the same contents copies it
//! instead (`tryCopyMovedFile`), and a transfer of contents another is
//! bringing waits for it, so as to copy them (`registerFileTransfer`).
//!
//! A transfer that was interrupted resumes from the temporary file it
//! left, if that is long enough and the source starts with it
//! (`validFilePrefix`).
//!
//! Not ported: external copy programs (`copyprog`), which are ignored with
//! a warning. Their preferences are registered.
//! A copy reports its progress to the client's progress printer
//! ([`crate::uutil`]), and stops when everything is aborted
//! ([`crate::abort::AllFlag`]), as it runs.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use gemel_base::case;
use gemel_base::features::Features;
use gemel_base::path::{Local, Path};
use gemel_base::prefs::{Category, DumpedPref, PrefDef, Prefs, Topic};
use gemel_base::util::{Error, FileId};
use gemel_compat::umarshal::{self, Reader, Writer};
use gemel_fs::fileinfo::{self, FsConfig, Info, SetAction, Stamp, Typ};
use gemel_fs::fingerprint::Fingerprint;
use gemel_fs::fs;
use gemel_fs::fspath::Fspath;
use gemel_fs::os::{self, FullFingerprint, TempNames};
use gemel_fs::osx::{self, RessStamp};
use gemel_fs::props::{Filesize, Props, PropsConfig};
use gemel_fs::reflink;
use gemel_rpc::connection::{Connection, STREAMING_ACTIVATED};
use gemel_rpc::host::Host;
use gemel_rpc::registry::{RootCmd, Rpc, ServerCmd, StreamCmd};
use gemel_rpc::root::Root;

use crate::abort::Abort;
use crate::pred::{self, Compiled};
use crate::transfer::{self, rsync};
use crate::{exclusive, fpcache, update, uutil, xferhint};

// ---- Preferences ----

/// `rsync`.
pub static RSYNC_ACTIVATED: PrefDef<bool> = PrefDef::new(
    "rsync",
    Category::Advanced(Topic::Remote),
    || true,
    "activate the rsync transfer mode",
    "Unison uses the 'rsync algorithm' for 'diffs-only' transfer \
     of updates to large files.  Setting this flag to false makes Unison \
     use whole-file transfers instead.  Under normal circumstances, \
     there is no reason to do this, but if you are having trouble with \
     repeated 'rsync failure' errors, setting it to \
     false should permit you to synchronize the offending files.",
);

/// `copyprog`.
pub static COPYPROG: PrefDef<String> = PrefDef::new(
    "copyprog",
    Category::Advanced(Topic::General),
    || "rsync --partial --inplace --compress".to_owned(),
    "external program for copying large files",
    "A string giving the name of an \
     external program that can be used to copy large files efficiently  \
     (plus command-line switches telling it to copy files in-place).  \
     The default setting invokes {\\tt rsync} with appropriate \
     options---most users should not need to change it.",
)
.deprecated();

/// `copyprogrest`.
pub static COPYPROGREST: PrefDef<String> = PrefDef::new(
    "copyprogrest",
    Category::Advanced(Topic::General),
    || "rsync --partial --append-verify --compress".to_owned(),
    "variant of copyprog for resuming partial transfers",
    "A variant of {\\tt copyprog} that names an external program \
     that should be used to continue the transfer of a large file \
     that has already been partially transferred.  Typically, \
     {\\tt copyprogrest} will just be {\\tt copyprog} \
     with one extra option (e.g., {\\tt --partial}, for rsync).  \
     The default setting invokes {\\tt rsync} with appropriate \
     options---most users should not need to change it.",
)
.deprecated();

/// `copythreshold`.
pub static COPYTHRESHOLD: PrefDef<i64> = PrefDef::new(
    "copythreshold",
    Category::Advanced(Topic::General),
    || -1,
    "use copyprog on files bigger than this (if >=0, in Kb)",
    "A number indicating above what filesize (in kilobytes) Unison should \
     use the external \
     copying utility specified by {\\tt copyprog}. Specifying 0 will cause \
     {\\em all} copies to use the external program; \
     a negative number will prevent any files from using it.  \
     The default is -1.",
)
.deprecated();

/// `copymax`.
pub static COPYMAX: PrefDef<i64> = PrefDef::new(
    "copymax",
    Category::Advanced(Topic::General),
    || 1,
    "maximum number of simultaneous copyprog transfers",
    "A number indicating how many instances of the external copying utility \
     Unison is allowed to run simultaneously (default to 1).",
)
.deprecated();

/// Registers this module's preferences, in OCaml's order.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&RSYNC_ACTIVATED)?;
    prefs.register(&COPYPROG)?;
    prefs.register(&COPYPROGREST)?;
    prefs.register(&COPYTHRESHOLD)?;
    // Removed in 2.53.3.
    prefs.mark_removed("copyquoterem");
    prefs.register(&COPYMAX)
}

fn internal(what: &str) -> Error {
    Error::Fatal(format!("Internal error: copy: {what}"))
}

// ---- What copying needs from the host ----

/// The preferences a host's copies depend on, taken from the host so that
/// copying can run off the engine thread.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// The filesystem settings (with the negotiated features).
    pub fs: FsConfig,
    /// `follow`.
    pub follow: Arc<Compiled>,
    /// The temporary-file names in use.
    pub temp_names: TempNames,
}

impl Ctx {
    /// The settings of `host`.
    pub fn of(host: &Host) -> Result<Ctx, Error> {
        let prefs = host.prefs().borrow();
        let mut fs = FsConfig::from_prefs(&prefs);
        fs.props = fs.props.with_features(&host.features().borrow());
        Ok(Ctx {
            fs,
            follow: pred::FOLLOW.compiled(&prefs, case::mode(&prefs))?,
            temp_names: host.state::<TempNames>().borrow().clone(),
        })
    }

    fn follow(&self) -> impl Fn(&Path<Local>) -> bool + '_ {
        |p| self.follow.test(p.as_bytes())
    }

    fn props(&self) -> &PropsConfig {
        &self.fs.props
    }
}

/// What a copy replaces (`` [`Update of (len, ress len) | `Copy] ``).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replacing {
    /// An existing file, with these data and resource fork lengths: the
    /// copy gets its props.
    Update(Filesize, Filesize),
    /// Nothing: the copy gets the default props.
    Copy,
}

/// Progress callbacks: `progress` gets each count of bytes copied, and
/// `check` may abort the copy (`Abort.checkAll`).
pub struct Progress<'a> {
    /// Called with each count of bytes copied.
    pub progress: &'a mut dyn FnMut(Filesize),
    /// Fails to abort the copy.
    pub check: &'a dyn Fn() -> Result<(), Error>,
}

impl Progress<'_> {
    /// No progress to report, and no aborting.
    pub fn none<R>(f: impl FnOnce(&mut Progress<'_>) -> R) -> R {
        let mut ignore = |_: Filesize| {};
        let ok = || -> Result<(), Error> { Ok(()) };
        f(&mut Progress {
            progress: &mut ignore,
            check: &ok,
        })
    }
}

// ---- Checking the source ----

/// What `checkForChangesToSource` checks the source against: the archive's
/// record of it, and maybe the fingerprint of the copy.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceCheck {
    /// The source.
    pub path: Path<Local>,
    /// Its archived props.
    pub desc: Props,
    /// Its archived fingerprint.
    pub fp: FullFingerprint,
    /// Its archived stamp, if known.
    pub stamp: Option<Stamp>,
    /// Its archived resource fork.
    pub ress: RessStamp,
    /// The copy's fingerprint, when the archive has a pseudo-fingerprint.
    pub new_fp: Option<FullFingerprint>,
    /// Whether to recompute the fingerprint even when the source looks
    /// unchanged (after a failed copy).
    pub paranoid: bool,
}

impl SourceCheck {
    fn marshal(&self, w: &mut Writer, c: &PropsConfig) {
        self.path.marshal(w);
        self.desc.marshal(w, c);
        self.fp.marshal(w);
        w.option(self.stamp.as_ref(), |w, s| s.marshal(w));
        self.ress.marshal_stamp(w);
        w.option(self.new_fp.as_ref(), |w, fp| fp.marshal(w));
        w.bool(self.paranoid);
    }

    fn unmarshal(r: &mut Reader<'_>, c: &PropsConfig) -> umarshal::Result<SourceCheck> {
        Ok(SourceCheck {
            path: Path::unmarshal(r)?,
            desc: Props::unmarshal(r, c)?,
            fp: FullFingerprint::unmarshal(r)?,
            stamp: r.option(Stamp::unmarshal)?,
            ress: RessStamp::unmarshal_stamp(r)?,
            new_fp: r.option(FullFingerprint::unmarshal)?,
            paranoid: r.bool()?,
        })
    }
}

/// `Os.fingerprint` of a file whose type is known.
fn fingerprint(
    ctx: &Ctx,
    fspath: &Fspath,
    path: &Path<Local>,
    typ: Typ,
) -> Result<FullFingerprint, Error> {
    os::fingerprint(fspath, path, typ.info_kind(), ctx.props().rsrc)
}

/// What [`check_source`] found: the source is as recorded, or changed
/// where the fast check couldn't see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceState {
    AsRecorded,
    MissedChange,
}

/// `checkForChangesToSourceLocal`: fails if the source changed since
/// update detection (by the archive's record, or against `new_fp`). A
/// change the fast check missed is recorded for the next scan. The source
/// is read off the engine thread.
pub async fn check_for_changes_to_source_local(
    host: &Host,
    fspath: &Fspath,
    check: SourceCheck,
) -> Result<(), Error> {
    let ctx = Ctx::of(host)?;
    let abspath = fspath.concat(&check.path);
    let (f, c) = (fspath.clone(), check.clone());
    match exclusive::blocking(host, move || check_source(&ctx, &f, &c)).await? {
        SourceState::AsRecorded => Ok(()),
        SourceState::MissedChange => {
            update::mark_possibly_updated(host, fspath, &check.path)?;
            Err(Error::Transient(format!(
                "The source file {abspath}\n\
                 has been modified but the fast update detection mechanism\n\
                 failed to detect it.  Try running once with the fastcheck\n\
                 option set to 'no'."
            )))
        }
    }
}

/// The checks of [`check_for_changes_to_source_local`] that read the
/// source.
fn check_source(ctx: &Ctx, fspath: &Fspath, check: &SourceCheck) -> Result<SourceState, Error> {
    let path = &check.path;
    let source = fileinfo::get_basic(true, fspath, path, &ctx.follow(), &ctx.fs)?;
    let abspath = fspath.concat(path);
    match &check.new_fp {
        None => {
            let clearly_changed = source.typ != Typ::File
                || source.desc.length() != check.desc.length()
                || osx::ress_length(&source.os_x.ress_info) != osx::ress_length(&check.ress);
            let data_clearly_unchanged = !clearly_changed
                && source.desc.same_time(&check.desc, ctx.props())
                && !fpcache::excel_file(path)
                && match check.stamp {
                    Some(Stamp::Inode(inode)) => source.inode == inode,
                    Some(Stamp::NoStamp) => true,
                    Some(Stamp::Rescan) | None => false,
                };
            let ress_clearly_unchanged = !clearly_changed
                && osx::ress_unchanged(
                    &check.ress,
                    &source.os_x.ress_info,
                    None,
                    data_clearly_unchanged,
                );
            if data_clearly_unchanged && ress_clearly_unchanged {
                if check.paranoid
                    && !check.fp.is_pseudo()
                    && check.fp != fingerprint(ctx, fspath, path, source.typ)?
                {
                    return Ok(SourceState::MissedChange);
                }
            } else if clearly_changed || check.fp != fingerprint(ctx, fspath, path, source.typ)? {
                return Err(Error::Transient(format!(
                    "The source file {abspath}\nhas been modified during synchronization.  \
                     Transfer aborted."
                )));
            }
            Ok(SourceState::AsRecorded)
        }
        Some(new_fp) => {
            // The archive has a pseudo-fingerprint, so the source can only
            // be compared with the copy.
            if !check.fp.is_pseudo() {
                return Err(internal(
                    "a copy's fingerprint without a pseudo-fingerprint",
                ));
            }
            if *new_fp != fingerprint(ctx, fspath, path, source.typ)? {
                return Err(Error::Transient(format!(
                    "Current source file {abspath}\n not same as transferred file.  \
                     Transfer aborted."
                )));
            }
            Ok(SourceState::AsRecorded)
        }
    }
}

/// `checkForChangesToSourceOnRoot`.
pub static CHECK_FOR_CHANGES_TO_SOURCE: RootCmd<SourceCheck, ()> = RootCmd::new(
    "checkForChangesToSource",
    |a, w, f| a.marshal(w, &PropsConfig::default().with_features(f)),
    |r, f| SourceCheck::unmarshal(r, &PropsConfig::default().with_features(f)),
    |(), _, _| {},
    |_, _| Ok(()),
    |host, fspath, check| {
        Box::pin(async move { check_for_changes_to_source_local(&host, &fspath, check).await })
    },
);

// ---- Checking the copy ----

/// `transferStatus`: how a copy went.
#[derive(Debug, Clone, PartialEq)]
pub enum TransferStatus {
    /// The copy is right; its info.
    Succeeded(Info),
    /// The copy's info and fingerprint, to check against the source (the
    /// archive had a pseudo-fingerprint).
    NeedsDoubleCheckAgainstCurrentSource(Info, FullFingerprint),
    /// The copy is wrong, for this reason.
    Failed(String),
}

/// `paranoidCheck`: compares the copy's fingerprint with the archive's.
pub fn paranoid_check(
    ctx: &Ctx,
    fspath_to: &Fspath,
    path_to: &Path<Local>,
    fp: &FullFingerprint,
) -> Result<TransferStatus, Error> {
    let info = fileinfo::get_basic(false, fspath_to, path_to, &|_| false, &ctx.fs)?;
    let fp2 = fingerprint(ctx, fspath_to, path_to, info.typ)?;
    Ok(if fp.is_pseudo() {
        TransferStatus::NeedsDoubleCheckAgainstCurrentSource(info, fp2)
    } else if fp2 != *fp {
        TransferStatus::Failed(fp.reason_for_mismatch(&fp2).to_owned())
    } else {
        TransferStatus::Succeeded(info)
    })
}

/// `saveTempFileLocal`: keeps a wrong copy (as `<name>-bad`, at the root)
/// and fails, saying so.
pub fn save_temp_file_local(
    host: &Host,
    fspath_to: &Fspath,
    (path_to, real_path_to, reason): &(Path<Local>, Path<Local>, String),
) -> Result<(), Error> {
    let ctx = Ctx::of(host)?;
    let bad = match real_path_to.deconstruct_rev() {
        Some((nm, _)) => Path::empty().child(&nm).add_suffix_to_final_name(b"-bad"),
        None => Path::from_bytes(b"bad")?,
    };
    let save_path = os::temp_path(true, fspath_to, &bad, &ctx.temp_names, &ctx.fs)?;
    // Not failing again here, which would hide why.
    match os::rename(
        "save temp",
        fspath_to,
        path_to,
        fspath_to,
        &save_path,
        None,
        &ctx.fs,
    ) {
        Ok(()) | Err(Error::Transient(_)) => {}
        Err(e) => return Err(e),
    }
    Err(Error::Transient(format!(
        "The file {} was incorrectly transferred  (fingerprint mismatch in {reason}) \
         -- temp file saved as {}",
        String::from_utf8_lossy(path_to.as_bytes()),
        fspath_to.concat(&save_path)
    )))
}

/// `saveTempFileOnRoot`.
pub static SAVE_TEMP_FILE: RootCmd<(Path<Local>, Path<Local>, String), ()> = RootCmd::new(
    "saveTempFile",
    |(p, real, reason), w, _| {
        p.marshal(w);
        real.marshal(w);
        w.string(reason.as_bytes());
    },
    |r, _| {
        Ok((
            Path::unmarshal(r)?,
            Path::unmarshal(r)?,
            String::from_utf8_lossy(&r.string()?).into_owned(),
        ))
    },
    |(), _, _| {},
    |_, _| Ok(()),
    |host, fspath, args| Box::pin(async move { save_temp_file_local(&host, &fspath, &args) }),
);

// ---- Copying ----

/// `removeOldTempFile`.
fn remove_old_temp_file(ctx: &Ctx, fspath_to: &Fspath, path_to: &Path<Local>) -> Result<(), Error> {
    if os::exists(fspath_to, path_to, &ctx.fs)? {
        os::delete(fspath_to, path_to, &ctx.fs)?;
    }
    Ok(())
}

/// Which fork to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fork {
    Data,
    Ress,
}

/// A read or write error, as OCaml reports a channel's `Sys_error`
/// through `convertUnixErrorsToTransient what`.
fn io_error(what: &str, e: &std::io::Error) -> Error {
    let s = e.to_string();
    let s = s.find(" (os error ").map_or(&s[..], |i| &s[..i]);
    Error::Transient(format!("Error in {what}:\n{s}"))
}

/// `openFileIn`.
fn open_in(
    fspath: &Fspath,
    path: &Path<Local>,
    fork: Fork,
    what: &str,
) -> Result<std::fs::File, Error> {
    match fork {
        Fork::Data => fs::open_in_bin(&fspath.concat(path)).map_err(|e| e.to_transient_sys(what)),
        Fork::Ress => osx::open_ress_in(fspath, path),
    }
}

/// `openFileOut`: a new data fork is private until its props are set
/// (or the default file permissions, if `dontchmod`).
fn open_out(
    ctx: &Ctx,
    fspath: &Fspath,
    path: &Path<Local>,
    fork: Fork,
    what: &str,
) -> Result<std::fs::File, Error> {
    match fork {
        Fork::Data => {
            let perm = if ctx.fs.dont_chmod {
                Props::file_default().perm.bits as u32
            } else {
                0o600
            };
            fs::open_out_new(&fspath.concat(path), perm).map_err(|e| e.to_transient(what))
        }
        Fork::Ress => osx::open_ress_out(fspath, path),
    }
}

/// `copy_size`: at most 10 MiB at a time, for progress.
fn copy_size(len: Filesize) -> usize {
    len.clamp(0, 10_485_760) as usize
}

/// `Uutil.readWriteBounded`: copies up to `len` bytes (stopping at the end
/// of `src`), reporting progress about every 100 KiB.
fn read_write_bounded(
    src: &mut std::fs::File,
    dst: &mut std::fs::File,
    len: Filesize,
    fork: Fork,
    progress: &mut Progress<'_>,
) -> Result<(), Error> {
    const WHAT: &str = "readWriteBounded";
    const BUFSIZE: usize = 65536;
    let mut buf = vec![0u8; BUFSIZE];
    let mut len = len;
    let mut since = 0usize;
    let flush = |since: &mut usize, progress: &mut Progress<'_>| -> Result<(), Error> {
        if *since > 0 {
            report(progress, fork, *since as Filesize)?;
            *since = 0;
        }
        Ok(())
    };
    while len > 0 {
        let want = if len > BUFSIZE as Filesize {
            BUFSIZE
        } else {
            len as usize
        };
        let n = src.read(&mut buf[..want]).map_err(|e| io_error(WHAT, &e))?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).map_err(|e| io_error(WHAT, &e))?;
        since += n;
        if since >= 100 * 1024 {
            flush(&mut since, progress)?;
        }
        len -= n as Filesize;
    }
    flush(&mut since, progress)
}

/// `copyContents`'s progress: checks for an abort (except for resource
/// forks), then reports.
fn report(p: &mut Progress<'_>, fork: Fork, n: Filesize) -> Result<(), Error> {
    if fork != Fork::Ress {
        (p.check)()?;
    }
    (p.progress)(n);
    Ok(())
}

/// `copyContents`: copies a fork, by cloning it if the filesystem can,
/// else in the kernel if it can, else by reading and writing.
fn copy_contents(
    ctx: &Ctx,
    from: (&Fspath, &Path<Local>),
    to: (&Fspath, &Path<Local>),
    fork: Fork,
    len: Filesize,
    p: &mut Progress<'_>,
) -> Result<(), Error> {
    const WHAT: &str = "copying locally";
    let (from_abs, to_abs) = (from.0.concat(from.1), to.0.concat(to.1));
    if fork == Fork::Data && reflink::clone_path(&from_abs, &to_abs) {
        (p.progress)(len);
        return Ok(());
    }
    let mut src = open_in(from.0, from.1, fork, WHAT)?;
    let mut dst = open_out(ctx, to.0, to.1, fork, WHAT)?;
    if fork == Fork::Data && reflink::clone_file(&src, &dst) {
        return report(p, fork, len);
    }
    if fork == Fork::Data {
        // copyFileRange: in the kernel until it fails, then on from where it
        // got to (the destination's position has moved on already).
        let mut copied: Filesize = 0;
        while copied < len {
            match reflink::copy_file(&src, &dst, copied as u64, copy_size(len - copied)) {
                Ok(n) => {
                    report(p, fork, n as Filesize)?;
                    if n == 0 {
                        return Ok(());
                    }
                    copied += n as Filesize;
                }
                Err(_) => break,
            }
        }
        if copied >= len {
            return Ok(());
        }
        if copied != 0 {
            src.seek(SeekFrom::Start(copied as u64))
                .map_err(|e| io_error(WHAT, &e))?;
        }
    }
    read_write_bounded(&mut src, &mut dst, len, fork, p)
}

/// `localFileContents`: copies both forks to `path_to`, removing what was
/// there.
fn local_file_contents(
    ctx: &Ctx,
    from: (&Fspath, &Path<Local>),
    to: (&Fspath, &Path<Local>),
    desc: &Props,
    ress_length: Filesize,
    p: &mut Progress<'_>,
) -> Result<(), Error> {
    remove_old_temp_file(ctx, to.0, to.1)?;
    copy_contents(ctx, from, to, Fork::Data, desc.length(), p)?;
    if ress_length > 0 {
        copy_contents(ctx, from, to, Fork::Ress, ress_length, p)?;
    }
    Ok(())
}

/// `setFileinfo`: gives the copy the props `desc`, over those of the file
/// it replaces (at `real_path_to`) or the defaults.
fn set_fileinfo(
    ctx: &Ctx,
    fspath_to: &Fspath,
    path_to: &Path<Local>,
    real_path_to: &Path<Local>,
    replacing: Replacing,
    desc: &Props,
) -> Result<(), Error> {
    let default = Props::file_default();
    let action = match replacing {
        Replacing::Update(..) => SetAction::Copy(real_path_to),
        Replacing::Copy => SetAction::Set(&default),
    };
    fileinfo::set(fspath_to, path_to, action, desc, &ctx.fs)
}

/// `localFile`: copies the file at `path_from` to `path_to` (a temporary
/// name for `real_path_to`), with the props `desc`. (OCaml also reads the
/// props' extended data here; gemel doesn't synchronise xattrs and ACLs
/// yet.)
#[allow(clippy::too_many_arguments)]
pub fn local_file(
    ctx: &Ctx,
    fspath_from: &Fspath,
    path_from: &Path<Local>,
    fspath_to: &Fspath,
    path_to: &Path<Local>,
    real_path_to: &Path<Local>,
    replacing: Replacing,
    desc: &Props,
    ress_length: Filesize,
    p: &mut Progress<'_>,
) -> Result<(), Error> {
    local_file_contents(
        ctx,
        (fspath_from, path_from),
        (fspath_to, path_to),
        desc,
        ress_length,
        p,
    )?;
    set_fileinfo(ctx, fspath_to, path_to, real_path_to, replacing, desc)
}

// ---- Transfers between hosts ----
//
// The destination's host runs `transferFile`, with the connection to the
// source's. For each fork, it asks the source to `compress` the file, which
// streams the tokens back (`processTransferInstruction`): as they are, or,
// when the destination has an old version to start from, as rsync deltas
// against its block information. The destination decodes each chunk as it
// arrives, into the temporary file.

/// Which fork a transfer carries, and from where (`` `DATA | `DATA_APPEND
/// of len | `RESS ``).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// The data fork.
    Data,
    /// The data fork, from this offset on (a partial transfer resumed).
    DataAppend(Filesize),
    /// The resource fork.
    Ress,
}

impl FileKind {
    /// `mdata`.
    fn marshal(self, w: &mut Writer) {
        match self {
            FileKind::Data => w.tag(0),
            FileKind::DataAppend(len) => {
                w.tag(1);
                w.int64(len);
            }
            FileKind::Ress => w.tag(2),
        }
    }

    /// `mdata`.
    fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<FileKind> {
        Ok(match r.tag(3)? {
            0 => FileKind::Data,
            1 => FileKind::DataAppend(r.int64()?),
            _ => FileKind::Ress,
        })
    }

    fn fork(self) -> Fork {
        match self {
            FileKind::Ress => Fork::Ress,
            _ => Fork::Data,
        }
    }
}

/// `openFileIn'`: a file to read, from where the transfer starts; errors
/// are worded for `what` OCaml was doing (`convertUnixErrorsToTransient`).
fn open_file_in(
    fspath: &Fspath,
    path: &Path<Local>,
    kind: FileKind,
    what: &str,
) -> Result<std::fs::File, Error> {
    let mut f = open_in(fspath, path, kind.fork(), what)?;
    if let FileKind::DataAppend(len) = kind {
        f.seek(SeekFrom::Start(len as u64))
            .map_err(|e| io_error(what, &e))?;
    }
    Ok(f)
}

/// What OCaml is doing when the source reads its file (`compress`).
const COMPRESSING: &str = "transferring file contents";
/// What OCaml is doing when the destination decodes a chunk, opening its
/// files on the first.
const DECODING: &str = "processing a transfer instruction";

/// `openFileOut'`: the destination; a resumed one keeps what it has.
fn open_file_out(
    ctx: &Ctx,
    fspath: &Fspath,
    path: &Path<Local>,
    kind: FileKind,
) -> Result<std::fs::File, Error> {
    const WHAT: &str = DECODING;
    match kind {
        FileKind::DataAppend(len) => {
            let full = fspath.concat(path);
            let mut f = fs::open_out_existing(&full).map_err(|e| e.to_transient(WHAT))?;
            if !ctx.fs.dont_chmod {
                fs::chmod(&full, 0o600).map_err(|e| e.to_transient(WHAT))?;
            }
            f.seek(SeekFrom::Start(len as u64))
                .map_err(|e| io_error(WHAT, &e))?;
            Ok(f)
        }
        _ => open_out(ctx, fspath, path, kind.fork(), WHAT),
    }
}

/// `compress`'s argument: the destination's block information (for
/// rsync), the source and its fork, the length to send, the item, and the
/// destination's id for the transfer.
#[derive(Debug, Clone)]
pub struct CompressArgs {
    /// The destination's old file, for rsync.
    pub block_info: Option<rsync::BlockInfo>,
    /// The source's root.
    pub fspath_from: Fspath,
    /// The source.
    pub path_from: Path<Local>,
    /// Which fork, from where.
    pub kind: FileKind,
    /// How many bytes to send.
    pub size_from: Filesize,
    /// The item, for progress and aborts.
    pub id: FileId,
    /// The destination's id for the transfer, which each chunk carries.
    pub file_id: i64,
}

/// `compress` (`mcompress`).
pub static COMPRESS: ServerCmd<CompressArgs, ()> = ServerCmd::new(
    "compress",
    |a, w, _| {
        w.option(a.block_info.as_ref(), |w, b| b.marshal(w));
        a.fspath_from.marshal(w);
        a.path_from.marshal(w);
        a.kind.marshal(w);
        w.int64(a.size_from);
        w.int(a.id.0);
        w.int(a.file_id);
    },
    |r, _| {
        Ok(CompressArgs {
            block_info: r.option(rsync::BlockInfo::unmarshal)?,
            fspath_from: Fspath::unmarshal(r)?,
            path_from: Path::unmarshal(r)?,
            kind: FileKind::unmarshal(r)?,
            size_from: r.int64()?,
            id: FileId(r.int()?),
            file_id: r.int()?,
        })
    },
    |(), _, _| {},
    |_, _| Ok(()),
    |conn, args| Box::pin(compress(conn, args)),
);

/// A chunk of a transfer: the destination's id for it, and the tokens.
pub type Instruction = (u32, Vec<u8>);

/// `processTransferInstruction`: decodes a chunk of a transfer into its
/// destination. Its argument is the transfer's id (encoded as message ids
/// are), then the tokens, raw.
pub static PROCESS_TRANSFER_INSTRUCTION: StreamCmd<Instruction> = StreamCmd::new(
    "processTransferInstruction",
    "processTransferInstructionPing",
    |(id, data), out| {
        out.extend_from_slice(&gemel_rpc::protocol::encode_int(*id));
        out.extend_from_slice(data);
    },
    |b| {
        let id = gemel_rpc::protocol::decode_int(b)?;
        Ok((id, b[gemel_rpc::protocol::INT_SIZE..].to_vec()))
    },
    |conn, (id, data)| Box::pin(process_transfer_instruction(conn, id, data)),
);

/// Reports a transfer's progress, on the host doing it, as `"r"`; and stops
/// it when everything is aborted, except for a resource fork.
fn transfer_progress(
    host: &Host,
    id: FileId,
    kind: FileKind,
) -> impl FnMut(usize) -> Result<(), Error> + Send + 'static {
    let printer = uutil::progress_printer(host);
    let abort = host.state::<Abort>().borrow().all_flag();
    move |n| {
        if kind != FileKind::Ress {
            abort.check()?;
        }
        if let Some(p) = &printer {
            p(id, n as Filesize, "r");
        }
        Ok(())
    }
}

/// `Abort.check`, on the host.
fn abort_check(host: &Host, id: FileId) -> Result<(), Error> {
    let max = *host.prefs().borrow().read(&crate::abort::MAX_ERRORS);
    host.state::<Abort>().borrow().check(id, max)
}

/// `compress`, on the source's host: streams the fork's tokens to the
/// destination, as they are or as rsync deltas.
async fn compress(conn: Connection, a: CompressArgs) -> Result<(), Error> {
    let host = conn.host().clone();
    let streaming = *host.prefs().borrow().read(&STREAMING_ACTIVATED);
    let file_id = u32::try_from(a.file_id).map_err(|_| internal("a transfer's id out of range"))?;
    conn.stream(
        &PROCESS_TRANSFER_INSTRUCTION,
        streaming,
        |sender| async move {
            // The transfer is aborted on error only if it hasn't started.
            if a.kind != FileKind::Ress {
                abort_check(&host, a.id)?;
            }
            let mut progress = transfer_progress(&host, a.id, a.kind);
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
            // Read off the engine thread, the chunks coming back as they are
            // made; if sending fails, reading stops at the next chunk.
            let reading = tokio::task::spawn_blocking(move || -> Result<(), Error> {
                let mut f = open_file_in(&a.fspath_from, &a.path_from, a.kind, COMPRESSING)?;
                // `showPrefixProgress`: a resumed transfer's prefix counts.
                if let FileKind::DataAppend(len) = a.kind {
                    progress(len as usize)?;
                }
                let mut transmit = |c: &[u8]| {
                    tx.blocking_send(c.to_vec())
                        .map_err(|_| Error::Transient("Streaming aborted".into()))
                };
                match &a.block_info {
                    None => transfer::send(&mut f, a.size_from, &mut progress, &mut transmit),
                    Some(bi) => {
                        rsync::compress(bi, &mut f, a.size_from, &mut progress, &mut transmit)
                    }
                }
            });
            while let Some(chunk) = rx.recv().await {
                sender.send(&(file_id, chunk)).await?;
            }
            match reading.await {
                Ok(r) => r,
                Err(e) => std::panic::resume_unwind(e.into_panic()),
            }
        },
    )
    .await
}

/// A transfer the destination is receiving (`decompressor`'s entries): the
/// files are opened when the first chunk comes, and closed after the last.
struct Receiving {
    ctx: Ctx,
    fspath_to: Fspath,
    path_to: Path<Local>,
    real_path_to: Path<Local>,
    kind: FileKind,
    rsync: Option<rsync::Decompressor>,
    out: Option<std::io::BufWriter<std::fs::File>>,
    reference: Option<std::fs::File>,
    progress: Box<dyn FnMut(usize) -> Result<(), Error> + Send>,
    done: bool,
}

impl Receiving {
    /// Decodes a chunk; at the end, closes the files.
    fn chunk(&mut self, data: &[u8]) -> Result<(), Error> {
        if self.done {
            return Err(internal("a chunk after the end of a transfer"));
        }
        if self.out.is_none() {
            let f = open_file_out(&self.ctx, &self.fspath_to, &self.path_to, self.kind)?;
            self.out = Some(std::io::BufWriter::new(f));
            if let FileKind::DataAppend(len) = self.kind {
                (self.progress)(len as usize)?;
            }
        }
        let out = self.out.as_mut().expect("opened");
        let eof = match &mut self.rsync {
            None => transfer::receive(out, &mut *self.progress, data)?,
            Some(d) => {
                if self.reference.is_none() {
                    self.reference = Some(open_file_in(
                        &self.fspath_to,
                        &self.real_path_to,
                        self.kind,
                        DECODING,
                    )?);
                }
                let reference = self.reference.as_mut().expect("opened");
                d.chunk(reference, out, &mut *self.progress, data)?
            }
        };
        if eof {
            self.close()?;
        }
        Ok(())
    }

    /// `close_all`.
    fn close(&mut self) -> Result<(), Error> {
        self.done = true;
        self.reference = None;
        if let Some(out) = self.out.take() {
            out.into_inner()
                .map_err(|e| io_error("closing files", e.error()))?;
        }
        Ok(())
    }
}

/// The transfers a host is receiving, by their ids.
#[derive(Default)]
struct Transfers(HashMap<u32, Arc<std::sync::Mutex<Receiving>>>);

/// `processTransferInstruction`, on the destination's host.
async fn process_transfer_instruction(
    conn: Connection,
    file_id: u32,
    data: Vec<u8>,
) -> Result<(), Error> {
    let host = conn.host().clone();
    let receiving = host
        .state::<Transfers>()
        .borrow()
        .0
        .get(&file_id)
        .cloned()
        .ok_or_else(|| internal("a chunk for an unknown transfer"))?;
    exclusive::blocking(&host, move || {
        receiving
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .chunk(&data)
    })
    .await
}

/// `transferFileContents`, on the destination's host: receives a fork from
/// the source over `conn_from`, as rsync deltas when both files are big
/// enough and there is an old file to start from.
#[allow(clippy::too_many_arguments)]
async fn transfer_file_contents(
    conn_from: &Connection,
    a: &TransferArgs,
    kind: FileKind,
    size_from: Filesize,
) -> Result<(), Error> {
    let host = conn_from.host().clone();
    let ctx = Ctx::of(&host)?;
    let dest_size = match (a.replacing, kind) {
        (Replacing::Copy, _) => 0,
        (Replacing::Update(_, ress), FileKind::Ress) => ress,
        (Replacing::Update(data, _), _) => data,
    };
    let use_rsync = *host.prefs().borrow().read(&RSYNC_ACTIVATED)
        && rsync::above_rsync_threshold(dest_size)
        && rsync::above_rsync_threshold(size_from);
    let block_info = if use_rsync {
        let (fspath, path) = (a.fspath_to.clone(), a.real_path_to.clone());
        Some(
            exclusive::blocking(&host, move || {
                let mut f = open_file_in(&fspath, &path, kind, "preprocessing file")?;
                rsync::preprocess(&mut f, size_from, dest_size)
            })
            .await?,
        )
    } else {
        None
    };
    // The transfer is aborted on error only if it hasn't started.
    if kind != FileKind::Ress {
        abort_check(&host, a.id)?;
    }
    let receiving = Receiving {
        ctx,
        fspath_to: a.fspath_to.clone(),
        path_to: a.path_to.clone(),
        real_path_to: a.real_path_to.clone(),
        kind,
        rsync: block_info
            .as_ref()
            .map(|b| rsync::Decompressor::new(b.block_size)),
        out: None,
        reference: None,
        progress: Box::new(transfer_progress(&host, a.id, kind)),
        done: false,
    };
    let file_id = conn_from.endpoint().new_msg_id();
    let receiving = Arc::new(std::sync::Mutex::new(receiving));
    host.state::<Transfers>()
        .borrow_mut()
        .0
        .insert(file_id, receiving.clone());
    let args = CompressArgs {
        block_info,
        fspath_from: a.fspath_from.clone(),
        path_from: a.path_from.clone(),
        kind,
        size_from,
        id: a.id,
        file_id: i64::from(file_id),
    };
    let result = conn_from.call(&COMPRESS, args).await;
    host.state::<Transfers>().borrow_mut().0.remove(&file_id);
    let closed = exclusive::blocking(&host, move || {
        receiving.lock().unwrap_or_else(|e| e.into_inner()).close()
    })
    .await;
    result.and(closed)
}

/// `transferFile`'s argument (`mfinishExternalTransfer`).
#[derive(Debug, Clone)]
pub struct TransferArgs {
    /// The source's root.
    pub fspath_from: Fspath,
    /// The source.
    pub path_from: Path<Local>,
    /// The destination's root, or the directory the copy goes in.
    pub fspath_to: Fspath,
    /// The temporary file to copy to.
    pub path_to: Path<Local>,
    /// The file it will replace.
    pub real_path_to: Path<Local>,
    /// What it replaces.
    pub replacing: Replacing,
    /// The source's props.
    pub desc: Props,
    /// The source's fingerprint.
    pub fp: FullFingerprint,
    /// The source's resource fork.
    pub ress: RessStamp,
    /// The item.
    pub id: FileId,
}

impl TransferArgs {
    fn marshal(&self, w: &mut Writer, c: &PropsConfig) {
        self.fspath_from.marshal(w);
        self.path_from.marshal(w);
        self.fspath_to.marshal(w);
        self.path_to.marshal(w);
        self.real_path_to.marshal(w);
        match self.replacing {
            Replacing::Copy => w.tag(0),
            Replacing::Update(data, ress) => {
                w.tag(1);
                w.int64(data);
                w.int64(ress);
            }
        }
        self.desc.marshal(w, c);
        self.fp.marshal(w);
        self.ress.marshal_stamp(w);
        w.int(self.id.0);
    }

    fn unmarshal(r: &mut Reader<'_>, c: &PropsConfig) -> umarshal::Result<TransferArgs> {
        Ok(TransferArgs {
            fspath_from: Fspath::unmarshal(r)?,
            path_from: Path::unmarshal(r)?,
            fspath_to: Fspath::unmarshal(r)?,
            path_to: Path::unmarshal(r)?,
            real_path_to: Path::unmarshal(r)?,
            replacing: match r.tag(2)? {
                0 => Replacing::Copy,
                _ => Replacing::Update(r.int64()?, r.int64()?),
            },
            desc: Props::unmarshal(r, c)?,
            fp: FullFingerprint::unmarshal(r)?,
            ress: RessStamp::unmarshal_stamp(r)?,
            id: FileId(r.int()?),
        })
    }
}

impl TransferStatus {
    /// `mtransferStatus`.
    fn marshal(&self, w: &mut Writer, c: &PropsConfig) {
        match self {
            TransferStatus::Succeeded(info) => {
                w.tag(0);
                info.marshal(w, c);
            }
            TransferStatus::NeedsDoubleCheckAgainstCurrentSource(info, fp) => {
                w.tag(1);
                info.marshal(w, c);
                fp.marshal(w);
            }
            TransferStatus::Failed(reason) => {
                w.tag(2);
                w.string(reason.as_bytes());
            }
        }
    }

    /// `mtransferStatus`.
    fn unmarshal(r: &mut Reader<'_>, c: &PropsConfig) -> umarshal::Result<TransferStatus> {
        Ok(match r.tag(3)? {
            0 => TransferStatus::Succeeded(Info::unmarshal(r, c)?),
            1 => TransferStatus::NeedsDoubleCheckAgainstCurrentSource(
                Info::unmarshal(r, c)?,
                FullFingerprint::unmarshal(r)?,
            ),
            _ => TransferStatus::Failed(String::from_utf8_lossy(&r.string()?).into_owned()),
        })
    }
}

/// What `transferFile` did (`mtransferFile`).
#[derive(Debug, Clone, PartialEq)]
pub enum Transferred {
    /// The transfer's status, and a message for the log if it took a
    /// shortcut.
    Done(Box<TransferStatus>, Option<String>),
    /// An external program is to copy it (whether the destination has a
    /// partial copy already); OCaml servers answer this with `copyprog`.
    External(bool),
}

/// `transferFile` (a root command with the connection): transfers a file
/// to the destination's host, which runs it.
pub static TRANSFER_FILE: ServerCmd<TransferArgs, Transferred> = ServerCmd::new(
    "transferFile",
    |a, w, f| a.marshal(w, &PropsConfig::default().with_features(f)),
    |r, f| TransferArgs::unmarshal(r, &PropsConfig::default().with_features(f)),
    |t, w, f| match t {
        Transferred::Done(status, msg) => {
            w.tag(0);
            status.marshal(w, &PropsConfig::default().with_features(f));
            w.option(msg.as_ref(), |w, m| w.string(m.as_bytes()));
        }
        Transferred::External(existing) => {
            w.tag(1);
            w.bool(*existing);
        }
    },
    |r, f| {
        Ok(match r.tag(2)? {
            0 => Transferred::Done(
                Box::new(TransferStatus::unmarshal(
                    r,
                    &PropsConfig::default().with_features(f),
                )?),
                r.option(|r| Ok(String::from_utf8_lossy(&r.string()?).into_owned()))?,
            ),
            _ => Transferred::External(r.bool()?),
        })
    },
    |conn, a| Box::pin(transfer_file_local(conn, a)),
);

/// `fileIsTransferred`: whether the temporary file is the source already
/// (from an interrupted transfer), and its info.
fn file_is_transferred(ctx: &Ctx, a: &TransferArgs) -> Result<(Info, bool), Error> {
    let info = fileinfo::get_basic_with_ress(false, &a.fspath_to, &a.path_to, &|_| false, &ctx.fs)?;
    let done = info.typ == Typ::File
        && info.desc.length() == a.desc.length()
        && osx::ress_length(&info.os_x.ress_info) == osx::ress_length(&a.ress)
        && fingerprint(ctx, &a.fspath_to, &a.path_to, info.typ)? == a.fp;
    Ok((info, done))
}

/// `shouldUseExternalCopyprog`: what OCaml would hand to `copyprog`.
fn should_use_external_copyprog(prefs: &Prefs, a: &TransferArgs) -> bool {
    let threshold = *prefs.read(&COPYTHRESHOLD);
    !prefs.read(&COPYPROG).is_empty()
        && threshold >= 0
        && a.desc.length() >= 1
        && a.desc.length() >= 1000 * threshold
        && a.replacing == Replacing::Copy
}

/// Whether the warning that `copyprog` is ignored was given, on a host.
#[derive(Default)]
struct WarnedCopyprog(bool);

/// Warns, once on each host, that external copy programs are ignored.
fn warn_copyprog_ignored(host: &Host) {
    let warned = host.state::<WarnedCopyprog>();
    if !std::mem::replace(&mut warned.borrow_mut().0, true) {
        host.warn(
            "External copy programs (copyprog, copythreshold) are not supported; \
             transferring files with the built-in protocol instead.\n",
        );
    }
}

/// The client's preferences as sent to a server (`dump`), with external
/// copying off (`copythreshold = -1`): a server that is OCaml's would
/// otherwise have files copied by `copyprog`, which a gemel client can't
/// finish. If the preferences turn it on, the client warns that it's
/// ignored, as a gemel server would.
pub fn without_external_copies(
    client: &Host,
    dump: &mut [DumpedPref],
    features: &Features,
) -> Result<(), Error> {
    let on = {
        let prefs = client.prefs().borrow();
        !prefs.read(&COPYPROG).is_empty() && *prefs.read(&COPYTHRESHOLD) >= 0
    };
    if on {
        warn_copyprog_ignored(client);
    }
    let mut off = Prefs::new();
    off.register(&COPYTHRESHOLD)?;
    off.set(&COPYTHRESHOLD, -1);
    let off = off.dump(1, features)?;
    for d in dump.iter_mut() {
        if let Some(o) = off.iter().find(|o| o.0 == d.0) {
            *d = o.clone();
        }
    }
    Ok(())
}

/// `tryCopyMovedFile`: copies a file this host already has with the same
/// contents (found by `xferhint`), instead of transferring it. The copy's
/// info and the message for the log, if it worked; if not, the hint is
/// forgotten.
async fn try_copy_moved_file(
    host: &Host,
    ctx: &Ctx,
    a: &TransferArgs,
) -> Result<Option<(Info, String)>, Error> {
    if !*host.prefs().borrow().read(&xferhint::XFER_BY_COPYING) {
        return Ok(None);
    }
    let Some((cand_fspath, cand_path)) = xferhint::lookup(host, &a.fp) else {
        return Ok(None);
    };
    let printer = uutil::progress_printer(host);
    let abort = host.state::<Abort>().borrow().all_flag();
    // An error reading or copying the candidate is the outer one, and the
    // transfer goes on without it; one setting the copy's props or
    // checking it fails the transfer, as in OCaml, where those come after
    // a bind that its `try` doesn't reach.
    type Copied = Result<Result<Option<(Info, String)>, Error>, Error>;
    let copied = {
        let (ctx, a) = (ctx.clone(), a.clone());
        exclusive::blocking(host, move || -> Copied {
            // If the candidate is the replica's root, `from_root` is
            // right; otherwise it isn't expected to be a link, and it is
            // right still.
            let info = fileinfo::get_basic(true, &cand_fspath, &cand_path, &|_| false, &ctx.fs)?;
            if info.typ == Typ::Absent || info.desc.length() != a.desc.length() {
                // The candidate has gone.
                return Ok(Ok(None));
            }
            let mut progress = |n| {
                if let Some(p) = &printer {
                    p(a.id, n, "l");
                }
            };
            let check = || abort.check();
            local_file_contents(
                &ctx,
                (&cand_fspath, &cand_path),
                (&a.fspath_to, &a.path_to),
                &a.desc,
                osx::ress_length(&a.ress),
                &mut Progress {
                    progress: &mut progress,
                    check: &check,
                },
            )?;
            // (OCaml reads the props' extended data here; gemel doesn't
            // synchronise xattrs and ACLs yet.)
            Ok((|| {
                set_fileinfo(
                    &ctx,
                    &a.fspath_to,
                    &a.path_to,
                    &a.real_path_to,
                    a.replacing,
                    &a.desc,
                )?;
                let (info, transferred) = file_is_transferred(&ctx, &a)?;
                if !transferred {
                    // The candidate has changed.
                    return Ok(None);
                }
                let msg = format!(
                    "Shortcut: copied {}/{} from local file {}/{}\n",
                    String::from_utf8_lossy(a.fspath_to.as_bytes()),
                    String::from_utf8_lossy(a.real_path_to.as_bytes()),
                    String::from_utf8_lossy(cand_fspath.as_bytes()),
                    String::from_utf8_lossy(cand_path.as_bytes()),
                );
                Ok(Some((info, msg)))
            })())
        })
        .await
    };
    match copied {
        Ok(Ok(Some(done))) => Ok(Some(done)),
        Ok(Ok(None)) | Err(Error::Transient(_)) => {
            xferhint::delete_entry(host, &a.fp);
            Ok(None)
        }
        Ok(Err(e)) | Err(e) => Err(e),
    }
}

/// `transferFileLocal`, on the destination's host, with the connection to
/// the source's.
async fn transfer_file_local(conn_from: Connection, a: TransferArgs) -> Result<Transferred, Error> {
    let host = conn_from.host().clone();
    let ctx = Ctx::of(&host)?;
    let (temp_info, transferred) = {
        let (ctx, a) = (ctx.clone(), a.clone());
        exclusive::blocking(&host, move || file_is_transferred(&ctx, &a)).await?
    };
    let hint = (a.fspath_to.clone(), a.path_to.clone(), a.fp.clone());
    if transferred {
        // From an interrupted transfer: only the props are left to set.
        let msg = format!(
            "{}/{} has already been transferred\n",
            gemel_base::util::escaped(a.fspath_to.as_bytes()),
            String::from_utf8_lossy(a.real_path_to.as_bytes())
        );
        if let Some(p) = uutil::progress_printer(&host) {
            p(a.id, a.desc.length() + osx::ress_length(&a.ress), "alr");
        }
        {
            let (ctx, a) = (ctx.clone(), a.clone());
            exclusive::blocking(&host, move || {
                set_fileinfo(
                    &ctx,
                    &a.fspath_to,
                    &a.path_to,
                    &a.real_path_to,
                    a.replacing,
                    &a.desc,
                )
            })
            .await?;
        }
        xferhint::insert_entry(&host, hint);
        return Ok(Transferred::Done(
            Box::new(TransferStatus::Succeeded(temp_info)),
            Some(msg),
        ));
    }
    let fp = a.fp.clone();
    register_file_transfer(&host, &fp, async {
        if let Some((info, msg)) = try_copy_moved_file(&host, &ctx, &a).await? {
            xferhint::insert_entry(&host, hint);
            return Ok(Transferred::Done(
                Box::new(TransferStatus::Succeeded(info)),
                Some(msg),
            ));
        }
        if should_use_external_copyprog(&host.prefs().borrow(), &a) {
            warn_copyprog_ignored(&host);
        }
        let status = really_transfer_file(&conn_from, &ctx, &a, &temp_info).await?;
        xferhint::insert_entry(&host, hint);
        Ok(Transferred::Done(Box::new(status), None))
    })
    .await
}

/// `filesBeingTransferred`: for the contents being transferred to a host,
/// the transfers of the same contents waiting for theirs to end.
#[derive(Default)]
struct BeingTransferred(HashMap<FullFingerprint, VecDeque<tokio::sync::oneshot::Sender<()>>>);

/// `registerFileTransfer`: runs `transfer`, after any transfer of the same
/// contents under way on this host, so that it can copy what that one
/// made instead (see [`try_copy_moved_file`]).
async fn register_file_transfer<T>(
    host: &Host,
    fp: &FullFingerprint,
    transfer: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    if !*host.prefs().borrow().read(&xferhint::XFER_BY_COPYING) {
        return transfer.await;
    }
    let table = host.state::<BeingTransferred>();
    let turn = {
        let mut t = table.borrow_mut();
        match t.0.get_mut(fp) {
            None => {
                t.0.insert(fp.clone(), VecDeque::new());
                None
            }
            Some(waiting) => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                waiting.push_back(tx);
                Some(rx)
            }
        }
    };
    if let Some(turn) = turn {
        let _ = turn.await;
    }
    let result = transfer.await;
    // `wakeupNextTransfer`.
    let mut t = table.borrow_mut();
    match t.0.get_mut(fp).and_then(VecDeque::pop_front) {
        Some(next) => {
            let _ = next.send(());
        }
        None => {
            t.0.remove(fp);
        }
    }
    result
}

/// `fingerprintLimit`: how many bytes `fingerprintPrefix` digests at once.
const FINGERPRINT_LIMIT: Filesize = 1_072_693_248;

/// `fingerprintPrefix`: the digests of the first `len` bytes of
/// `fspath/path`, [`FINGERPRINT_LIMIT`] bytes at a time, the last first.
fn fingerprint_prefix(
    fspath: &Fspath,
    path: &Path<Local>,
    len: Filesize,
) -> Result<Vec<Fingerprint>, Error> {
    let full = fspath.concat(path);
    let (mut offset, mut len, mut fps) = (0, len, Vec::new());
    while len > 0 {
        let l = len.min(FINGERPRINT_LIMIT);
        fps.push(Fingerprint::subfile(&full, offset, l)?);
        offset += l as u64;
        len -= l;
    }
    fps.reverse();
    Ok(fps)
}

/// `fingerprintSubfile` (`fingerprintPrefixRemotely`), on the source's
/// host: [`fingerprint_prefix`].
pub static FINGERPRINT_SUBFILE: ServerCmd<(Fspath, Path<Local>, Filesize), Vec<Fingerprint>> =
    ServerCmd::new(
        "fingerprintSubfile",
        |(fspath, path, len), w, _| {
            fspath.marshal(w);
            path.marshal(w);
            w.int64(*len);
        },
        |r, _| Ok((Fspath::unmarshal(r)?, Path::unmarshal(r)?, r.int64()?)),
        |fps, w, _| w.list(fps, |w, fp| fp.marshal(w)),
        |r, _| r.list(Fingerprint::unmarshal),
        |conn, (fspath, path, len)| {
            Box::pin(async move {
                exclusive::blocking(conn.host(), move || fingerprint_prefix(&fspath, &path, len))
                    .await
            })
        },
    );

/// `appendThreshold`: a temporary file left by an interrupted transfer is
/// kept only from this length on.
const APPEND_THRESHOLD: Filesize = 1024 * 1024;

/// `validFilePrefix`: how much of the temporary file (`info`) an
/// interrupted transfer left that the source starts with, if it is long
/// enough to keep and shorter than the source. The source failing to
/// digest its file means none.
async fn valid_file_prefix(
    conn_from: &Connection,
    a: &TransferArgs,
    info: &Info,
) -> Result<Option<Filesize>, Error> {
    let len = info.desc.length();
    if !(info.typ == Typ::File && len >= APPEND_THRESHOLD && len < a.desc.length()) {
        return Ok(None);
    }
    let from = conn_from
        .call(
            &FINGERPRINT_SUBFILE,
            (a.fspath_from.clone(), a.path_from.clone(), len),
        )
        .await;
    let Ok(from) = from else {
        return Ok(None);
    };
    let (f, p) = (a.fspath_to.clone(), a.path_to.clone());
    let to = exclusive::blocking(conn_from.host(), move || fingerprint_prefix(&f, &p, len)).await?;
    Ok((from == to).then_some(len))
}

/// `reallyTransferFile`: the data fork, from where an interrupted transfer
/// left it if it can, then the resource fork and the props, then the
/// check.
async fn really_transfer_file(
    conn_from: &Connection,
    ctx: &Ctx,
    a: &TransferArgs,
    temp_info: &Info,
) -> Result<TransferStatus, Error> {
    let host = conn_from.host().clone();
    let prefix = valid_file_prefix(conn_from, a, temp_info).await?;
    if prefix.is_none() {
        let (ctx, f, p) = (ctx.clone(), a.fspath_to.clone(), a.path_to.clone());
        exclusive::blocking(&host, move || remove_old_temp_file(&ctx, &f, &p)).await?;
    }
    let kind = prefix.map_or(FileKind::Data, FileKind::DataAppend);
    transfer_file_contents(conn_from, a, kind, a.desc.length()).await?;
    // `transferResourceForkAndSetFileinfo`.
    let ress_length = osx::ress_length(&a.ress);
    if ress_length > 0 {
        transfer_file_contents(conn_from, a, FileKind::Ress, ress_length).await?;
    }
    let (ctx, a) = (ctx.clone(), a.clone());
    exclusive::blocking(&host, move || {
        set_fileinfo(
            &ctx,
            &a.fspath_to,
            &a.path_to,
            &a.real_path_to,
            a.replacing,
            &a.desc,
        )?;
        paranoid_check(&ctx, &a.fspath_to, &a.path_to, &a.fp)
    })
    .await
}

/// `transferFile`'s client side: transfers to `root_to` from `root_from`,
/// one of which is remote.
#[allow(clippy::too_many_arguments)]
async fn transfer_file(
    rpc: &Rpc,
    root_from: &Root,
    path_from: &Path<Local>,
    root_to: &Root,
    fspath_to: &Fspath,
    path_to: &Path<Local>,
    real_path_to: &Path<Local>,
    replacing: Replacing,
    source: &Source,
    id: FileId,
) -> Result<TransferStatus, Error> {
    abort_check(rpc.client_host(), id)?;
    let args = TransferArgs {
        fspath_from: root_from.fspath.clone(),
        path_from: path_from.clone(),
        fspath_to: fspath_to.clone(),
        path_to: path_to.clone(),
        real_path_to: real_path_to.clone(),
        replacing,
        desc: source.desc.clone(),
        fp: source.fp.clone(),
        ress: source.ress.clone(),
        id,
    };
    match rpc
        .call_root_with_connection(&TRANSFER_FILE, root_to, root_from, args)
        .await?
    {
        Transferred::Done(status, msg) => {
            if let Some(msg) = msg {
                // Done on the server without a transfer: the progress the
                // transfer would have shown.
                if !root_to.is_local()
                    && let Some(p) = uutil::progress_printer(rpc.client_host())
                {
                    p(
                        id,
                        source.desc.length() + osx::ress_length(&source.ress),
                        "rem",
                    );
                }
                crate::with_trace(rpc, |t, p| t.log(p, &msg))?;
            }
            Ok(*status)
        }
        Transferred::External(_) => Err(Error::Transient(format!(
            "The server would copy {} with an external program (copyprog), which isn't \
             supported: set copythreshold to -1",
            String::from_utf8_lossy(real_path_to.as_bytes())
        ))),
    }
}

/// What [`file`] copies: the source's archived record.
#[derive(Debug, Clone)]
pub struct Source {
    /// Its props (with the replica's synchronised ones).
    pub desc: Props,
    /// Its fingerprint.
    pub fp: FullFingerprint,
    /// Its stamp, if known.
    pub stamp: Option<Stamp>,
    /// Its resource fork.
    pub ress: RessStamp,
}

/// `Copy.file`: copies the file at `path_from` on `root_from` to `path_to`
/// on `root_to` (a temporary name for `real_path_to`), checks the copy and
/// that the source didn't change meanwhile, and returns the copy's info.
/// Between local roots, the file is copied here; otherwise the
/// destination's host has it transferred from the source's.
#[allow(clippy::too_many_arguments)]
pub async fn file(
    rpc: &Rpc,
    root_from: &Root,
    path_from: &Path<Local>,
    root_to: &Root,
    fspath_to: &Fspath,
    path_to: &Path<Local>,
    real_path_to: &Path<Local>,
    replacing: Replacing,
    source: &Source,
    id: FileId,
) -> Result<Info, Error> {
    let timer = crate::start_timer(rpc, "Transmitting file")?;
    let status = if !(root_from.is_local() && root_to.is_local()) {
        transfer_file(
            rpc,
            root_from,
            path_from,
            root_to,
            fspath_to,
            path_to,
            real_path_to,
            replacing,
            source,
            id,
        )
        .await?
    } else {
        let ctx = Ctx::of(rpc.local_host())?;
        let (fspath_from, path_from) = (root_from.fspath.clone(), path_from.clone());
        let (fspath_to, path_to, real_path_to) =
            (fspath_to.clone(), path_to.clone(), real_path_to.clone());
        let (desc, fp) = (source.desc.clone(), source.fp.clone());
        let ress_length = osx::ress_length(&source.ress);
        // The client's, where the user interface reports and aborts.
        let client = rpc.client_host();
        let printer = uutil::progress_printer(client);
        let abort = client.state::<Abort>().borrow().all_flag();
        exclusive::blocking(rpc.local_host(), move || {
            let mut progress = |n| {
                if let Some(p) = &printer {
                    p(id, n, "l");
                }
            };
            let check = || abort.check();
            local_file(
                &ctx,
                &fspath_from,
                &path_from,
                &fspath_to,
                &path_to,
                &real_path_to,
                replacing,
                &desc,
                ress_length,
                &mut Progress {
                    progress: &mut progress,
                    check: &check,
                },
            )?;
            paranoid_check(&ctx, &fspath_to, &path_to, &fp)
        })
        .await?
    };
    crate::show_timer(rpc, &timer)?;
    let check = |new_fp, paranoid| SourceCheck {
        path: path_from.clone(),
        desc: source.desc.clone(),
        fp: source.fp.clone(),
        stamp: source.stamp,
        ress: source.ress.clone(),
        new_fp,
        paranoid,
    };
    match status {
        TransferStatus::Succeeded(info) => {
            rpc.call_root(&CHECK_FOR_CHANGES_TO_SOURCE, root_from, check(None, false))
                .await?;
            Ok(info)
        }
        TransferStatus::NeedsDoubleCheckAgainstCurrentSource(info, new_fp) => {
            rpc.call_root(
                &CHECK_FOR_CHANGES_TO_SOURCE,
                root_from,
                check(Some(new_fp), false),
            )
            .await?;
            Ok(info)
        }
        TransferStatus::Failed(reason) => {
            // Maybe the source changed: that is the error to report.
            rpc.call_root(&CHECK_FOR_CHANGES_TO_SOURCE, root_from, check(None, true))
                .await?;
            rpc.call_root(
                &SAVE_TEMP_FILE,
                root_to,
                (path_to.clone(), real_path_to.clone(), reason),
            )
            .await?;
            Err(internal("saving a bad copy didn't fail"))
        }
    }
}

/// `Copy.recursively`: copies a tree (links, files and directories; what
/// doesn't exist is skipped).
pub fn recursively(
    ctx: &Ctx,
    fspath_from: &Fspath,
    path_from: &Path<Local>,
    fspath_to: &Fspath,
    path_to: &Path<Local>,
) -> Result<(), Error> {
    let info = fileinfo::get(true, fspath_from, path_from, &ctx.follow(), &ctx.fs)?;
    match info.typ {
        Typ::Symlink => os::symlink(fspath_to, path_to, &os::read_link(fspath_from, path_from)?),
        Typ::File => Progress::none(|p| {
            local_file(
                ctx,
                fspath_from,
                path_from,
                fspath_to,
                path_to,
                path_to,
                Replacing::Copy,
                &info.desc,
                osx::ress_length(&info.os_x.ress_info),
                p,
            )
        }),
        Typ::Directory => {
            os::create_dir(fspath_to, path_to, info.desc.perm.bits as u32)?;
            for n in os::children_of(fspath_from, path_from, &ctx.temp_names, &ctx.fs)? {
                recursively(
                    ctx,
                    fspath_from,
                    &path_from.child(&n),
                    fspath_to,
                    &path_to.child(&n),
                )?;
            }
            Ok(())
        }
        // Copying onto a conflicting deletion (`copyonconflict`) can get here.
        Typ::Absent => Ok(()),
    }
}

/// Registers this module's commands.
pub fn register_commands(rpc: &mut Rpc) -> Result<(), Error> {
    let r = rpc.registry_mut();
    r.register_root(&CHECK_FOR_CHANGES_TO_SOURCE)?;
    r.register_root(&SAVE_TEMP_FILE)?;
    r.register_stream(&PROCESS_TRANSFER_INSTRUCTION)?;
    r.register_server(&FINGERPRINT_SUBFILE)?;
    r.register_server(&COMPRESS)?;
    r.register_server(&TRANSFER_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gemel_base::prefs::BoolWithDefault;

    struct Dir(std::path::PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            let d = std::env::temp_dir().join(format!("gemel-copy-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Dir(d)
        }

        #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
        fn fspath(&self) -> Fspath {
            Fspath::canonize(Some(self.0.to_str().unwrap().as_bytes())).unwrap()
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn ctx() -> Ctx {
        Ctx {
            fs: FsConfig {
                props: PropsConfig::default(),
                allow_symlinks: BoolWithDefault::Default,
                symlinks_allowed: true,
                ignore_inode_numbers: false,
                dont_chmod: false,
            },
            follow: Arc::new(Compiled::new(Vec::new(), case::Mode::Sensitive).unwrap()),
            temp_names: TempNames::default(),
        }
    }

    /// Copies `len` bytes of a file of `size` bytes by reading and writing,
    /// failing the abort check after `abort_after` checks.
    fn read_write(
        size: usize,
        len: Filesize,
        abort_after: usize,
    ) -> (Result<(), Error>, Vec<u8>, Filesize, usize) {
        let d = Dir::new(&format!("rw-{size}-{len}-{abort_after}"));
        let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        std::fs::write(d.0.join("src"), &data).unwrap();
        let mut src = std::fs::File::open(d.0.join("src")).unwrap();
        let mut dst = std::fs::File::create(d.0.join("dst")).unwrap();
        let mut seen = 0;
        let checks = std::cell::Cell::new(0);
        let mut progress = |n| seen += n;
        let check = || {
            checks.set(checks.get() + 1);
            if checks.get() > abort_after {
                Err(Error::Transient(crate::abort::ABORTED_BY_USER.into()))
            } else {
                Ok(())
            }
        };
        let r = read_write_bounded(
            &mut src,
            &mut dst,
            len,
            Fork::Data,
            &mut Progress {
                progress: &mut progress,
                check: &check,
            },
        );
        drop(dst);
        (
            r,
            std::fs::read(d.0.join("dst")).unwrap(),
            seen,
            checks.get(),
        )
    }

    #[test]
    fn reading_and_writing_stops_at_the_length_or_the_end() {
        let data: Vec<u8> = (0..300_000).map(|i| (i % 251) as u8).collect();
        // More than a buffer, all of it: progress about every 100 KiB.
        let (r, out, seen, checks) = read_write(300_000, 300_000, usize::MAX);
        assert!(r.is_ok());
        assert_eq!((out, seen, checks), (data.clone(), 300_000, 3));
        // Less than the file.
        let (r, out, seen, _) = read_write(300_000, 70_000, usize::MAX);
        assert!(r.is_ok());
        assert_eq!((&out[..], seen), (&data[..70_000], 70_000));
        // A file shorter than the length.
        let (r, out, seen, _) = read_write(1000, 300_000, usize::MAX);
        assert!(r.is_ok());
        assert_eq!((&out[..], seen), (&data[..1000], 1000));
    }

    #[test]
    fn reading_and_writing_can_be_aborted() {
        let (r, out, _, checks) = read_write(300_000, 300_000, 1);
        assert!(crate::abort::is_abort(&r.unwrap_err()));
        assert_eq!(checks, 2);
        assert!(out.len() < 300_000);
    }

    #[test]
    fn a_copy_stops_when_another_thread_aborts_everything() {
        let d = Dir::new("rw-abort-flag");
        std::fs::write(d.0.join("src"), vec![7u8; 300_000]).unwrap();
        let mut src = std::fs::File::open(d.0.join("src")).unwrap();
        let mut dst = std::fs::File::create(d.0.join("dst")).unwrap();
        let flag = crate::abort::Abort::default().all_flag();
        // At the first report, another thread aborts everything.
        let setter = flag.clone();
        let mut progress = |_| {
            let setter = setter.clone();
            std::thread::spawn(move || setter.set()).join().unwrap();
        };
        let check = || flag.check();
        let r = read_write_bounded(
            &mut src,
            &mut dst,
            300_000,
            Fork::Data,
            &mut Progress {
                progress: &mut progress,
                check: &check,
            },
        );
        drop(dst);
        assert!(crate::abort::is_abort(&r.unwrap_err()));
        let copied = std::fs::metadata(d.0.join("dst")).unwrap().len();
        assert!(copied < 300_000, "{copied}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn resource_forks_copy_by_reading_and_writing() {
        let d = Dir::new("ress");
        std::fs::write(d.0.join("src"), "data").unwrap();
        std::fs::write(d.0.join("src/..namedfork/rsrc"), "the resource fork").unwrap();
        // A file that wasn't cloned, so has no fork yet.
        std::fs::write(d.0.join("dst"), "data").unwrap();
        let (fspath, ctx) = (d.fspath(), ctx());
        let checks = std::cell::Cell::new(0);
        let mut seen = 0;
        let mut progress = |n| seen += n;
        let check = || {
            checks.set(checks.get() + 1);
            Ok(())
        };
        copy_contents(
            &ctx,
            (&fspath, &Path::from_bytes(b"src").unwrap()),
            (&fspath, &Path::from_bytes(b"dst").unwrap()),
            Fork::Ress,
            17,
            &mut Progress {
                progress: &mut progress,
                check: &check,
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(d.0.join("dst/..namedfork/rsrc")).unwrap(),
            "the resource fork"
        );
        // Resource forks report progress but aren't aborted.
        assert_eq!((seen, checks.get()), (17, 0));
    }
}
