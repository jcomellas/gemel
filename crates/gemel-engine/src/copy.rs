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
//! Not ported yet (milestone 3, with remote roots): transfers between
//! hosts (`transferFile`, rsync, compression, resuming a partial
//! transfer, `copyprog`), and with them the shortcut of copying an
//! identical local file instead (`tryCopyMovedFile`), which OCaml only
//! takes for remote transfers. Their preferences are registered.
//! Progress reporting (`Uutil.showProgress`) and the timers come with the
//! user interface (phase 7).

use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use gemel_base::case;
use gemel_base::path::{Local, Path};
use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
use gemel_base::util::{Error, FileId};
use gemel_compat::umarshal::{self, Reader, Writer};
use gemel_fs::fileinfo::{self, FsConfig, Info, SetAction, Stamp, Typ};
use gemel_fs::fs;
use gemel_fs::fspath::Fspath;
use gemel_fs::os::{self, FullFingerprint, TempNames};
use gemel_fs::osx::{self, RessStamp};
use gemel_fs::props::{Filesize, Props, PropsConfig};
use gemel_fs::reflink;
use gemel_rpc::host::Host;
use gemel_rpc::registry::{RootCmd, Rpc};
use gemel_rpc::root::Root;

use crate::pred::{self, Compiled};
use crate::{exclusive, fpcache, update};

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
/// Only between local roots so far.
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
    _id: FileId,
) -> Result<Info, Error> {
    if !(root_from.is_local() && root_to.is_local()) {
        return Err(Error::Fatal(
            "Copying between hosts is not supported yet".to_owned(),
        ));
    }
    let ctx = Ctx::of(rpc.local_host())?;
    let status = {
        let (fspath_from, path_from) = (root_from.fspath.clone(), path_from.clone());
        let (fspath_to, path_to, real_path_to) =
            (fspath_to.clone(), path_to.clone(), real_path_to.clone());
        let (desc, fp) = (source.desc.clone(), source.fp.clone());
        let ress_length = osx::ress_length(&source.ress);
        exclusive::blocking(rpc.local_host(), move || {
            Progress::none(|p| {
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
                    p,
                )
            })?;
            paranoid_check(&ctx, &fspath_to, &path_to, &fp)
        })
        .await?
    };
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
    r.register_root(&SAVE_TEMP_FILE)
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
