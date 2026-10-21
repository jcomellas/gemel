//! File-level operations used by transport.
//!
//! Ports Unison `src/files.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! The operations propagation applies to a replica: deleting, setting
//! props, copying (to a temporary name, then renamed into place) and
//! moving, each checking first that the target hasn't changed since update
//! detection, and updating the archives last. Their parts on a root are
//! root commands, with OCaml's names and encodings. The filesystem work
//! runs off the engine thread, a host's one piece at a time (see
//! [`exclusive`]), as OCaml does it synchronously.
//!
//! Copying a directory copies its children one at a time, in name order,
//! which is the order OCaml's threads run in between local roots; running
//! transfers concurrently (`maxthreads`) comes with transport. Extended
//! data (xattrs and ACLs) isn't read or written yet, so the props pass
//! through as they are.

use std::future::Future;
use std::pin::Pin;

use gemel_base::case::{self, Mode};
use gemel_base::name::Name;
use gemel_base::path::{Global, Local, Path};
use gemel_base::prefs::{Category, Internal, PrefDef, Prefs, Topic};
use gemel_base::util::{Error, FileId};
use gemel_compat::umarshal::{self, Reader, Writer};
use gemel_fs::fileinfo::{self, SetAction, Typ};
use gemel_fs::fs;
use gemel_fs::fspath::Fspath;
use gemel_fs::os;
use gemel_fs::props::{Props, PropsConfig};
use gemel_fs::system;
use gemel_rpc::host::Host;
use gemel_rpc::registry::{HostCmd, RootCmd, Rpc};
use gemel_rpc::root::Root;

use crate::archive::{self, Archive};
use crate::common::{Codec, ContentsChange, UpdateContent, UpdateItem};
use crate::copy::{self, Ctx, Replacing, Source};
use crate::globals::Roots;
use crate::stasher::{self, Disposition};
use crate::update::{self, TRANSLATE_PATH};
use crate::with_trace;
use crate::{abort, exclusive, xferhint};

fn internal(what: &str) -> Error {
    Error::Fatal(format!("Internal error: files: {what}"))
}

fn path_str<S>(p: &Path<S>) -> String {
    String::from_utf8_lossy(p.as_bytes()).into_owned()
}

/// The props codec for these features.
fn props_config(f: &gemel_base::features::Features) -> PropsConfig {
    PropsConfig::default().with_features(f)
}

// ---- The commit log ----

/// `commitLogName`: in the Unison directory.
fn commit_log_name(host: &Host) -> Vec<u8> {
    host.file_in_unison_dir("DANGER.README")
}

/// `writeCommitLog`: says what is being moved, in case the run stops
/// halfway (a fatal error if it can't).
fn write_commit_log(log: &[u8], source: &Fspath, target: &Fspath, temp: &str) -> Result<(), Error> {
    use std::io::Write as _;
    let what = "writing commit log";
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut c = opts
        .open(system::os_path(log))
        .map_err(|e| system::SysError::new("open", log, e).to_fatal(what))?;
    let text = format!(
        "Warning: the last run of {} terminated abnormally while moving\n   {source}\nto\n   \
         {target}\nvia\n   {temp}\n\nPlease check the state of these files immediately\n(and \
         delete this notice when you've done so).\n",
        archive::MY_NAME
    );
    c.write_all(text.as_bytes())
        .map_err(|e| system::SysError::new("write", log, e).to_fatal(what))
}

/// `clearCommitLog`. (On Windows, where unlinking may not be immediate,
/// it is renamed out of the way first.)
fn clear_commit_log(log: &[u8], tmp_name: &Path<Local>) -> Result<(), Error> {
    let unlink_path = if cfg!(windows) {
        let base = gemel_base::util::filename_basename(&path_str(tmp_name));
        let p = [log, base.as_bytes()].concat();
        let mut n = 0;
        loop {
            let candidate = [&p[..], n.to_string().as_bytes()].concat();
            if system::file_exists(&candidate) {
                n += 1;
                continue;
            }
            break match system::rename(log, &candidate) {
                Ok(()) => candidate,
                Err(_) => log.to_vec(),
            };
        }
    } else {
        log.to_vec()
    };
    system::unlink(&unlink_path).map_err(|e| e.to_fatal("clearing commit log"))
}

/// `processCommitLog`: fails if a run stopped in the middle of a rename.
fn process_commit_log(host: &Host) -> Result<(), Error> {
    let log = commit_log_name(host);
    if system::file_exists(&log) {
        return Err(Error::Fatal(format!(
            "Warning: the previous run of {} terminated in a dangerous state.\n            \
             Please consult the file {}, delete it, and try again.",
            archive::MY_NAME,
            String::from_utf8_lossy(&log)
        )));
    }
    Ok(())
}

/// `processCommitLogOnHost`.
pub static PROCESS_COMMIT_LOG: HostCmd<(), ()> = HostCmd::new(
    "processCommitLog",
    |(), _, _| {},
    |_, _| Ok(()),
    |(), _, _| {},
    |_, _| Ok(()),
    |host, ()| Box::pin(async move { process_commit_log(&host) }),
);

/// `Files.processCommitLogs`: on every root's host (so twice on a host
/// with both).
pub async fn process_commit_logs(rpc: &Rpc, roots: &Roots) -> Result<(), Error> {
    for r in roots.list() {
        rpc.call_host(&PROCESS_COMMIT_LOG, &r.host, ()).await?;
    }
    Ok(())
}

// ---- Preferences ----

/// `copyonconflict`.
pub static COPY_ON_CONFLICT: PrefDef<bool> = PrefDef::new(
    "copyonconflict",
    Category::Advanced(Topic::Syncprocess),
    || false,
    "keep copies of conflicting files",
    "When this flag is set, Unison will make a copy of files that would \
     otherwise be overwritten or deleted in case of conflicting changes, \
     and more generally whenever the default behavior is overridden. \
     This makes it possible to automatically resolve conflicts in a \
     fairly safe way when synchronizing continuously, in combination \
     with the \\verb|-repeat watch| and \\verb|-prefer newer| preferences.",
);

/// `diff`.
pub static DIFF_CMD: PrefDef<String> = PrefDef::new(
    "diff",
    Category::Advanced(Topic::General),
    || "diff -u OLDER NEWER".to_owned(),
    "set command for showing differences between files",
    "This preference can be used to control the name and command-line \
     arguments of the system \
     utility used to generate displays of file differences.  The default \
     is `\\verb|diff -u OLDER NEWER|'.  If the value of this preference contains the substrings \
     CURRENT1 and CURRENT2, these will be replaced by the names of the files to be \
     diffed.  If the value of this preference contains the substrings \
     NEWER and OLDER, these will be replaced by the names of files to be \
     diffed, NEWER being the most recently modified file of the two.  \
     Without any of these substrings, the two filenames will be appended to the command.  In all \
     cases, the filenames are suitably quoted.",
);

/// `keeptempfilesaftermerge`.
pub static KEEP_TEMP_FILES_AFTER_MERGE: PrefDef<bool> = PrefDef::new(
    "keeptempfilesaftermerge",
    Category::Internal(Internal::Devel),
    || false,
    "*",
    "",
);

/// Registers this module's preferences, in OCaml's order.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&COPY_ON_CONFLICT)?;
    prefs.register(&DIFF_CMD)?;
    prefs.register(&KEEP_TEMP_FILES_AFTER_MERGE)
}

// ---- Conflict copies (copyonconflict) ----

/// What `prepareCopy` saved: the directory, the path, and the copy (none
/// if there was nothing there).
type CopyInfo = (Fspath, Path<Local>, Option<Path<Local>>);

/// `prepareCopy`: with `copyonconflict`, when overriding the default,
/// copies what is at `path` aside.
fn prepare_copy(
    ctx: &Ctx,
    copy_on_conflict: bool,
    working_dir: &Fspath,
    path: &Path<Local>,
    not_default: bool,
) -> Result<Option<CopyInfo>, Error> {
    if !(not_default && copy_on_conflict) {
        return Ok(None);
    }
    let follow = |p: &Path<Local>| ctx.follow.test(p.as_bytes());
    match fileinfo::get_type(true, working_dir, path, &follow, &ctx.fs)? {
        Typ::Absent => Ok(Some((working_dir.clone(), path.clone(), None))),
        _ => {
            let tmp = os::temp_path(true, working_dir, path, &ctx.temp_names, &ctx.fs)?;
            copy::recursively(ctx, working_dir, path, working_dir, &tmp)?;
            Ok(Some((working_dir.clone(), path.clone(), Some(tmp))))
        }
    }
}

/// `finishCopy`: puts the copy aside under a conflict name (`name
/// (conflict on <date>)`), or, if there was nothing, returns the name for
/// what replaces it (`… _was_deleted)`).
fn finish_copy(ctx: &Ctx, info: Option<CopyInfo>) -> Result<Option<Path<Local>>, Error> {
    let Some((working_dir, path, tmp)) = info else {
        return Ok(None);
    };
    let tm = system::localtime(system::gettimeofday()).ok_or_else(|| internal("no local time"))?;
    let mut n = 0;
    let conflict_path = loop {
        let p = path.add_to_final_name(
            format!(
                " (conflict{}_on_{:04}-{:02}-{:02}{})",
                if n == 0 {
                    String::new()
                } else {
                    format!(" #{n}")
                },
                tm.year,
                tm.month,
                tm.day,
                if tmp.is_none() { "_was_deleted" } else { "" }
            )
            .as_bytes(),
        );
        if !os::exists(&working_dir, &p, &ctx.fs)? {
            break p;
        }
        n += 1;
    };
    match tmp {
        Some(tmp) => {
            os::rename(
                "keepCopy",
                &working_dir,
                &tmp,
                &working_dir,
                &conflict_path,
                None,
                &ctx.fs,
            )?;
            Ok(None)
        }
        None => Ok(Some(conflict_path)),
    }
}

/// The settings of a host's file operations.
struct Settings {
    ctx: Ctx,
    copy_on_conflict: bool,
    mode: Mode,
}

impl Settings {
    fn of(host: &Host) -> Result<Settings, Error> {
        let prefs = host.prefs().borrow();
        Ok(Settings {
            ctx: Ctx::of(host)?,
            copy_on_conflict: *prefs.read(&COPY_ON_CONFLICT),
            mode: case::mode(&prefs),
        })
    }
}

/// `prepareCopy` off the engine thread.
async fn prepare_copy_on(
    host: &Host,
    working_dir: &Fspath,
    path: &Path<Local>,
    not_default: bool,
) -> Result<Option<CopyInfo>, Error> {
    let s = Settings::of(host)?;
    let (wd, p) = (working_dir.clone(), path.clone());
    exclusive::blocking(host, move || {
        prepare_copy(&s.ctx, s.copy_on_conflict, &wd, &p, not_default)
    })
    .await
}

/// `finishCopy` off the engine thread.
async fn finish_copy_on(host: &Host, info: Option<CopyInfo>) -> Result<Option<Path<Local>>, Error> {
    if info.is_none() {
        return Ok(None);
    }
    let ctx = Ctx::of(host)?;
    exclusive::blocking(host, move || finish_copy(&ctx, info)).await
}

// ---- Deleting ----

/// `deleteLocal`: deletes `path` (after checking it is as update detection
/// saw it, and backing it up), then updates the archive.
async fn delete_local(
    host: &Host,
    fspath: &Fspath,
    (path, ui, not_default): (Path<Global>, UpdateItem, bool),
) -> Result<(), Error> {
    let local = update::translate_path_local(host, fspath, &path)?;
    let copy_info = prepare_copy_on(host, fspath, &local, not_default).await?;
    // The target must be unchanged (there is an unavoidable race here).
    let prev_arch = update::check_no_updates(host, fspath, &local, &ui, false).await?;
    finish_copy_on(host, copy_info).await?;
    stasher::backup(host, fspath, &local, Disposition::AndRemove, &prev_arch).await?;
    // The archive last.
    update::replace_archive_local(host, fspath, &local, Archive::NoArchive)
}

fn write_ui(ui: &UpdateItem, w: &mut Writer, f: &gemel_base::features::Features) {
    ui.marshal(w, &Codec::new(f));
}

fn read_ui(r: &mut Reader<'_>, f: &gemel_base::features::Features) -> umarshal::Result<UpdateItem> {
    UpdateItem::unmarshal(r, &Codec::new(f))
}

/// `deleteOnRoot`.
pub static DELETE: RootCmd<(Path<Global>, UpdateItem, bool), ()> = RootCmd::new(
    "delete",
    |(p, ui, d), w, f| {
        p.marshal(w);
        write_ui(ui, w, f);
        w.bool(*d);
    },
    |r, f| Ok((Path::unmarshal(r)?, read_ui(r, f)?, r.bool()?)),
    |(), _, _| {},
    |_, _| Ok(()),
    |host, fspath, args| Box::pin(async move { delete_local(&host, &fspath, args).await }),
);

/// `Files.delete`: deletes `path_to` on `root_to`, and records the path
/// as gone on `root_from` too.
pub async fn delete(
    rpc: &Rpc,
    root_from: &Root,
    path_from: &Path<Global>,
    root_to: &Root,
    path_to: &Path<Global>,
    ui: &UpdateItem,
    not_default: bool,
) -> Result<(), Error> {
    rpc.call_root(&DELETE, root_to, (path_to.clone(), ui.clone(), not_default))
        .await?;
    update::replace_archive(rpc, root_from, path_from.clone(), Archive::NoArchive).await
}

// ---- Props ----

/// `fileUpdated`: the contents changed.
fn file_updated(ui: &UpdateItem) -> bool {
    matches!(
        ui,
        UpdateItem::Updates(UpdateContent::File(_, ContentsChange::Updated(..)), _)
    )
}

/// `setPropLocal`: sets the props (`ui` gives the modification time,
/// `new_desc` the rest), stashes, then updates the archive.
async fn set_prop_local(
    host: &Host,
    fspath: &Fspath,
    (path, ui, new_desc, old_desc): (Path<Global>, UpdateItem, Props, Props),
) -> Result<(), Error> {
    let local = update::translate_path_local(host, fspath, &path)?;
    let ctx = Ctx::of(host)?;
    {
        let (fspath, local, new_desc) = (fspath.clone(), local.clone(), new_desc.clone());
        exclusive::blocking(host, move || {
            let (working_dir, real) =
                fspath.find_working_dir(&local, ctx.follow.test(local.as_bytes()))?;
            fileinfo::set(
                &working_dir,
                &real,
                SetAction::Update(&old_desc),
                &new_desc,
                &ctx.fs,
            )
        })
        .await?;
    }
    if file_updated(&ui) {
        stasher::stash_current_version(host, fspath, &local, None).await?;
    }
    // The archive last.
    update::update_props(host, fspath, &local, Some(&new_desc), &ui)
}

/// `setPropOnRoot`.
pub static SET_PROP: RootCmd<(Path<Global>, UpdateItem, Props, Props), ()> = RootCmd::new(
    "setProp",
    |(p, ui, new, old), w, f| {
        p.marshal(w);
        write_ui(ui, w, f);
        new.marshal(w, &props_config(f));
        old.marshal(w, &props_config(f));
    },
    |r, f| {
        Ok((
            Path::unmarshal(r)?,
            read_ui(r, f)?,
            Props::unmarshal(r, &props_config(f))?,
            Props::unmarshal(r, &props_config(f))?,
        ))
    },
    |(), _, _| {},
    |_, _| Ok(()),
    |host, fspath, args| Box::pin(async move { set_prop_local(&host, &fspath, args).await }),
);

/// `updatePropsOnRoot`: records the props after propagation (the archive
/// first, then the stash). Older versions send a global path; with the
/// xattr feature it is the local one.
pub static UPDATE_PROPS: RootCmd<(Path<Local>, Option<Props>, UpdateItem), ()> = RootCmd::new(
    "updateProps",
    |(p, props, ui), w, f| {
        p.marshal(w);
        w.option(props.as_ref(), |w, d| d.marshal(w, &props_config(f)));
        write_ui(ui, w, f);
    },
    |r, f| {
        Ok((
            Path::unmarshal(r)?,
            r.option(|r| Props::unmarshal(r, &props_config(f)))?,
            read_ui(r, f)?,
        ))
    },
    |(), _, _| {},
    |_, _| Ok(()),
    |host, fspath, (path, props, ui)| {
        Box::pin(async move {
            let xattr = host
                .features()
                .borrow()
                .enabled_by_name(gemel_fs::props::FEATURE_XATTR);
            let local = if xattr {
                path
            } else {
                update::translate_path_local(&host, &fspath, &path.make_global())?
            };
            update::update_props(&host, &fspath, &local, props.as_ref(), &ui)?;
            if file_updated(&ui) {
                stasher::stash_current_version(&host, &fspath, &local, None).await?;
            }
            Ok(())
        })
    },
);

/// `Files.updateProps`.
pub async fn update_props(
    rpc: &Rpc,
    root: &Root,
    path: &Path<Local>,
    props: Option<&Props>,
    ui: &UpdateItem,
) -> Result<(), Error> {
    rpc.call_root(
        &UPDATE_PROPS,
        root,
        (path.clone(), props.cloned(), ui.clone()),
    )
    .await
}

/// `Files.setProp`: sets the props `new_desc` (which were `old_desc`) at
/// `path_to`, then records them on both roots.
#[allow(clippy::too_many_arguments)]
pub async fn set_prop(
    rpc: &Rpc,
    root_from: &Root,
    path_from: &Path<Global>,
    root_to: &Root,
    path_to: &Path<Global>,
    new_desc: &Props,
    old_desc: &Props,
    ui_from: &UpdateItem,
    ui_to: &UpdateItem,
) -> Result<(), Error> {
    rpc.call_root(
        &SET_PROP,
        root_to,
        (
            path_to.clone(),
            ui_to.clone(),
            new_desc.clone(),
            old_desc.clone(),
        ),
    )
    .await?;
    let local_from = rpc
        .call_root(&TRANSLATE_PATH, root_from, path_from.clone())
        .await?;
    update_props(rpc, root_from, &local_from, None, ui_from).await
}

// ---- Directories and links ----

/// `mkdirOnRoot`: makes sure there is a directory at `path` (writable,
/// unless `dontchmod`), replacing anything else; says whether it was there
/// already, and its props.
pub static MKDIR: RootCmd<(Fspath, Path<Local>), (bool, Props)> = RootCmd::new(
    "mkdir",
    |(f, p), w, _| {
        f.marshal(w);
        p.marshal(w);
    },
    |r, _| Ok((Fspath::unmarshal(r)?, Path::unmarshal(r)?)),
    |(b, d), w, f| {
        w.bool(*b);
        d.marshal(w, &props_config(f));
    },
    |r, f| Ok((r.bool()?, Props::unmarshal(r, &props_config(f))?)),
    |host, _, (working_dir, path)| {
        Box::pin(async move {
            let ctx = Ctx::of(&host)?;
            exclusive::blocking(&host, move || {
                let info = fileinfo::get_basic(false, &working_dir, &path, &|_| false, &ctx.fs)?;
                if info.typ == Typ::Directory {
                    if !ctx.fs.dont_chmod {
                        let _ = fs::chmod(&working_dir.concat(&path), info.desc.perm.bits | 0o700);
                    }
                    return Ok((true, info.desc));
                }
                if info.typ != Typ::Absent {
                    os::delete(&working_dir, &path, &ctx.fs)?;
                }
                os::create_dir(&working_dir, &path, Props::dir_default().perm.bits as u32)?;
                let desc =
                    fileinfo::get_basic(false, &working_dir, &path, &|_| false, &ctx.fs)?.desc;
                Ok((false, desc))
            })
            .await
        })
    },
);

/// `setDirPropOnRoot`: gives a copied directory its props, over the ones
/// it was created with.
pub static SET_DIR_PROP: RootCmd<(Fspath, Path<Local>, Props, Props), ()> = RootCmd::new(
    "setDirProp",
    |(fsp, p, initial, new), w, f| {
        fsp.marshal(w);
        p.marshal(w);
        initial.marshal(w, &props_config(f));
        new.marshal(w, &props_config(f));
    },
    |r, f| {
        Ok((
            Fspath::unmarshal(r)?,
            Path::unmarshal(r)?,
            Props::unmarshal(r, &props_config(f))?,
            Props::unmarshal(r, &props_config(f))?,
        ))
    },
    |(), _, _| {},
    |_, _| Ok(()),
    |host, _, (working_dir, path, initial, new)| {
        Box::pin(async move {
            let ctx = Ctx::of(&host)?;
            fileinfo::set(&working_dir, &path, SetAction::Set(&initial), &new, &ctx.fs)
        })
    },
);

/// `makeSymlink`: replaces what is at `path` with a link. (On Windows,
/// OCaml changes the working directory around this so that a relative
/// target resolves, to tell file and directory links apart; `os::symlink`
/// resolves it against the link's directory instead, as the working
/// directory is the whole process's.)
pub static MAKE_SYMLINK: RootCmd<(Fspath, Path<Local>, Vec<u8>), ()> = RootCmd::new(
    "makeSymlink",
    |(f, p, l), w, _| {
        f.marshal(w);
        p.marshal(w);
        w.string(l);
    },
    |r, _| Ok((Fspath::unmarshal(r)?, Path::unmarshal(r)?, r.string()?)),
    |(), _, _| {},
    |_, _| Ok(()),
    |host, _, (working_dir, path, link)| {
        Box::pin(async move {
            let ctx = Ctx::of(&host)?;
            exclusive::blocking(&host, move || {
                if os::exists(&working_dir, &path, &ctx.fs)? {
                    os::delete(&working_dir, &path, &ctx.fs)?;
                }
                os::symlink(&working_dir, &path, &link)
            })
            .await
        })
    },
);

// ---- Renaming ----

/// How [`perform_rename`] went: whether the target had to be moved aside
/// first, and the temporary name it was moved to.
struct RenamePlan {
    move_first: bool,
    temp: Option<Path<Local>>,
}

/// `performRename`: renames `from` onto `to` (backing up what is there).
/// When the target is a directory (or the source is), it is moved aside
/// first, under a commit log, so that a run stopped halfway can be noticed.
/// `exdev` makes a rename across filesystems fail (moves don't copy).
#[allow(clippy::too_many_arguments)]
async fn perform_rename(
    host: &Host,
    fspath_to: &Fspath,
    local_path_to: &Path<Local>,
    (working_dir_from, path_from): (&Fspath, &Path<Local>),
    (working_dir_to, path_to): (&Fspath, &Path<Local>),
    exdev: bool,
    prev_arch: &Archive,
) -> Result<(), Error> {
    let ctx = Ctx::of(host)?;
    let source = working_dir_from.concat(path_from);
    let target = working_dir_to.concat(path_to);
    let plan = {
        let (ctx, source, target) = (ctx.clone(), source.clone(), target.clone());
        let (wd_to, p_to) = (working_dir_to.clone(), path_to.clone());
        exclusive::blocking(host, move || -> Result<RenamePlan, Error> {
            let typ_from = fileinfo::get_type(false, &source, &Path::empty(), &|_| false, &ctx.fs)?;
            if typ_from == Typ::Absent {
                return Err(Error::Transient(format!(
                    "Error while renaming {source} to {target} -- source file has disappeared!"
                )));
            }
            let typ_to = fileinfo::get_type(false, &target, &Path::empty(), &|_| false, &ctx.fs)?;
            let move_first = match (typ_from, typ_to) {
                (_, Typ::Absent) => false,
                (Typ::File | Typ::Symlink, Typ::File | Typ::Symlink) => false,
                // The safe default.
                _ => true,
            };
            let temp = if move_first {
                Some(os::temp_path(
                    true,
                    &wd_to,
                    &p_to,
                    &ctx.temp_names,
                    &ctx.fs,
                )?)
            } else {
                None
            };
            Ok(RenamePlan { move_first, temp })
        })
        .await?
    };
    stasher::backup(
        host,
        fspath_to,
        local_path_to,
        Disposition::ByCopying,
        prev_arch,
    )
    .await?;
    let log = commit_log_name(host);
    let wd_to = working_dir_to.clone();
    exclusive::blocking(host, move || -> Result<(), Error> {
        let mut no_exdev = || Err(Error::Transient("EXDEV".to_owned()));
        let exdev_handler: Option<&mut dyn FnMut() -> Result<(), Error>> =
            if exdev { Some(&mut no_exdev) } else { None };
        let empty = Path::<Local>::empty();
        if !plan.move_first {
            return os::rename(
                "renameLocal(3)",
                &source,
                &empty,
                &target,
                &empty,
                exdev_handler,
                &ctx.fs,
            );
        }
        let temp_path = plan.temp.expect("planned");
        let temp = wd_to.concat(&temp_path);
        write_commit_log(&log, &source, &target, &temp.to_string())?;
        // Util.finalize: the log is cleared after a success or a transient
        // error (the first rename failing leaves things consistent), but
        // not after a fatal one (the second rename failing leaves the log,
        // and the next run stops on it).
        let moved = os::rename(
            "renameLocal(1)",
            &target,
            &empty,
            &temp,
            &empty,
            None,
            &ctx.fs,
        )
        .and_then(|()| {
            os::rename(
                "renameLocal(2)",
                &source,
                &empty,
                &target,
                &empty,
                exdev_handler,
                &ctx.fs,
            )
            .map_err(|e| match e {
                Error::Transient(m) => Error::Fatal(m),
                e => e,
            })
        });
        match moved {
            Ok(()) => clear_commit_log(&log, &temp_path)?,
            Err(e @ Error::Transient(_)) => {
                clear_commit_log(&log, &temp_path)?;
                return Err(e);
            }
            Err(e) => return Err(e),
        }
        // A temporary file left behind is harmless, so this comes after.
        os::delete(&temp, &empty, &ctx.fs)
    })
    .await
}

/// What [`rename_local`] gets.
pub type RenameArgs = (
    (Path<Local>, Fspath, Path<Local>, Path<Local>),
    (UpdateItem, Option<Archive>, bool),
);

/// `renameLocal`: checks the target is as update detection saw it, then
/// renames the temporary copy into place; with an archive, records it
/// (after stashing, and recording the copy hints).
async fn rename_local(
    host: &Host,
    fspath_to: &Fspath,
    ((local_path_to, working_dir, path_from, path_to), (ui, arch, not_default)): RenameArgs,
) -> Result<(), Error> {
    let copy_info = prepare_copy_on(host, &working_dir, &path_to, not_default).await?;
    // The target must be unchanged (there is an unavoidable race here).
    let prev_arch = update::check_no_updates(host, fspath_to, &local_path_to, &ui, false).await?;
    // A conflict copy when the file was changed in one replica and deleted
    // in the other.
    let path_to = finish_copy_on(host, copy_info).await?.unwrap_or(path_to);
    perform_rename(
        host,
        fspath_to,
        &local_path_to,
        (&working_dir, &path_from),
        (&working_dir, &path_to),
        false,
        &prev_arch,
    )
    .await?;
    if let Some(arch_to) = arch {
        stasher::stash_current_version(host, fspath_to, &local_path_to, None).await?;
        update::iter_files(fspath_to, &local_path_to, &arch_to, &mut |f, p, fp| {
            xferhint::insert_entry(host, (f.clone(), p.clone(), fp.clone()));
        });
        // The archive last.
        update::replace_archive_local(host, fspath_to, &local_path_to, arch_to)?;
    }
    Ok(())
}

/// `renameOnHost`.
pub static RENAME: RootCmd<RenameArgs, ()> = RootCmd::new(
    "rename",
    |((l, wd, from, to), (ui, arch, d)), w, f| {
        l.marshal(w);
        wd.marshal(w);
        from.marshal(w);
        to.marshal(w);
        write_ui(ui, w, f);
        w.option(arch.as_ref(), |w, a| a.marshal(w, &Codec::new(f).props));
        w.bool(*d);
    },
    |r, f| {
        Ok((
            (
                Path::unmarshal(r)?,
                Fspath::unmarshal(r)?,
                Path::unmarshal(r)?,
                Path::unmarshal(r)?,
            ),
            (
                read_ui(r, f)?,
                r.option(|r| Archive::unmarshal(r, &Codec::new(f).props, Mode::Sensitive))?,
                r.bool()?,
            ),
        ))
    },
    |(), _, _| {},
    |_, _| Ok(()),
    |host, fspath, ((l, wd, from, to), (ui, arch, d))| {
        Box::pin(async move {
            let mode = case::mode(&host.prefs().borrow());
            let arch = arch.map(|a| a.with_mode(mode));
            rename_local(&host, &fspath, ((l, wd, from, to), (ui, arch, d))).await
        })
    },
);

/// `Files.rename`.
#[allow(clippy::too_many_arguments)]
pub async fn rename(
    rpc: &Rpc,
    root: &Root,
    local_path: &Path<Local>,
    working_dir: &Fspath,
    path_old: &Path<Local>,
    path_new: &Path<Local>,
    ui: &UpdateItem,
    arch: Option<Archive>,
    not_default: bool,
) -> Result<(), Error> {
    rpc.call_root(
        &RENAME,
        root,
        (
            (
                local_path.clone(),
                working_dir.clone(),
                path_old.clone(),
                path_new.clone(),
            ),
            (ui.clone(), arch, not_default),
        ),
    )
    .await
}

// ---- Target paths ----

/// Where a copy goes: the directory the file will live in (a link's
/// target's, if the path is a link), its name there, a temporary name next
/// to it, and the path as the archive spells it.
pub type TargetPaths = (Fspath, Path<Local>, Path<Local>, Path<Local>);

fn write_target_paths((wd, real, temp, local): &TargetPaths, w: &mut Writer) {
    wd.marshal(w);
    real.marshal(w);
    temp.marshal(w);
    local.marshal(w);
}

fn read_target_paths(r: &mut Reader<'_>) -> umarshal::Result<TargetPaths> {
    Ok((
        Fspath::unmarshal(r)?,
        Path::unmarshal(r)?,
        Path::unmarshal(r)?,
        Path::unmarshal(r)?,
    ))
}

/// `setupTargetPathsLocal`.
fn setup_target_paths_local(
    host: &Host,
    fspath: &Fspath,
    path: &Path<Global>,
) -> Result<TargetPaths, Error> {
    let ctx = Ctx::of(host)?;
    let local = update::translate_path_local(host, fspath, path)?;
    let (working_dir, real) = fspath.find_working_dir(&local, ctx.follow.test(local.as_bytes()))?;
    let temp = os::temp_path(false, &working_dir, &real, &ctx.temp_names, &ctx.fs)?;
    Ok((working_dir, real, temp, local))
}

/// `setupTargetPaths`.
pub static SETUP_TARGET_PATHS: RootCmd<Path<Global>, TargetPaths> = RootCmd::new(
    "setupTargetPaths",
    |p, w, _| p.marshal(w),
    |r, _| Path::unmarshal(r),
    |t, w, _| write_target_paths(t, w),
    |r, _| read_target_paths(r),
    |host, fspath, path| Box::pin(async move { setup_target_paths_local(&host, &fspath, &path) }),
);

/// `createDirectories`: makes the missing parents of `path`, with these
/// props (the parent's first). One already there is left as it is.
fn create_directories(
    ctx: &Ctx,
    fspath: &Fspath,
    path: &Path<Local>,
    props: &[Props],
) -> Result<(), Error> {
    let Some((desc, rem)) = props.split_first() else {
        return Ok(());
    };
    let (_, parent) = path
        .deconstruct_rev()
        .ok_or_else(|| internal("parents' props for the root"))?;
    create_directories(ctx, fspath, &parent, rem)?;
    match fs::mkdir(
        &fspath.concat(&parent),
        Props::dir_default().perm.bits as u32,
    ) {
        Ok(()) => fileinfo::set(fspath, &parent, SetAction::Copy(&parent), desc, &ctx.fs),
        // Created already, for another path with the same parent.
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.to_transient("creating parent directories")),
    }
}

/// `setupTargetPathsAndCreateParentDirectoryLocal`.
async fn setup_target_paths_and_create_parent_directory_local(
    host: &Host,
    fspath: &Fspath,
    (path, props): (Path<Global>, Vec<Props>),
) -> Result<TargetPaths, Error> {
    let local = update::translate_path_local(host, fspath, &path)?;
    let ctx = Ctx::of(host)?;
    let (f, l) = (fspath.clone(), local.clone());
    exclusive::blocking(host, move || create_directories(&ctx, &f, &l, &props)).await?;
    setup_target_paths_local(host, fspath, &path)
}

/// `setupTargetPathsAndCreateParentDirectory`.
pub static SETUP_TARGET_PATHS_AND_CREATE_PARENT_DIRECTORY: RootCmd<
    (Path<Global>, Vec<Props>),
    TargetPaths,
> = RootCmd::new(
    "setupTargetPathsAndCreateParentDirectory",
    |(p, props), w, f| {
        p.marshal(w);
        w.list(props, |w, d| d.marshal(w, &props_config(f)));
    },
    |r, f| {
        Ok((
            Path::unmarshal(r)?,
            r.list(|r| Props::unmarshal(r, &props_config(f)))?,
        ))
    },
    |t, w, _| write_target_paths(t, w),
    |r, _| read_target_paths(r),
    |host, fspath, args| {
        Box::pin(async move {
            setup_target_paths_and_create_parent_directory_local(&host, &fspath, args).await
        })
    },
);

// ---- Copying ----

/// What `updateSourceArchive` gets: the source path, its updates, and the
/// paths whose copy failed.
pub type SourceArchiveArgs = (Path<Local>, UpdateItem, Vec<Path<Local>>);

/// `updateSourceArchiveLocal`: records on the source root what was copied
/// (the archive first, then the paths whose copy failed, then the stash).
async fn update_source_archive_local(
    host: &Host,
    fspath: &Fspath,
    (local_path, ui, err_paths): SourceArchiveArgs,
) -> Result<(), Error> {
    let new_arch = update::update_archive(host, fspath, &local_path, &ui)?;
    update::replace_archive_local(host, fspath, &local_path, new_arch)?;
    for p in &err_paths {
        update::replace_archive_local(host, fspath, p, Archive::NoArchive)?;
    }
    // A directory whose children failed: the next scan must look inside
    // even if fast checking says it is unchanged.
    if !err_paths.is_empty() {
        update::mark_possibly_updated(host, fspath, &local_path)?;
    }
    stasher::stash_current_version(host, fspath, &local_path, None).await
}

/// `updateSourceArchive`.
pub static UPDATE_SOURCE_ARCHIVE: RootCmd<SourceArchiveArgs, ()> = RootCmd::new(
    "updateSourceArchive",
    |(p, ui, errs), w, f| {
        p.marshal(w);
        write_ui(ui, w, f);
        w.list(errs, |w, p| p.marshal(w));
    },
    |r, f| {
        Ok((
            Path::unmarshal(r)?,
            read_ui(r, f)?,
            r.list(Path::unmarshal)?,
        ))
    },
    |(), _, _| {},
    |_, _| Ok(()),
    |host, fspath, args| {
        Box::pin(async move { update_source_archive_local(&host, &fspath, args).await })
    },
);

/// `deleteSpuriousChildrenRec`: deletes the children on disk that aren't
/// among `arch_children` (both sorted under the case mode).
fn delete_spurious_children(
    ctx: &Ctx,
    mode: Mode,
    fspath: &Fspath,
    path: &Path<Local>,
    arch_children: &[Name],
) -> Result<(), Error> {
    let mut children = os::children_of(fspath, path, &ctx.temp_names, &ctx.fs)?;
    children.sort_by(|a, b| a.compare_in(b, mode));
    let mut arch = arch_children.iter().peekable();
    for nm in &children {
        // Skip the archive names before this one.
        while arch.next_if(|a| a.compare_in(nm, mode).is_lt()).is_some() {}
        if arch.peek().is_some_and(|a| a.compare_in(nm, mode).is_eq()) {
            continue;
        }
        // FIX (OCaml's): maybe these should become temporary files?
        os::delete(fspath, &path.child(nm), &ctx.fs)?;
    }
    Ok(())
}

/// `deleteSpuriousChildren`.
pub static DELETE_SPURIOUS_CHILDREN: RootCmd<(Fspath, Path<Local>, Vec<Name>), ()> = RootCmd::new(
    "deleteSpuriousChildren",
    |(f, p, names), w, _| {
        f.marshal(w);
        p.marshal(w);
        w.list(names, |w, n| n.marshal(w));
    },
    |r, _| {
        Ok((
            Fspath::unmarshal(r)?,
            Path::unmarshal(r)?,
            r.list(Name::unmarshal)?,
        ))
    },
    |(), _, _| {},
    |_, _| Ok(()),
    |host, _, (fspath, path, names)| {
        Box::pin(async move {
            let s = Settings::of(&host)?;
            exclusive::blocking(&host, move || {
                delete_spurious_children(&s.ctx, s.mode, &fspath, &path, &names)
            })
            .await
        })
    },
);

/// `normalizeProps`: the source's parents' props for the target's
/// missing parents (dropping those the target has), reversed as OCaml
/// does.
fn normalize_props(props_from: &[Props], props_to: &[Props]) -> Result<Vec<Props>, Error> {
    if props_from.len() < props_to.len() {
        return Err(internal("more parents on the target than on the source"));
    }
    Ok(props_from[props_to.len()..].iter().rev().cloned().collect())
}

type CopyRec<'a> = Pin<Box<dyn Future<Output = Result<(Archive, Vec<Path<Local>>), Error>> + 'a>>;

/// One copy: what stays the same throughout the recursion.
struct Copying<'a> {
    rpc: &'a Rpc,
    root_from: &'a Root,
    root_to: &'a Root,
    working_dir: &'a Fspath,
    replacing: Replacing,
    id: FileId,
    mode: Mode,
    fast_check: bool,
    props: PropsConfig,
    fs: gemel_fs::fileinfo::FsConfig,
    max_errors: i64,
    /// The transient errors, most recent first.
    errors: Vec<Error>,
}

impl<'a> Copying<'a> {
    /// `Abort.check`.
    fn check_abort(&self) -> Result<(), Error> {
        self.rpc
            .client_host()
            .state::<abort::Abort>()
            .borrow()
            .check(self.id, self.max_errors)
    }

    /// `copyRec`: copies the archive `f` from `p_from` to `p_to` (to become
    /// `real_p_to`), returning the archive of what was copied and the paths
    /// that failed. A transient error fails the subtree only.
    fn copy_rec<'b>(
        &'b mut self,
        p_from: Path<Local>,
        p_to: Path<Local>,
        real_p_to: Path<Local>,
        f: &'b Archive,
    ) -> CopyRec<'b>
    where
        'a: 'b,
    {
        Box::pin(async move {
            match self.copy_one(&p_from, &p_to, &real_p_to, f).await {
                Ok(r) => Ok(r),
                Err(e @ Error::Transient(_)) => {
                    if !abort::is_abort(&e) {
                        self.rpc
                            .client_host()
                            .state::<abort::Abort>()
                            .borrow_mut()
                            .file(self.id);
                    }
                    self.errors.insert(0, e);
                    Ok((Archive::NoArchive, vec![p_from]))
                }
                Err(e) => Err(e),
            }
        })
    }

    async fn copy_one(
        &mut self,
        p_from: &Path<Local>,
        p_to: &Path<Local>,
        real_p_to: &Path<Local>,
        f: &Archive,
    ) -> Result<(Archive, Vec<Path<Local>>), Error> {
        match f {
            Archive::File(desc, fp, stamp, ress) => {
                self.check_abort()?;
                let source = Source {
                    desc: desc.clone(),
                    fp: fp.clone(),
                    stamp: self.fast_check.then_some(*stamp),
                    ress: ress.clone(),
                };
                let info = copy::file(
                    self.rpc,
                    self.root_from,
                    p_from,
                    self.root_to,
                    self.working_dir,
                    p_to,
                    real_p_to,
                    self.replacing,
                    &source,
                    self.id,
                )
                .await?;
                Ok((
                    Archive::File(
                        info.desc.override_with(desc, &self.props),
                        fp.clone(),
                        info.stamp(&self.fs),
                        info.ress_stamp(),
                    ),
                    Vec::new(),
                ))
            }
            Archive::Symlink(l) => {
                self.check_abort()?;
                self.rpc
                    .call_root(
                        &MAKE_SYMLINK,
                        self.root_to,
                        (self.working_dir.clone(), p_to.clone(), l.clone()),
                    )
                    .await?;
                Ok((f.clone(), Vec::new()))
            }
            Archive::Dir(desc, children) => {
                let (existing, initial_desc) = self
                    .rpc
                    .call_root(
                        &MKDIR,
                        self.root_to,
                        (self.working_dir.clone(), p_to.clone()),
                    )
                    .await?;
                self.check_abort()?;
                let mut new_children = crate::name_map::NameMap::new(self.mode);
                let mut paths = Vec::new();
                let mut child_error = false;
                for (name, child) in children.iter() {
                    let name_to = name.normalize(self.mode).into_owned();
                    let (arch, failed) = self
                        .copy_rec(
                            p_from.child(name),
                            p_to.child(&name_to),
                            real_p_to.child(&name_to),
                            child,
                        )
                        .await?;
                    if arch == Archive::NoArchive {
                        child_error = true;
                    } else {
                        new_children.add(name.clone(), arch);
                    }
                    paths.extend(failed);
                }
                if existing || child_error {
                    let names: Vec<Name> = new_children.iter().map(|(n, _)| n.clone()).collect();
                    self.rpc
                        .call_root(
                            &DELETE_SPURIOUS_CHILDREN,
                            self.root_to,
                            (self.working_dir.clone(), p_to.clone(), names),
                        )
                        .await?;
                }
                // The actual permissions, to keep inherited bits.
                self.rpc
                    .call_root(
                        &SET_DIR_PROP,
                        self.root_to,
                        (
                            self.working_dir.clone(),
                            p_to.clone(),
                            initial_desc,
                            desc.clone(),
                        ),
                    )
                    .await?;
                Ok((Archive::Dir(desc.clone(), new_children), paths))
            }
            Archive::NoArchive => Err(internal("copying nothing")),
        }
    }
}

/// `Files.copy`: copies `path_from` on `root_from` to `path_to` on
/// `root_to`, as `ui_from` says it is, to a temporary name, then renames
/// it into place (checking first that the target is as `ui_to` says) and
/// records it on both roots. What failed to copy is left out of the
/// archives, and the first such error is returned.
#[allow(clippy::too_many_arguments)]
pub async fn copy(
    rpc: &Rpc,
    roots: &Roots,
    replacing: Replacing,
    root_from: &Root,
    path_from: &Path<Global>,
    ui_from: &UpdateItem,
    props_from: &[Props],
    root_to: &Root,
    path_to: &Path<Global>,
    ui_to: &UpdateItem,
    props_to: &[Props],
    not_default: bool,
    id: FileId,
) -> Result<(), Error> {
    let local_path_from = rpc
        .call_root(&TRANSLATE_PATH, root_from, path_from.clone())
        .await?;
    let parent_props = normalize_props(props_from, props_to)?;
    let (working_dir, real_path_to, temp_path_to, local_path_to) = rpc
        .call_root(
            &SETUP_TARGET_PATHS_AND_CREATE_PARENT_DIRECTORY,
            root_to,
            (path_to.clone(), parent_props),
        )
        .await?;
    let client = rpc.client_host();
    let (mode, fast_check, max_errors, props, fs) = {
        let prefs = client.prefs().borrow();
        let mut fs = gemel_fs::fileinfo::FsConfig::from_prefs(&prefs);
        fs.props = fs.props.with_features(&client.features().borrow());
        (
            case::mode(&prefs),
            update::use_fast_checking(&prefs),
            *prefs.read(&abort::MAX_ERRORS),
            fs.props,
            fs,
        )
    };
    // New files get NFC names in Unicode case-insensitive mode.
    let real_path_to = match replacing {
        Replacing::Update(..) => real_path_to,
        Replacing::Copy => {
            let (name, parent) = real_path_to
                .deconstruct_rev()
                .ok_or_else(|| internal("copying onto the root's parent"))?;
            parent.child(&name.normalize(mode))
        }
    };
    // What to propagate, from the archive of the first root (the local one).
    let root_local = roots
        .in_canonical_order()
        .into_iter()
        .next()
        .ok_or_else(|| internal("no roots"))?;
    let local_arch = update::update_archive(
        rpc.local_host(),
        &root_local.fspath,
        &local_path_from,
        ui_from,
    )?;
    let mut c = Copying {
        rpc,
        root_from,
        root_to,
        working_dir: &working_dir,
        replacing,
        id,
        mode,
        fast_check,
        props,
        fs,
        max_errors,
        errors: Vec::new(),
    };
    let (arch_to, err_paths) = c
        .copy_rec(
            local_path_from.clone(),
            temp_path_to.clone(),
            real_path_to.clone(),
            &local_arch,
        )
        .await?;
    let errors = std::mem::take(&mut c.errors);
    if arch_to == Archive::NoArchive {
        // Nothing could be copied: the most recent error.
        return Err(errors
            .into_iter()
            .next()
            .unwrap_or_else(|| internal("a failed copy without an error")));
    }
    rename(
        rpc,
        root_to,
        &local_path_to,
        &working_dir,
        &temp_path_to,
        &real_path_to,
        ui_to,
        Some(arch_to),
        not_default,
    )
    .await?;
    rpc.call_root(
        &UPDATE_SOURCE_ARCHIVE,
        root_from,
        (local_path_from, ui_from.clone(), err_paths),
    )
    .await?;
    // The first error, if any.
    match errors.into_iter().last() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// ---- Moving ----

/// `moveArchiveLocal`: records a move on the root it was made on (only
/// the move; props are recorded separately), then stashes.
async fn move_archive_local(
    host: &Host,
    fspath: &Fspath,
    (path_from, path_to, reverting): (Path<Global>, Path<Global>, bool),
) -> Result<(), Error> {
    let local_from = update::translate_path_local(host, fspath, &path_from)?;
    let local_to = update::translate_path_local(host, fspath, &path_to)?;
    // The archive first, then the stash.
    if !reverting {
        let prev = update::update_archive(host, fspath, &local_from, &UpdateItem::NoUpdates)?;
        update::replace_archive_local(host, fspath, &local_to, prev)?;
    }
    update::replace_archive_local(host, fspath, &local_from, Archive::NoArchive)?;
    stasher::stash_current_version(host, fspath, &local_to, None).await
}

/// `moveArchiveOnRoot`.
pub static MOVE_ARCHIVE: RootCmd<(Path<Global>, Path<Global>, bool), ()> = RootCmd::new(
    "moveArchive",
    |(from, to, rev), w, _| {
        from.marshal(w);
        to.marshal(w);
        w.bool(*rev);
    },
    |r, _| Ok((Path::unmarshal(r)?, Path::unmarshal(r)?, r.bool()?)),
    |(), _, _| {},
    |_, _| Ok(()),
    |host, fspath, args| Box::pin(async move { move_archive_local(&host, &fspath, args).await }),
);

/// What [`move_local`] gets: the source and target paths with their
/// updates, then the target's parents' props, whether the move is being
/// reverted, and whether this isn't the default action.
pub type MoveArgs = (
    (Path<Global>, UpdateItem, Path<Global>, UpdateItem),
    (Vec<Props>, bool, bool),
);

/// `moveLocal`: checks the source and the target are as update detection
/// saw them, renames (never copying across filesystems), stashes, records
/// the copy hints, and updates the archive last.
async fn move_local(
    host: &Host,
    fspath: &Fspath,
    ((path_source, ui_source, path_target, ui_target), (props_target, reverting, not_default)): MoveArgs,
) -> Result<(), Error> {
    let (working_dir_to, real_path_to, _, local_path_to) =
        setup_target_paths_and_create_parent_directory_local(
            host,
            fspath,
            (path_target, props_target),
        )
        .await?;
    let local_path_from = update::translate_path_local(host, fspath, &path_source)?;
    let s = Settings::of(host)?;
    let (working_dir_from, real_path_from) = fspath.find_working_dir(
        &local_path_from,
        s.ctx.follow.test(local_path_from.as_bytes()),
    )?;
    // New names are NFC in Unicode case-insensitive mode.
    let (name, parent) = real_path_to
        .deconstruct_rev()
        .ok_or_else(|| internal("moving onto the root's parent"))?;
    let real_path_to = parent.child(&name.normalize(s.mode));
    let fast_check = update::use_fast_checking(&host.prefs().borrow());
    let cur_src_arch =
        update::check_no_updates(host, fspath, &local_path_from, &ui_source, fast_check).await?;
    let copy_info = prepare_copy_on(host, &working_dir_to, &real_path_to, not_default).await?;
    // The target must be unchanged (there is an unavoidable race here).
    let cur_tgt_arch =
        update::check_no_updates(host, fspath, &local_path_to, &ui_target, false).await?;
    let real_path_to = finish_copy_on(host, copy_info)
        .await?
        .unwrap_or(real_path_to);
    perform_rename(
        host,
        fspath,
        &local_path_to,
        (&working_dir_from, &real_path_from),
        (&working_dir_to, &real_path_to),
        true,
        &cur_tgt_arch,
    )
    .await?;
    stasher::stash_current_version(host, fspath, &local_path_to, None).await?;
    update::iter_files(fspath, &local_path_to, &cur_src_arch, &mut |f, p, fp| {
        xferhint::insert_entry(host, (f.clone(), p.clone(), fp.clone()));
    });
    // The archive last: the target gets the state of the last sync (the
    // only thing the rename did; props are set separately).
    if !reverting {
        let prev = update::update_archive(host, fspath, &local_path_from, &UpdateItem::NoUpdates)?;
        update::replace_archive_local(host, fspath, &local_path_to, prev)?;
    }
    update::replace_archive_local(host, fspath, &local_path_from, Archive::NoArchive)
}

/// `moveOnRoot`.
pub static MOVE: RootCmd<MoveArgs, ()> = RootCmd::new(
    "move",
    |((ps, uis, pt, uit), (props, rev, d)), w, f| {
        ps.marshal(w);
        write_ui(uis, w, f);
        pt.marshal(w);
        write_ui(uit, w, f);
        w.list(props, |w, p| p.marshal(w, &props_config(f)));
        w.bool(*rev);
        w.bool(*d);
    },
    |r, f| {
        Ok((
            (
                Path::unmarshal(r)?,
                read_ui(r, f)?,
                Path::unmarshal(r)?,
                read_ui(r, f)?,
            ),
            (
                r.list(|r| Props::unmarshal(r, &props_config(f)))?,
                r.bool()?,
                r.bool()?,
            ),
        ))
    },
    |(), _, _| {},
    |_, _| Ok(()),
    |host, fspath, args| Box::pin(async move { move_local(&host, &fspath, args).await }),
);

/// One side of a move for [`move_`]: the original path and its updates,
/// the moved (new) path and its updates, and the new parents' props.
#[derive(Debug, Clone)]
pub struct MoveSide<'a> {
    /// The original path.
    pub path: &'a Path<Global>,
    /// Its updates.
    pub ui: &'a UpdateItem,
    /// The path after the move.
    pub new_path: &'a Path<Global>,
    /// Its updates.
    pub new_ui: &'a UpdateItem,
    /// The props of its new parent directories.
    pub new_props: &'a [Props],
}

/// `Files.move`: repeats on `root_to` a move made on `root_from` (or
/// reverts one made on `root_to`, when the source is absent and new),
/// then records it on `root_from` and sets the moved path's props
/// (`new_desc`, over `old_desc`). A rename that would cross filesystems
/// fails with `Transient "EXDEV"`, for transport to copy instead.
#[allow(clippy::too_many_arguments)]
pub async fn move_(
    rpc: &Rpc,
    root_from: &Root,
    from: MoveSide<'_>,
    new_desc: &Props,
    root_to: &Root,
    to: MoveSide<'_>,
    old_desc: &Props,
    not_default: bool,
    size: gemel_fs::props::Filesize,
    id: FileId,
) -> Result<(), Error> {
    use UpdateContent::{Absent, Dir, File};
    use UpdateItem::{NoUpdates, Updates};
    let reverting = matches!(from.ui, Updates(Absent, crate::common::PrevState::New));
    // Crude checks: the source moved away entirely; a file or a directory
    // moved; when reverting, the original source was emptied.
    let sane = match (from.ui, to.ui, to.new_ui) {
        (Updates(Absent, _), NoUpdates | Updates(File(..) | Dir(..), _), _) if !reverting => true,
        (Updates(Absent, _), Updates(File(..) | Dir(..), _), Updates(Absent, _)) if reverting => {
            true
        }
        _ => false,
    };
    if !sane {
        return Err(Error::Transient(
            "Internal error. Please report this bug (keep\nthe details of what you were doing) \
             and (temporarily)\nturn off the \"moves\" preference to continue with\nupdate \
             propagation."
                .to_owned(),
        ));
    }
    rpc.call_root(&TRANSLATE_PATH, root_from, from.new_path.clone())
        .await?;
    let parent_props = normalize_props(from.new_props, to.new_props)?;
    rpc.call_root(
        &MOVE,
        root_to,
        (
            (
                to.path.clone(),
                to.ui.clone(),
                to.new_path.clone(),
                to.new_ui.clone(),
            ),
            (parent_props, reverting, not_default),
        ),
    )
    .await?;
    // No stopping or rolling back from here: the source archive is
    // updated, and props that fail to set are picked up next time.
    rpc.call_root(
        &MOVE_ARCHIVE,
        root_from,
        (from.path.clone(), from.new_path.clone(), reverting),
    )
    .await?;
    crate::uutil::show_progress(rpc.client_host(), id, size, "mov");
    // The props after the rename, which could change them. (Only on the
    // path itself: a moved directory's children wait for the next sync.)
    set_prop(
        rpc,
        root_from,
        from.new_path,
        root_to,
        to.new_path,
        new_desc,
        old_desc,
        from.new_ui,
        &UpdateItem::NoUpdates,
    )
    .await
}

// ---- Diff ----

/// `Fspath.quotes`.
fn quotes(f: &Fspath) -> String {
    gemel_base::util::quotes(&f.to_string())
}

/// `Files.diff`: runs the `diff` command on the two versions of a file and
/// shows the result (`show_diff`, with the command), if any. Only between
/// local roots so far.
#[allow(clippy::too_many_arguments)]
pub async fn diff(
    rpc: &Rpc,
    root1: &Root,
    path1: &Path<Global>,
    ui1: &UpdateItem,
    root2: &Root,
    path2: &Path<Global>,
    ui2: &UpdateItem,
    show_diff: &mut dyn FnMut(&str, &str),
    _id: FileId,
) -> Result<(), Error> {
    let ((desc1, _, _), (desc2, _, _)) = crate::common::file_infos(ui1, ui2)?;
    if !(root1.is_local() && root2.is_local()) {
        return Err(Error::Fatal(
            "Diffing files on remote hosts is not supported yet".to_owned(),
        ));
    }
    let host = rpc.local_host();
    let p1 = update::translate_path_local(host, &root1.fspath, path1)?;
    let p2 = update::translate_path_local(host, &root2.fspath, path2)?;
    let (f1, f2) = (root1.fspath.concat(&p1), root2.fspath.concat(&p2));
    let template = rpc.client_host().prefs().borrow().read(&DIFF_CMD).clone();
    let cmd = if template.contains("NEWER") {
        let (newer, older) = if desc1.time() > desc2.time() {
            (&f1, &f2)
        } else {
            (&f2, &f1)
        };
        template
            .replace("OLDER", &quotes(older))
            .replace("NEWER", &quotes(newer))
    } else if !template.contains("CURRENT1") {
        format!("{template} {} {}", quotes(&f1), quotes(&f2))
    } else {
        template
            .replace("CURRENT1", &quotes(&f1))
            .replace("CURRENT2", &quotes(&f2))
    };
    let (_, result) = crate::external::run_external_program(&cmd).await?;
    if !result.is_empty() {
        show_diff(&cmd, &result);
    }
    Ok(())
}

// ---- Listing ----

/// `Files.ls`: the entries of `dir` (`.` and `..` too) matching the glob
/// `pattern`, in byte order.
pub fn ls(dir: &str, pattern: &str) -> Result<Vec<String>, Error> {
    let what = "listing files";
    let re = gemel_base::rx::Rx::glob(pattern)
        .map_err(|e| Error::Transient(format!("Error in {what}:\n{e}")))?;
    let mut files: Vec<Vec<u8>> = system::read_dir(dir.as_bytes())
        .map_err(|e| e.to_transient(what))?
        .into_iter()
        .chain([b".".to_vec(), b"..".to_vec()])
        .collect();
    files.sort();
    Ok(files
        .into_iter()
        .filter(|f| re.match_string(f))
        .map(|f| String::from_utf8_lossy(&f).into_owned())
        .collect())
}

// ---- Merge ----

/// `formatMergeCmd`: the `merge` command for `p`, with the files filled
/// in (an archive version is needed for `CURRENTARCH`, not for
/// `CURRENTARCHOPT`).
#[allow(clippy::too_many_arguments)]
fn format_merge_cmd(
    prefs: &Prefs,
    p: &Path<Global>,
    f1: &str,
    f2: &str,
    backup: Option<&str>,
    out1: &str,
    out2: &str,
    outarch: &str,
    batchmode: &str,
) -> Result<String, Error> {
    let mode = case::mode(prefs);
    if !crate::globals::should_merge(prefs, mode, p)? {
        return Err(Error::Transient(format!(
            "'merge' preference not set for {}",
            path_str(p)
        )));
    }
    let Some(raw) = crate::globals::merge_cmd_for_path(prefs, mode, p)? else {
        return Err(Error::Transient(format!(
            "'merge' preference does not provide a command template for {}",
            path_str(p)
        )));
    };
    let cooked = raw.replace("CURRENT1", f1).replace("CURRENT2", f2);
    let cooked = match backup {
        None => {
            let cooked = cooked.replace("CURRENTARCHOPT", "");
            if cooked.contains("CURRENTARCH") {
                return Err(Error::Transient(
                    "No archive found, but the 'merge' command template expects one.  \
                     (Consider enabling 'backupcurrent' for this file or using CURRENTARCHOPT \
                     instead of CURRENTARCH.)"
                        .to_owned(),
                ));
            }
            cooked
        }
        Some(s) => cooked
            .replace("CURRENTARCHOPT", s)
            .replace("CURRENTARCH", s),
    };
    Ok(cooked
        .replace("NEW1", out1)
        .replace("NEW2", out2)
        .replace("NEWARCH", outarch)
        .replace("NEW", out1)
        .replace("BATCHMODE", batchmode)
        .replace("PATH", &gemel_base::util::quotes(&path_str(p))))
}

/// `copyBack`: copies a merge result to `path_to` on `root_to`, and
/// records it (with `arch_to`, if the merge settled the file).
#[allow(clippy::too_many_arguments)]
async fn copy_back(
    rpc: &Rpc,
    fspath_from: &Fspath,
    path_from: &Path<Local>,
    root_to: &Root,
    path_to: &Path<Global>,
    props_to: &Props,
    ui_to: &UpdateItem,
    arch_to: Option<Archive>,
    id: FileId,
) -> Result<(), Error> {
    let (working_dir, real_path_to, temp_path_to, local_path_to) = rpc
        .call_root(&SETUP_TARGET_PATHS, root_to, path_to.clone())
        .await?;
    let ctx = Ctx::of(rpc.local_host())?;
    let info = fileinfo::get_basic(false, fspath_from, path_from, &|_| false, &ctx.fs)?;
    let fp = os::fingerprint(
        fspath_from,
        path_from,
        info.typ.info_kind(),
        ctx.fs.props.rsrc,
    )?;
    let source = Source {
        desc: props_to.set_length(info.desc.length()),
        fp,
        stamp: None,
        ress: info.ress_stamp(),
    };
    copy::file(
        rpc,
        &Root::local(fspath_from.clone()),
        path_from,
        root_to,
        &working_dir,
        &temp_path_to,
        &real_path_to,
        Replacing::Copy,
        &source,
        id,
    )
    .await?;
    rename(
        rpc,
        root_to,
        &local_path_to,
        &working_dir,
        &temp_path_to,
        &real_path_to,
        ui_to,
        arch_to,
        false,
    )
    .await
}

/// The merge's working files, next to the file.
struct MergeFiles {
    working_dir: Fspath,
    working1: Path<Local>,
    working2: Path<Local>,
    working_arch: Path<Local>,
    new1: Path<Local>,
    new2: Path<Local>,
    new_arch: Path<Local>,
}

impl MergeFiles {
    fn abs(&self, p: &Path<Local>) -> Fspath {
        self.working_dir.concat(p)
    }

    fn exists(&self, p: &Path<Local>) -> bool {
        system::file_exists(self.abs(p).as_bytes())
    }

    fn delete(&self, ctx: &Ctx, p: &Path<Local>) -> Result<(), Error> {
        os::delete(&self.working_dir, p, &ctx.fs)
    }

    fn fingerprint(&self, ctx: &Ctx, p: &Path<Local>) -> Result<os::FullFingerprint, Error> {
        let typ = fileinfo::get_type(false, &self.working_dir, p, &|_| false, &ctx.fs)?;
        os::fingerprint(&self.working_dir, p, typ.info_kind(), ctx.fs.props.rsrc)
    }

    /// `merge`'s `copy`: copies each source over its target.
    fn copy(&self, ctx: &Ctx, pairs: &[(&Path<Local>, &Path<Local>)]) -> Result<(), Error> {
        for (src, trg) in pairs {
            self.delete(ctx, trg)?;
            let info = fileinfo::get(false, &self.working_dir, src, &|_| false, &ctx.fs)?;
            copy::Progress::none(|p| {
                copy::local_file(
                    ctx,
                    &self.working_dir,
                    src,
                    &self.working_dir,
                    trg,
                    trg,
                    Replacing::Copy,
                    &info.desc,
                    gemel_fs::osx::ress_length(&info.os_x.ress_info),
                    p,
                )
            })?;
        }
        Ok(())
    }

    /// Deletes them all (`merge`'s finaliser), ignoring transient errors.
    fn clean_up(&self, ctx: &Ctx) {
        for p in [
            &self.working1,
            &self.working2,
            &self.working_arch,
            &self.new1,
            &self.new2,
            &self.new_arch,
        ] {
            let _ = self.delete(ctx, p);
        }
    }
}

/// What the merge program's outputs say to keep (`merge`'s big `if`):
/// copies them into place among the working files.
fn settle_merge(
    ctx: &Ctx,
    m: &MergeFiles,
    status: crate::external::ProcessStatus,
    (fp1, fp2): (&os::FullFingerprint, &os::FullFingerprint),
    say: &mut dyn FnMut(&str),
) -> Result<(), Error> {
    let (w1, w2, wa) = (&m.working1, &m.working2, &m.working_arch);
    let (n1, n2) = (&m.new1, &m.new2);
    let ok = status.success();
    match (m.exists(n1), m.exists(n2), m.exists(&m.new_arch)) {
        (true, true, new_arch) => {
            say(if new_arch {
                "Three outputs detected \n"
            } else {
                "Two outputs detected \n"
            });
            if m.fingerprint(ctx, n1)? == m.fingerprint(ctx, n2)? {
                m.copy(ctx, &[(n1, w1), (n2, w2), (n1, wa)])
            } else if ok {
                // Not equal, but the program returned 0: the first output
                // wins, for the other replica and the archive too.
                say("Two outputs not equal but merge command returned 0, so we will\n");
                say("overwrite the other replica and the archive with the first output\n");
                m.copy(ctx, &[(n1, w1), (n1, w2), (n1, wa)])
            } else {
                // Not equal and a failure: both back, the archive as it was.
                say("Two outputs not equal and the merge command exited with nonzero status, \n");
                say("so we will copy back the new files but not update the archive\n");
                m.copy(ctx, &[(n1, w1), (n2, w2)])
            }
        }
        (true, false, false) => {
            if ok {
                say("One output detected \n");
                m.copy(ctx, &[(n1, w1), (n1, w2), (n1, wa)])
            } else {
                say("One output detected but merge command returned nonzero exit status\n");
                Err(Error::Transient(
                    "One output detected but merge command returned nonzero exit status\n"
                        .to_owned(),
                ))
            }
        }
        (false, true, false) => Err(internal("only the second merge output")),
        (false, false, false) => {
            say("No outputs detected \n");
            let (still1, still2) = (m.exists(w1), m.exists(w2));
            if still1 && still2 {
                say("No output from merge cmd and both original files are still present\n");
                let (fp1b, fp2b) = (m.fingerprint(ctx, w1)?, m.fingerprint(ctx, w2)?);
                if *fp1 == fp1b && *fp2 == fp2b {
                    return Err(Error::Transient(
                        "Merge program didn't change either temp file".to_owned(),
                    ));
                }
                if fp1b == fp2b {
                    say("Merge program made files equal\n");
                    m.copy(ctx, &[(w1, wa)])
                } else if *fp2 == fp2b {
                    say("Merge program changed just first input\n");
                    m.copy(ctx, &[(w1, w2), (w1, wa)])
                } else if *fp1 == fp1b {
                    say("Merge program changed just second input\n");
                    m.copy(ctx, &[(w2, w1), (w2, wa)])
                } else if !ok {
                    Err(Error::Transient(
                        "Error: the merge function changed both of its inputs but did not \
                         make them equal"
                            .to_owned(),
                    ))
                } else {
                    // Both changed differently, but a 0 means "morally
                    // equal": the second wins, so the file is in sync again.
                    say("Merge program changed both of its inputs in");
                    say("different ways, but returned zero.\n");
                    m.copy(ctx, &[(w2, w1), (w2, wa)])
                }
            } else if still1 && !still2 && ok {
                say("No outputs and second replica has been deleted \n");
                m.copy(ctx, &[(w1, w2), (w1, wa)])
            } else if !still1 && still2 && ok {
                say("No outputs and first replica has been deleted \n");
                m.copy(ctx, &[(w2, w1), (w2, wa)])
            } else if ok {
                Err(Error::Transient(
                    "Error: the merge program deleted both of its inputs and generated no \
                     output!"
                        .to_owned(),
                ))
            } else {
                say("The merge program exited with nonzero status and did not leave");
                say(" both files equal");
                Err(Error::Transient(
                    "Error: the merge program failed and did not leave both files equal".to_owned(),
                ))
            }
        }
        _ => Err(internal("an unexpected set of merge outputs")),
    }
}

/// Shows a merge's result (a title, and what the merge program printed)
/// and says whether to keep it; it may wait for the user to answer.
pub type ShowMerge<'a> = dyn FnMut(String, String) -> crate::transport::Task<'a, bool> + 'a;

/// `showStatus`: how the merge program ended, as the log says.
fn show_status(s: crate::external::ProcessStatus) -> String {
    use crate::external::ProcessStatus::{Exited, Signaled, Stopped};
    match s {
        Exited(i) => format!("exited ({i})"),
        Signaled(i) => format!("killed with signal {i}"),
        Stopped(i) => format!("stopped with signal {i}"),
    }
}

/// `Files.merge`: runs the `merge` program on the two versions of a file
/// (and the last synchronised one, if a backup of it is found), shows the
/// result (`show_merge` must agree, or the merge is cancelled), and copies
/// what it made back to both replicas. Only between local roots so far.
#[allow(clippy::too_many_arguments)]
pub async fn merge(
    rpc: &Rpc,
    root1: &Root,
    path1: &Path<Global>,
    ui1: &UpdateItem,
    root2: &Root,
    path2: &Path<Global>,
    ui2: &UpdateItem,
    id: FileId,
    show_merge: &mut ShowMerge<'_>,
) -> Result<(), Error> {
    if !(root1.is_local() && root2.is_local()) {
        return Err(Error::Fatal(
            "Merging files on remote hosts is not supported yet".to_owned(),
        ));
    }
    let host = rpc.local_host();
    let ctx = Ctx::of(host)?;
    let fspath1 = &root1.fspath;
    let local_path1 = update::translate_path_local(host, fspath1, path1)?;
    let (working_dir, basep) =
        fspath1.find_working_dir(&local_path1, ctx.follow.test(local_path1.as_bytes()))?;
    let temp =
        |s: &str| basep.add_prefix_to_final_name(format!("{}{s}", os::TEMP_FILE_PREFIX).as_bytes());
    let m = MergeFiles {
        working1: temp("merge1-"),
        working2: temp("merge2-"),
        working_arch: temp("mergearch-"),
        new1: temp("mergenew1-"),
        new2: temp("mergenew2-"),
        new_arch: temp("mergenewarch-"),
        working_dir,
    };
    let ((desc1, fp1, ress1), (desc2, fp2, ress2)) = crate::common::file_infos(ui1, ui2)?;
    let r = merge_in(
        rpc,
        &ctx,
        &m,
        (root1, path1, ui1, &local_path1, &basep),
        (root2, path2, ui2),
        ((desc1, fp1, ress1), (desc2, fp2, ress2)),
        id,
        show_merge,
    )
    .await;
    // Util.finalize: cleaned up after a success or a transient error.
    if matches!(r, Ok(()) | Err(Error::Transient(_)))
        && !*rpc
            .client_host()
            .prefs()
            .borrow()
            .read(&KEEP_TEMP_FILES_AFTER_MERGE)
    {
        m.clean_up(&ctx);
    }
    r
}

type Merged<'a> = (
    &'a Root,
    &'a Path<Global>,
    &'a UpdateItem,
    &'a Path<Local>,
    &'a Path<Local>,
);
type FileInfos = (crate::common::FileInfo, crate::common::FileInfo);

#[allow(clippy::too_many_arguments)]
async fn merge_in(
    rpc: &Rpc,
    ctx: &Ctx,
    m: &MergeFiles,
    (root1, path1, ui1, local_path1, basep): Merged<'_>,
    (root2, path2, ui2): (&Root, &Path<Global>, &UpdateItem),
    ((desc1, fp1, ress1), (desc2, fp2, ress2)): FileInfos,
    id: FileId,
    show_merge: &mut ShowMerge<'_>,
) -> Result<(), Error> {
    use crate::common::PrevState;
    use UpdateItem::{NoUpdates, Updates};
    let host = rpc.local_host();
    let fspath1 = &root1.fspath;
    // Local copies of both replicas.
    for p in [&m.working1, &m.working2, &m.working_arch] {
        m.delete(ctx, p)?;
    }
    copy::file(
        rpc,
        root1,
        local_path1,
        root1,
        &m.working_dir,
        &m.working1,
        basep,
        Replacing::Copy,
        &Source {
            desc: desc1.clone(),
            fp: fp1.clone(),
            stamp: None,
            ress: ress1,
        },
        id,
    )
    .await?;
    let local_path2 = rpc.call_root(&TRANSLATE_PATH, root2, path2.clone()).await?;
    copy::file(
        rpc,
        root2,
        &local_path2,
        root1,
        &m.working_dir,
        &m.working2,
        basep,
        Replacing::Copy,
        &Source {
            desc: desc2.clone(),
            fp: fp2.clone(),
            stamp: None,
            ress: ress2,
        },
        id,
    )
    .await?;
    // The last synchronised version, if a backup has it.
    let arch = match (ui1, ui2) {
        (Updates(_, PrevState::File(_, fp, ..)), Updates(_, PrevState::File(_, fp2, ..))) => {
            if fp != fp2 {
                return Err(internal("different previous versions"));
            }
            stasher::get_recent_version(host, fspath1, local_path1, fp).await?
        }
        (NoUpdates, Updates(_, PrevState::File(_, fp, ..)))
        | (Updates(_, PrevState::File(_, fp, ..)), NoUpdates) => {
            stasher::get_recent_version(host, fspath1, local_path1, fp).await?
        }
        (Updates(_, PrevState::New), Updates(_, PrevState::New))
        | (Updates(_, PrevState::New), NoUpdates)
        | (NoUpdates, Updates(_, PrevState::New)) => None,
        _ => return Err(internal("merging something that isn't a file")),
    };
    // A copy of it, in case the merge program overwrites it and crashes.
    if let Some(f) = &arch {
        let info = fileinfo::get(false, f, &Path::empty(), &|_| false, &ctx.fs)?;
        copy::Progress::none(|p| {
            copy::local_file(
                ctx,
                f,
                &Path::empty(),
                &m.working_dir,
                &m.working_arch,
                &m.working_arch,
                Replacing::Copy,
                &info.desc,
                gemel_fs::osx::ress_length(&info.os_x.ress_info),
                p,
            )
        })?;
    }
    for p in [&m.new1, &m.new2, &m.new_arch] {
        m.delete(ctx, p)?;
    }
    let fp1 = m.fingerprint(ctx, &m.working1)?;
    let fp2 = m.fingerprint(ctx, &m.working2)?;
    let cmd = {
        let prefs = rpc.client_host().prefs().borrow();
        format_merge_cmd(
            &prefs,
            path1,
            &quotes(&m.abs(&m.working1)),
            &quotes(&m.abs(&m.working2)),
            arch.as_ref().map(quotes).as_deref(),
            &quotes(&m.abs(&m.new1)),
            &quotes(&m.abs(&m.new2)),
            &quotes(&m.abs(&m.new_arch)),
            if *prefs.read(&crate::globals::BATCH) {
                "batch"
            } else {
                ""
            },
        )?
    };
    with_trace(rpc, |t, p| t.log(p, &format!("Merge command: {cmd}\n")))?;
    let (status, log) = crate::external::run_external_program(&cmd).await?;
    with_trace(rpc, |t, p| {
        t.log(
            p,
            &format!("Merge result ({}):\n{log}\n", show_status(status)),
        )
    })?;
    crate::debug(rpc.client_host(), "files", || {
        format!("Merge result = {}\n", show_status(status))
    });
    // This also shows the result, so it comes before what may fail below.
    if !show_merge(format!("Results of merging {}", path_str(path1)), log).await {
        return Err(Error::Transient(
            "Merge command canceled by the user".to_owned(),
        ));
    }
    // Shown as they would be logged (on stderr, unless the interface has
    // none).
    let mut say = |s: &str| {
        with_trace(rpc, |t, _| {
            if t.send_log_msgs_to_stderr {
                t.msg(s);
            }
        })
    };
    settle_merge(ctx, m, status, (&fp1, &fp2), &mut say)?;
    // Commit the results.
    let arch_fspath = m.abs(&m.working_arch);
    let (desc1, desc2, arch_to) = if system::file_exists(arch_fspath.as_bytes()) {
        let info = fileinfo::get_basic(false, &arch_fspath, &Path::empty(), &|_| false, &ctx.fs)?;
        let fp = os::fingerprint(
            &arch_fspath,
            &Path::empty(),
            info.typ.info_kind(),
            ctx.fs.props.rsrc,
        )?;
        let entry = pseudo_merge_desc(ctx, &desc1, &desc2, ui1, ui2, &info.desc)
            .map(|desc| Archive::File(desc, fp, info.stamp(&ctx.fs), info.ress_stamp()));
        (
            desc1.set_time(&info.desc),
            desc2.set_time(&info.desc),
            entry,
        )
    } else {
        (desc1, desc2, None)
    };
    copy_back(
        rpc,
        &m.working_dir,
        &m.working1,
        root1,
        path1,
        &desc1,
        ui1,
        arch_to.clone(),
        id,
    )
    .await?;
    copy_back(
        rpc,
        &m.working_dir,
        &m.working2,
        root2,
        path2,
        &desc2,
        ui2,
        arch_to,
        id,
    )
    .await
}

/// `pseudoMergeDesc`: the archive's props for a merged file: its length
/// and times, and the rest from whichever original is closest (so the
/// next sync sees no props conflict), if one is.
fn pseudo_merge_desc(
    ctx: &Ctx,
    desc1: &Props,
    desc2: &Props,
    ui1: &UpdateItem,
    ui2: &UpdateItem,
    merged: &Props,
) -> Option<Props> {
    use crate::common::PrevState;
    use UpdateItem::{NoUpdates, Updates};
    let c = &ctx.fs.props;
    let fixup = |d: &Props| d.set_time(merged).set_length(merged.length());
    let pref = if fixup(desc1).similar(&fixup(desc2), c) {
        Some(desc1)
    } else {
        match (ui1, ui2) {
            (Updates(_, PrevState::File(pdesc1, ..)), Updates(_, PrevState::File(pdesc2, ..))) => {
                if pdesc1.similar(desc1, c) {
                    Some(desc1)
                } else if pdesc2.similar(desc2, c) {
                    Some(desc2)
                } else if pdesc1.similar(pdesc2, c) {
                    Some(pdesc1)
                } else {
                    None
                }
            }
            (NoUpdates, NoUpdates | Updates(..)) => Some(desc1),
            (Updates(..), NoUpdates) => Some(desc2),
            _ => None,
        }
    };
    pref.map(fixup)
}

/// Registers this module's commands.
pub fn register_commands(rpc: &mut Rpc) -> Result<(), Error> {
    let r = rpc.registry_mut();
    r.register_host(&PROCESS_COMMIT_LOG)?;
    r.register_root(&DELETE)?;
    r.register_root(&SET_PROP)?;
    r.register_root(&UPDATE_PROPS)?;
    r.register_root(&MKDIR)?;
    r.register_root(&SET_DIR_PROP)?;
    r.register_root(&MAKE_SYMLINK)?;
    r.register_root(&RENAME)?;
    r.register_root(&SETUP_TARGET_PATHS)?;
    r.register_root(&SETUP_TARGET_PATHS_AND_CREATE_PARENT_DIRECTORY)?;
    r.register_root(&UPDATE_SOURCE_ARCHIVE)?;
    r.register_root(&DELETE_SPURIOUS_CHILDREN)?;
    r.register_root(&MOVE_ARCHIVE)?;
    r.register_root(&MOVE)
}
