//! Backups of overwritten files.
//!
//! Ports Unison `src/stasher.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Before propagation overwrites or deletes a path matching `backup`, it
//! is kept as a backup: renamed (or copied) into the backup directory
//! (central, or next to the file), with the older backups shifted up a
//! version. Paths matching `backupcurr` also have their current version
//! kept (as version 0), as merge programs' common ancestor. The names
//! backups get (`makeBackupName`) are a host slot, set by
//! [`init_backups_local`] from the preferences.

use std::sync::Arc;

use gemel_base::case;
use gemel_base::path::{Local, Path};
use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
use gemel_base::util::{self, Error};
use gemel_fs::fileinfo::{self, Typ};
use gemel_fs::fingerprint::Fingerprint;
use gemel_fs::fspath::Fspath;
use gemel_fs::os;
use gemel_fs::osx;
use gemel_fs::props::Props;
use gemel_rpc::host::Host;
use gemel_rpc::registry::{RootCmd, Rpc};

use crate::archive::Archive;
use crate::copy::{self, Ctx, Progress, Replacing};
use crate::globals::{self, Ignore, Roots};
use crate::pred::{Compiled, Pred};
use crate::{exclusive, update, xferhint};

// ---- Preferences ----

/// `backuploc` (alias `backuplocation`).
pub static BACKUP_LOCATION: PrefDef<String> = PrefDef::new(
    "backuploc",
    Category::Advanced(Topic::Syncprocess),
    || "central".to_owned(),
    "where backups are stored ('local' or 'central')",
    "This preference determines whether backups should be kept locally, near the \
     original files, or in a central directory specified by the \\texttt{backupdir} \
     preference. If set to \\verb|local|, backups will be kept in \
     the same directory as the original files, and if set to \\verb|central|, \
     \\texttt{backupdir} will be used instead.",
);

/// `backup` (alias `mirror`).
pub static BACKUP: Pred = Pred::new(
    "backup",
    Category::Advanced(Topic::Syncprocess),
    "add a pattern to the backup list",
    "Including the preference \\texttt{-backup \\ARG{pathspec}} \
     causes Unison to keep backup files for each path that matches \
     \\ARG{pathspec}; directories (nor their permissions or any other \
      metadata) are not backed up.  These backup files are kept in the \
     directory specified by the \\verb|backuplocation| preference. The backups are named \
     according to the \\verb|backupprefix| and \\verb|backupsuffix| preferences. \
     The number of versions that are kept is determined by the \
     \\verb|maxbackups| preference.\
     \n\n The syntax of \\ARG{pathspec} is described in \
     \\sectionref{pathspec}{Path Specification}.",
);

/// `backupnot` (alias `mirrornot`).
pub static BACKUP_NOT: Pred = Pred::new(
    "backupnot",
    Category::Advanced(Topic::Syncprocess),
    "add a pattern to the backupnot list",
    "The values of this preference specify paths or individual files or \
     regular expressions that should {\\em not} \
     be backed up, even if the {\\tt backup} preference selects \
     them---i.e., it selectively overrides {\\tt backup}.",
);

/// `backupprefix`.
pub static BACKUP_PREFIX: PrefDef<String> = PrefDef::new(
    "backupprefix",
    Category::Advanced(Topic::Syncprocess),
    || ".bak.$VERSION.".to_owned(),
    "prefix for the names of backup files",
    "When a backup for a file \\verb|NAME| is created, it is stored \
     in a directory specified by \\texttt{backuplocation}, in a file called \
     \\texttt{backupprefix}\\verb|NAME|\\texttt{backupsuffix}. \
     \\texttt{backupprefix} can include a directory name (causing Unison to \
     keep all backup files for a given directory in a subdirectory with this name), and both \
      \\texttt{backupprefix} and \\texttt{backupsuffix} can contain the string \
     \\ARG{\\$VERSION}, which will be replaced by the \\emph{age} of the backup \
     (1 for the most recent, 2 for the second most recent, and so on...). \
     This keyword is ignored if it appears in a directory name \
     in the prefix; if it  does not appear anywhere \
     in the prefix or the suffix, it will be automatically \
     placed at the beginning of the suffix.  \
     \n\n\
     One thing to be careful of: If the {\\tt backuploc} preference is set \
     to {\\tt local}, Unison will automatically ignore {\\em all} files \
     whose prefix and suffix match {\\tt backupprefix} and {\\tt backupsuffix}.  \
     So be careful to choose values for these preferences that are sufficiently \
     different from the names of your real files.",
);

/// `backupsuffix`.
pub static BACKUP_SUFFIX: PrefDef<String> = PrefDef::new(
    "backupsuffix",
    Category::Advanced(Topic::Syncprocess),
    String::new,
    "a suffix to be added to names of backup files",
    "See \\texttt{backupprefix} for full documentation.",
);

/// `backups`.
pub static BACKUPS: PrefDef<bool> = PrefDef::new(
    "backups",
    Category::Advanced(Topic::Syncprocess),
    || false,
    "keep backup copies of all files (see also 'backup')",
    "Setting this flag to true is equivalent to \
      setting \\texttt{backuplocation} to \\texttt{local} \
      and \\texttt{backup} to \\verb|Name *|.",
)
.deprecated();

/// `maxbackups` (aliases `mirrorversions` and `backupversions`).
pub static MAX_BACKUPS: PrefDef<i64> = PrefDef::new(
    "maxbackups",
    Category::Advanced(Topic::Syncprocess),
    || 2,
    "number of backed up versions of a file",
    "This preference specifies the number of backup versions that will \
     be kept by unison, for each path that matches the predicate \
     \\verb|backup|.  The default is 2.",
);

/// `backupdir`.
pub static BACKUP_DIR: PrefDef<String> = PrefDef::new(
    "backupdir",
    Category::Advanced(Topic::Syncprocess),
    String::new,
    "directory for storing centralized backups",
    "If this preference is set, Unison will use it as the name of the \
     directory used to store backup files specified by \
     the {\\tt backup} preference, when {\\tt backuplocation} is set \
     to \\verb|central|. It is checked {\\em after} the \
     {\\tt UNISONBACKUPDIR} environment variable.",
);

/// `backupcurr` (alias `backupcurrent`).
pub static BACKUP_CURRENT: Pred = Pred::new(
    "backupcurr",
    Category::Advanced(Topic::Syncprocess),
    "add a pattern to the backupcurr list",
    "Including the preference \\texttt{-backupcurr \\ARG{pathspec}} \
      causes Unison to keep a backup of the {\\em current} version of every file \
     matching \\ARG{pathspec}.  \
      This file will be saved as a backup with version number 000. Such \
      backups can be used as inputs to external merging programs, for instance.  See \
     the documentation for the \\verb|merge| preference. \
      For more details, see \\sectionref{merge}{Merging Conflicting Versions}.\
     \n\n The syntax of \\ARG{pathspec} is described in \
     \\sectionref{pathspec}{Path Specification}.",
);

/// `backupcurrnot` (alias `backupcurrentnot`).
pub static BACKUP_CURRENT_NOT: Pred = Pred::new(
    "backupcurrnot",
    Category::Advanced(Topic::Syncprocess),
    "add a pattern to the backupcurrnot list",
    "Exceptions to \\verb|backupcurr|, like the \\verb|ignorenot| preference.",
);

/// Registers this module's preferences and aliases, in OCaml's order.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&BACKUP_LOCATION)?;
    prefs.alias(&BACKUP_LOCATION, "backuplocation")?;
    BACKUP.register(prefs)?;
    BACKUP.alias(prefs, "mirror")?;
    BACKUP_NOT.register(prefs)?;
    BACKUP_NOT.alias(prefs, "mirrornot")?;
    prefs.register(&BACKUP_PREFIX)?;
    prefs.register(&BACKUP_SUFFIX)?;
    prefs.register(&BACKUPS)?;
    prefs.register(&MAX_BACKUPS)?;
    prefs.alias(&MAX_BACKUPS, "mirrorversions")?;
    prefs.alias(&MAX_BACKUPS, "backupversions")?;
    prefs.register(&BACKUP_DIR)?;
    BACKUP_CURRENT.register(prefs)?;
    BACKUP_CURRENT_NOT.register(prefs)?;
    BACKUP_CURRENT.alias(prefs, "backupcurrent")?;
    BACKUP_CURRENT_NOT.alias(prefs, "backupcurrentnot")
}

/// `shouldBackup`.
pub fn should_backup<S>(prefs: &Prefs, p: &Path<S>) -> Result<bool, Error> {
    let mode = case::mode(prefs);
    Ok(BACKUP.test(prefs, mode, p.as_bytes())? && !BACKUP_NOT.test(prefs, mode, p.as_bytes())?)
}

/// `shouldBackupCurrent`.
pub fn should_backup_current<S>(prefs: &Prefs, p: &Path<S>) -> Result<bool, Error> {
    let mode = case::mode(prefs);
    Ok(BACKUP_CURRENT.test(prefs, mode, p.as_bytes())?
        && !BACKUP_CURRENT_NOT.test(prefs, mode, p.as_bytes())?)
}

/// `translateOldPrefs`: `backups` means `backup = Name *` with local
/// backups.
fn translate_old_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    match (
        BACKUP.patterns(prefs).is_empty() && BACKUP_NOT.patterns(prefs).is_empty(),
        *prefs.read(&BACKUPS),
    ) {
        (true, true) => {
            BACKUP.intern(prefs, vec!["Name *".to_owned()]);
            prefs.set(&BACKUP_LOCATION, "local".to_owned());
            Ok(())
        }
        (_, false) => Ok(()),
        (false, true) => Err(Error::Fatal(
            "Both old 'backups' preference and new 'backup' preference are set!".to_owned(),
        )),
    }
}

/// `backupDirectory`: `UNISONBACKUPDIR`, `UNISONMIRRORDIR`, `backupdir`,
/// or `backup` in the Unison directory.
pub fn backup_directory(host: &Host) -> Result<Fspath, Error> {
    let env = |v| std::env::var_os(v).map(|s| gemel_fs::system::name_bytes(&s));
    let dir = match env("UNISONBACKUPDIR").or_else(|| env("UNISONMIRRORDIR")) {
        Some(d) => d,
        None => {
            let d = host.prefs().borrow().read(&BACKUP_DIR).clone();
            if d.is_empty() {
                host.file_in_unison_dir("backup")
            } else {
                d.into_bytes()
            }
        }
    };
    Fspath::canonize(Some(&dir))
}

// ---- Backup names ----

/// `Str.quote`.
fn str_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '[' | ']' | '*' | '.' | '\\' | '?' | '+' | '^' | '$') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

const VERSION_TAG: &str = "$VERSION";

/// `backup_rx`: the `Str` regular expressions of the backup directory (if
/// the prefix has one), prefix and suffix, with the version as a group.
fn backup_rx(prefs: &Prefs) -> Result<(Option<String>, String, String), Error> {
    const VERSION_RX: &str = "\\([0-9]+\\)";
    let prefix = prefs.read(&BACKUP_PREFIX);
    let suffix = str_quote(prefs.read(&BACKUP_SUFFIX));
    let udir = match util::filename_dirname(prefix).as_str() {
        "." => String::new(),
        s => format!("{}/", s.replace('\\', "/")),
    };
    let uprefix = util::filename_basename(prefix);
    let dir = (!udir.is_empty()).then(|| str_quote(&udir));
    let prefix = str_quote(&uprefix);
    let tag = str_quote(VERSION_TAG);
    if !format!("{prefix}{suffix}").contains(&tag) {
        return Err(Error::Fatal(
            "Either backupprefix or backupsuffix must contain '$VERSION'".to_owned(),
        ));
    }
    Ok((
        dir,
        prefix.replace(&tag, VERSION_RX),
        suffix.replace(&tag, VERSION_RX),
    ))
}

/// The pattern for backups made by older versions.
pub const OLD_BACKUP_PREF_PATHSPEC: &str = "Name *.unison.bak";

/// `addBackupFilesToIgnorePref`: ignores old-style backups, and (with
/// local backups) the backups.
fn add_backup_files_to_ignore_pref(prefs: &mut Prefs) -> Result<(), Error> {
    let (dir_rx, prefix_rx, suffix_rx) = backup_rx(prefs)?;
    let regexp_to_rx = |s: &str| s.replace("\\)", "").replace("\\(", "");
    let d = match &dir_rx {
        None => "/".to_owned(),
        Some(s) => regexp_to_rx(s),
    };
    let p = regexp_to_rx(&prefix_rx);
    let s = regexp_to_rx(&suffix_rx);
    let full = format!("(.*/)?{p}.*{s}");
    let dir = format!("(.*/)?{}", &d[..d.len() - 1]);
    let the_regexp = match dir_rx {
        None => format!("Regex {full}"),
        Some(_) => format!("Regex {dir}"),
    };
    globals::add_regexp_to_ignore(prefs, OLD_BACKUP_PREF_PATHSPEC);
    if prefs.read(&BACKUP_LOCATION) == "local" {
        globals::add_regexp_to_ignore(prefs, &the_regexp);
    }
    Ok(())
}

/// A backup prefix or suffix, given the version (`make_prefix` and
/// `make_suffix`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Affix {
    /// Without the version.
    Text(String),
    /// Text before and after the version.
    Version(String, String),
}

impl Affix {
    /// `makeFun`: `s` with `$VERSION` (at most once) as the version.
    fn new(s: &str) -> Result<Affix, Error> {
        let parts: Vec<&str> = s.split(VERSION_TAG).collect();
        match parts[..] {
            [t] => Ok(Affix::Text(t.to_owned())),
            // Str.full_split drops the empty text around a lone tag, which
            // OCaml then rejects.
            [before, after] if !(before.is_empty() && after.is_empty()) => {
                Ok(Affix::Version(before.to_owned(), after.to_owned()))
            }
            _ => Err(Error::Fatal(
                "The tag $VERSION should only appear once in the backupprefix and \
                 backupsuffix preferences."
                    .to_owned(),
            )),
        }
    }

    fn with(&self, i: i64) -> String {
        match self {
            Affix::Text(t) => t.clone(),
            Affix::Version(before, after) => format!("{before}{i}{after}"),
        }
    }
}

/// How backups are named (`make_prefix`, `make_suffix`), and where they
/// go: a host slot, set by [`init_backups_local`].
#[derive(Debug, Clone, Default)]
struct Naming(Option<(Affix, Affix)>);

/// `updateBackupNamingFunctions`.
fn update_backup_naming_functions(host: &Host) -> Result<(), Error> {
    let naming = {
        let prefs = host.prefs().borrow();
        (
            Affix::new(prefs.read(&BACKUP_PREFIX))?,
            Affix::new(prefs.read(&BACKUP_SUFFIX))?,
        )
    };
    host.state::<Naming>().borrow_mut().0 = Some(naming);
    Ok(())
}

/// The backup naming, from the preferences if [`init_backups_local`]
/// hasn't run (where OCaml would fail an assertion).
fn naming(host: &Host) -> Result<(Affix, Affix), Error> {
    if host.state::<Naming>().borrow().0.is_none() {
        update_backup_naming_functions(host)?;
    }
    Ok(host
        .state::<Naming>()
        .borrow()
        .0
        .clone()
        .expect("set above"))
}

/// The backup settings of one operation, taken from the host.
struct Backups {
    ctx: Ctx,
    naming: (Affix, Affix),
    central: bool,
    location: String,
    max_backups: i64,
    /// [`backup_directory`], if it could be resolved (OCaml only resolves
    /// it when it is needed).
    backup_dir: Result<Fspath, Error>,
    backup: Arc<Compiled>,
    backup_not: Arc<Compiled>,
    backup_current: Arc<Compiled>,
    backup_current_not: Arc<Compiled>,
    ignore: Ignore,
}

impl Backups {
    fn of(host: &Host) -> Result<Backups, Error> {
        let naming = naming(host)?;
        let backup_dir = backup_directory(host);
        let prefs = host.prefs().borrow();
        let mode = case::mode(&prefs);
        let location = prefs.read(&BACKUP_LOCATION).clone();
        Ok(Backups {
            ctx: Ctx::of(host)?,
            naming,
            central: location == "central",
            location,
            max_backups: *prefs.read(&MAX_BACKUPS),
            backup_dir,
            backup: BACKUP.compiled(&prefs, mode)?,
            backup_not: BACKUP_NOT.compiled(&prefs, mode)?,
            backup_current: BACKUP_CURRENT.compiled(&prefs, mode)?,
            backup_current_not: BACKUP_CURRENT_NOT.compiled(&prefs, mode)?,
            ignore: Ignore::new(&prefs, mode)?,
        })
    }

    /// `shouldBackup`.
    fn should_backup(&self, p: &Path<Local>) -> bool {
        self.backup.test(p.as_bytes()) && !self.backup_not.test(p.as_bytes())
    }

    /// `shouldBackupCurrent`.
    fn should_backup_current(&self, p: &Path<Local>) -> bool {
        self.backup_current.test(p.as_bytes()) && !self.backup_current_not.test(p.as_bytes())
    }

    /// `makeBackupName`: version `i`'s name. A root that is a file is named
    /// after itself; in a central directory the current version (0) has the
    /// original name.
    fn make_backup_name(
        &self,
        fspath: &Fspath,
        path: &Path<Local>,
        i: i64,
    ) -> Result<Path<Local>, Error> {
        let path = if path.is_empty() {
            Path::from_bytes(util::filename_basename(&fspath.to_string()).as_bytes())?
        } else {
            path.clone()
        };
        Ok(if i == 0 && self.central {
            path
        } else {
            path.add_prefix_to_final_name(self.naming.0.with(i).as_bytes())
                .add_suffix_to_final_name(self.naming.1.with(i).as_bytes())
        })
    }

    /// `stashDirectory`: where backups of `fspath`'s paths go.
    fn stash_directory(&self, fspath: &Fspath, path: &Path<Local>) -> Result<Fspath, Error> {
        match self.location.as_str() {
            "central" => self.backup_dir.clone(),
            // A root that is a file: next to it.
            "local" if path.is_empty() => {
                Fspath::canonize(Some(util::filename_dirname(&fspath.to_string()).as_bytes()))
            }
            "local" => Ok(fspath.clone()),
            _ => Err(Error::Fatal(
                "backuplocation preference should be setto central or local.".to_owned(),
            )),
        }
    }

    /// `backupPath`'s `f`: version `i`'s name, after moving the backups
    /// there up a version (and deleting the oldest).
    fn shift(
        &self,
        sfspath: &Fspath,
        fspath: &Fspath,
        path: &Path<Local>,
        i: i64,
    ) -> Result<Path<Local>, Error> {
        let temp = self.make_backup_name(fspath, path, i)?;
        if os::exists(sfspath, &temp, &self.ctx.fs)? {
            if i < self.max_backups {
                let next = self.shift(sfspath, fspath, path, i + 1)?;
                os::rename(
                    "backupPath",
                    sfspath,
                    &temp,
                    sfspath,
                    &next,
                    None,
                    &self.ctx.fs,
                )?;
            } else {
                os::delete(sfspath, &temp, &self.ctx.fs)?;
            }
        }
        Ok(temp)
    }

    /// `backupPath`'s `mkdirectories`: makes the backup directories down to
    /// `dir`. (In a central directory, something else in the way is
    /// shifted as a backup itself.)
    fn make_directories(&self, sfspath: &Fspath, dir: &Path<Local>) -> Result<(), Error> {
        let fs = &self.ctx.fs;
        if !os::exists(sfspath, &Path::empty(), fs)? {
            os::create_dir(
                sfspath,
                &Path::empty(),
                Props::dir_default().perm.bits as u32,
            )?;
        }
        let Some((_, parent)) = dir.deconstruct_rev() else {
            return Ok(());
        };
        self.make_directories(sfspath, &parent)?;
        let perms = fileinfo::get_basic(false, sfspath, &Path::empty(), &|_| false, fs)?
            .desc
            .perm
            .bits as u32;
        if !os::exists(sfspath, dir, fs)? {
            os::create_dir(sfspath, dir, perms)?;
        } else if self.central
            && fileinfo::get_type(false, sfspath, dir, &|_| false, fs)? != Typ::Directory
        {
            let dir = self.shift(sfspath, sfspath, dir, 0)?;
            os::create_dir(sfspath, &dir, perms)?;
        }
        Ok(())
    }

    /// `backupPath`: where to put a new backup of `path` (shifting the
    /// older ones), or none if the current version 0 is the same already.
    fn backup_path(
        &self,
        fspath: &Fspath,
        path: &Path<Local>,
    ) -> Result<Option<(Fspath, Path<Local>)>, Error> {
        let fs = &self.ctx.fs;
        let sfspath = self.stash_directory(fspath, path)?;
        let path0 = self.make_backup_name(fspath, path, 0)?;
        let follow = |p: &Path<Local>| self.ctx.follow.test(p.as_bytes());
        let source_typ = fileinfo::get_type(true, fspath, path, &follow, fs)?;
        let path0_typ = fileinfo::get_type(false, &sfspath, &path0, &|_| false, fs)?;
        let same = match (source_typ, path0_typ) {
            (Typ::File, Typ::File) => {
                Fingerprint::file(fspath, path)? == Fingerprint::file(&sfspath, &path0)?
            }
            (Typ::Symlink, Typ::Symlink) => {
                os::read_link(fspath, path)? == os::read_link(&sfspath, &path0)?
            }
            _ => false,
        };
        if same {
            return Ok(None);
        }
        let spath = self.shift(&sfspath, fspath, path, 0)?;
        match spath.deconstruct_rev() {
            None => self.make_directories(&sfspath, &Path::empty())?,
            Some((_, dir)) => self.make_directories(&sfspath, &dir)?,
        }
        Ok(Some((sfspath, spath)))
    }

    /// [`backup`]'s filesystem work: where the backup went, if one was made.
    fn backup(
        &self,
        fspath: &Fspath,
        path: &Path<Local>,
        disposition: Disposition,
    ) -> Result<Option<(Fspath, Path<Local>)>, Error> {
        let fs = &self.ctx.fs;
        let (working_dir, real_path) =
            fspath.find_working_dir(path, self.ctx.follow.test(path.as_bytes()))?;
        let dispose = || {
            if disposition == Disposition::AndRemove {
                os::delete(&working_dir, &real_path, fs)
            } else {
                Ok(())
            }
        };
        if !os::exists(&working_dir, &real_path, fs)? {
            return Ok(None);
        }
        if !self.should_backup(path) {
            dispose()?;
            return Ok(None);
        }
        let Some((back_root, back_path)) = self.backup_path(fspath, path)? else {
            dispose()?;
            return Ok(None);
        };
        let by_copying = || {
            copy::recursively(&self.ctx, fspath, path, &back_root, &back_path)?;
            dispose()
        };
        if disposition == Disposition::AndRemove {
            // FIX (OCaml's): this does the wrong thing with followed links.
            match os::rename(
                "backup",
                &working_dir,
                &real_path,
                &back_root,
                &back_path,
                None,
                fs,
            ) {
                Ok(()) => {}
                Err(Error::Transient(_)) => by_copying()?,
                Err(e) => return Err(e),
            }
        } else {
            by_copying()?;
        }
        Ok(Some((back_root, back_path)))
    }

    /// [`stash_current_version`]'s filesystem work.
    fn stash_current_version(
        &self,
        fspath: &Fspath,
        path: &Path<Local>,
        source_path: Option<&Path<Local>>,
    ) -> Result<(), Error> {
        if !self.should_backup_current(path) {
            return Ok(());
        }
        let source = source_path.unwrap_or(path);
        let follow = |p: &Path<Local>| self.ctx.follow.test(p.as_bytes());
        let stat = fileinfo::get(
            true,
            fspath,
            source,
            &follow,
            &self.ctx.fs,
            &Props::dummy(),
            &self.ctx.xattr.ctx(),
        )?;
        match stat.typ {
            Typ::Absent => Ok(()),
            Typ::Directory => {
                if source_path.is_some() {
                    return Err(Error::Fatal(
                        "Internal error: stasher: stashing a directory from elsewhere".to_owned(),
                    ));
                }
                for n in os::children_of(fspath, path, &self.ctx.temp_names, &self.ctx.fs)? {
                    let child = path.child(&n);
                    if !self.ignore.test(&child) {
                        self.stash_current_version(fspath, &child, None)?;
                    }
                }
                Ok(())
            }
            Typ::Symlink => {
                if let Some((sfspath, spath)) = self.backup_path(fspath, path)? {
                    os::symlink(&sfspath, &spath, &os::read_link(fspath, source)?)?;
                }
                Ok(())
            }
            Typ::File => {
                if let Some((sfspath, spath)) = self.backup_path(fspath, path)? {
                    Progress::none(|p| {
                        copy::local_file(
                            &self.ctx,
                            fspath,
                            source,
                            &sfspath,
                            &spath,
                            &spath,
                            Replacing::Copy,
                            &stat.desc,
                            osx::ress_length(&stat.os_x.ress_info),
                            p,
                        )
                    })?;
                }
                Ok(())
            }
        }
    }

    /// [`get_recent_version`]'s filesystem work.
    fn get_recent_version(
        &self,
        fspath: &Fspath,
        path: &Path<Local>,
        fp: &os::FullFingerprint,
    ) -> Result<Option<Fspath>, Error> {
        let fs = &self.ctx.fs;
        let dir = self.stash_directory(fspath, path)?;
        let mut i = 0;
        loop {
            let p = self.make_backup_name(fspath, path, i)?;
            if os::exists(&dir, &p, fs)? {
                let typ = fileinfo::get_type(false, &dir, &p, &|_| false, fs)?;
                if os::fingerprint(&dir, &p, typ.info_kind(), fs.props.rsrc)? == *fp {
                    return Ok(Some(dir.concat(&p)));
                }
            }
            // Versions 0 to maxbackups.
            if i >= self.max_backups {
                return Ok(None);
            }
            i += 1;
        }
    }
}

// ---- Backing up ----

/// What [`backup`] does with the original.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Remove it: moved to the backup, or deleted if none is needed.
    AndRemove,
    /// Leave it: the backup is a copy.
    ByCopying,
}

/// `Stasher.backup`: backs up `path` if `backup` says so; with
/// [`Disposition::AndRemove`] the original is gone afterwards. The backed
/// up files of `arch` become copy hints. The files are handled off the
/// engine thread.
pub async fn backup(
    host: &Host,
    fspath: &Fspath,
    path: &Path<Local>,
    disposition: Disposition,
    arch: &Archive,
) -> Result<(), Error> {
    let b = Backups::of(host)?;
    let (f, p) = (fspath.clone(), path.clone());
    let made = exclusive::now(move || b.backup(&f, &p, disposition)).await?;
    if let Some((back_root, back_path)) = made {
        update::iter_files(&back_root, &back_path, arch, &mut |f, p, fp| {
            xferhint::insert_entry(host, (f.clone(), p.clone(), fp.clone()));
        });
    }
    Ok(())
}

/// `stashCurrentVersion`: keeps the current version of `path` (or of the
/// bits at `source_path`, kept as `path`'s) if `backupcurr` says so; a
/// directory's children one by one, except ignored ones.
pub async fn stash_current_version(
    host: &Host,
    fspath: &Fspath,
    path: &Path<Local>,
    source_path: Option<&Path<Local>>,
) -> Result<(), Error> {
    if !should_backup_current(&host.prefs().borrow(), path)? {
        return Ok(());
    }
    let b = Backups::of(host)?;
    let (f, p, s) = (fspath.clone(), path.clone(), source_path.cloned());
    exclusive::now(move || b.stash_current_version(&f, &p, s.as_ref())).await
}

/// `getRecentVersion`: a backup of `path` (in this replica's backups only)
/// with this fingerprint, if there is one.
pub async fn get_recent_version(
    host: &Host,
    fspath: &Fspath,
    path: &Path<Local>,
    fp: &os::FullFingerprint,
) -> Result<Option<Fspath>, Error> {
    let b = Backups::of(host)?;
    let (f, p, fp) = (fspath.clone(), path.clone(), fp.clone());
    exclusive::now(move || b.get_recent_version(&f, &p, &fp)).await
}

// ---- Initialisation ----

/// `initBackupsLocal`: applies `backups`, ignores backups (see
/// [`add_backup_files_to_ignore_pref`]), and sets how they are named.
pub fn init_backups_local(host: &Host) -> Result<(), Error> {
    {
        let mut prefs = host.prefs().borrow_mut();
        translate_old_prefs(&mut prefs)?;
        add_backup_files_to_ignore_pref(&mut prefs)?;
    }
    update_backup_naming_functions(host)
}

/// `initBackupsRoot`.
pub static INIT_BACKUPS: RootCmd<(), ()> = RootCmd::new(
    "initBackups",
    |(), _, _| {},
    |_, _| Ok(()),
    |(), _, _| {},
    |_, _| Ok(()),
    |host, _, ()| Box::pin(async move { init_backups_local(&host) }),
);

/// `Stasher.initBackups`: on every root (so twice on a host with both).
pub async fn init_backups(rpc: &Rpc, roots: &Roots) -> Result<(), Error> {
    roots
        .iter(|r| rpc.call_root(&INIT_BACKUPS, r, ()))
        .await
        .map(drop)
}

/// Registers this module's commands.
pub fn register_commands(rpc: &mut Rpc) -> Result<(), Error> {
    rpc.registry_mut().register_root(&INIT_BACKUPS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affixes_take_the_version_once() {
        let with = |s, i| Affix::new(s).map(|a| a.with(i));
        assert_eq!(with("", 3).unwrap(), "");
        assert_eq!(with(".bak.$VERSION.", 3).unwrap(), ".bak.3.");
        assert_eq!(with("$VERSION.", 3).unwrap(), "3.");
        assert_eq!(with(".v$VERSION", 3).unwrap(), ".v3");
        assert_eq!(with("plain", 3).unwrap(), "plain");
        assert!(with("$VERSION", 3).is_err());
        assert!(with("$VERSION$VERSION", 3).is_err());
        assert!(with("a$VERSIONb$VERSIONc", 3).is_err());
    }

    #[test]
    fn str_quote_escapes_like_ocaml() {
        assert_eq!(str_quote(".bak.$VERSION.(x)"), "\\.bak\\.\\$VERSION\\.(x)");
        assert_eq!(str_quote("[a]*?+^\\"), "\\[a\\]\\*\\?\\+\\^\\\\");
    }
}
