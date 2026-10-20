//! Global roots, paths and preferences.
//!
//! Ports Unison `src/globals.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! OCaml keeps the installed roots in a global; here they are a [`Roots`]
//! value of the client session. Preferences are read from the [`Prefs`]
//! passed in. Not ported: `allRootsMapWithWaitingAction`, which comes with
//! the user interfaces that need it.

use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

use gemel_base::case::Mode;
use gemel_base::clroot::{self, Clroot, ClrootError};
use gemel_base::name::Name;
use gemel_base::path::{Global, Local, Path};
use gemel_base::prefs::{self, Category, DumpedPref, Internal, PrefDef, Prefs, Topic};
use gemel_base::util::Error;
use gemel_fs::fspath::Fspath;
use gemel_rpc::registry::{HostCmd, Rpc};
use gemel_rpc::root::{self, Host, Root};

use crate::negotiate;
use crate::pred::{Compiled, Pred};

// ---- Roots and paths ----

/// `root`.
pub static ROOT: PrefDef<Vec<String>> = PrefDef::new(
    "root",
    Category::Basic(Topic::Sync),
    Vec::new,
    "root of a replica (should be used exactly twice)",
    "Each use of this preference names the root of one of the replicas \
     for Unison to synchronize.  Exactly two roots are needed, so normal \
     modes of usage are either to give two values for \\verb|root| in the \
     profile, or to give no values in the profile and provide two \
     on the command line.  \
     Details of the syntax of roots can be found in \
     \\sectionref{roots}{Roots}.\n\n\
     The two roots can be given in either order; Unison will sort them \
     into a canonical order before doing anything else.  It also tries to \
     `canonize' the machine names and paths that appear in the roots, so \
     that, if Unison is invoked later with a slightly different name \
     for the same root, it will be able to locate the correct archives.",
);

/// `Globals.setRawRoots`.
pub fn set_raw_roots(prefs: &mut Prefs, roots: &[String]) {
    prefs.set(&ROOT, roots.iter().rev().cloned().collect());
}

/// `Globals.rawRoots`: the roots as given, in order.
pub fn raw_roots(prefs: &Prefs) -> Vec<String> {
    prefs.read(&ROOT).iter().rev().cloned().collect()
}

/// A root specification's parse error, as the error OCaml raises.
pub fn clroot_error(e: ClrootError) -> Error {
    match e {
        ClrootError::Transient(m) => Error::Transient(m),
        ClrootError::Fatal(m) | ClrootError::IllegalValue(m) | ClrootError::InvalidArgument(m) => {
            Error::Fatal(m)
        }
    }
}

/// `Globals.parsedClRawRoots`.
pub fn parsed_cl_raw_roots(prefs: &Prefs) -> Result<Vec<Clroot>, Error> {
    raw_roots(prefs)
        .iter()
        .map(|r| clroot::parse_root(r).map_err(clroot_error))
        .collect()
}

fn wrong_num_roots(roots: &[String]) -> Error {
    Error::Fatal(format!(
        "Wrong number of roots: 2 expected, but {} provided ({})\n\
         (Maybe you specified roots both on the command line and in the profile?)",
        roots.len(),
        roots.join(", ")
    ))
}

/// `Globals.rawRootPair`.
pub fn raw_root_pair(prefs: &Prefs) -> Result<(String, String), Error> {
    match <[String; 2]>::try_from(raw_roots(prefs)) {
        Ok([r1, r2]) => Ok((r1, r2)),
        Err(roots) => Err(wrong_num_roots(&roots)),
    }
}

/// The installed roots (`Globals.theroots`), in the user's order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Roots {
    roots: Vec<Root>,
}

impl Roots {
    /// `Globals.installRoots`: canonicalises the two roots given (`raw`,
    /// from [`raw_roots`]) and negotiates features with their hosts.
    pub async fn install(rpc: &Rpc, raw: &[String]) -> Result<Roots, Error> {
        if raw.len() != 2 {
            return Err(wrong_num_roots(raw));
        }
        let mut roots = Vec::new();
        for r in raw {
            let clroot = clroot::parse_root(r).map_err(clroot_error)?;
            roots.push(root::canonize_root(rpc, &clroot).await?);
        }
        if roots[0] == roots[1] {
            return Err(Error::Fatal(format!(
                "That's no good, the roots appear to be the same! Here's \
                 what I found:\nFirst root: {}\nSecond root: {}",
                roots[0], roots[1]
            )));
        }
        let roots = Roots { roots };
        negotiate::features(rpc, &roots.in_canonical_order()).await?;
        Ok(roots)
    }

    /// Roots installed as given, without canonicalising them or negotiating.
    pub fn from_roots(roots: Vec<Root>) -> Roots {
        Roots { roots }
    }

    /// `Globals.roots`.
    ///
    /// # Panics
    ///
    /// Unless there are two roots (OCaml's `assert false`).
    pub fn pair(&self) -> (&Root, &Root) {
        match &self.roots[..] {
            [r1, r2] => (r1, r2),
            _ => panic!("two roots are installed"),
        }
    }

    /// `Globals.rootsList`: in the user's order.
    pub fn list(&self) -> &[Root] {
        &self.roots
    }

    /// `Globals.rootsInCanonicalOrder`.
    pub fn in_canonical_order(&self) -> Vec<Root> {
        let mut roots = self.roots.clone();
        root::sort_roots(&mut roots);
        roots
    }

    /// `Globals.localRoot`: the first root in canonical order (local if
    /// either is).
    pub fn local_root(&self) -> Option<Root> {
        self.in_canonical_order().into_iter().next()
    }

    fn canonical_is_users(&self) -> bool {
        self.roots == self.in_canonical_order()
    }

    /// `Globals.reorderCanonicalListToUsersOrder`.
    pub fn reorder_canonical_list_to_users_order<T>(&self, mut l: Vec<T>) -> Vec<T> {
        if !self.canonical_is_users() {
            l.reverse();
        }
        l
    }

    /// `Globals.allRootsIter`: runs `f` on each root, concurrently, in
    /// canonical order; fails with the first root's error in that order.
    pub async fn iter<'a, F, Fut>(&'a self, f: F) -> Result<(), Error>
    where
        F: Fn(&'a Root) -> Fut,
        Fut: Future<Output = Result<(), Error>> + 'a,
    {
        let canonical = self.in_canonical_order();
        let order: Vec<&Root> = canonical
            .iter()
            .map(|r| self.roots.iter().find(|x| *x == r).expect("same roots"))
            .collect();
        join_all(order.into_iter().map(f).collect())
            .await
            .into_iter()
            .collect()
    }

    /// `Globals.allRootsIter2`: runs `f` on each root and the element of
    /// `l` for it (in the user's order), concurrently, in canonical order.
    pub async fn iter2<'a, T, F, Fut>(&'a self, f: F, l: Vec<T>) -> Result<(), Error>
    where
        F: Fn(&'a Root, T) -> Fut,
        Fut: Future<Output = Result<(), Error>> + 'a,
    {
        let mut pairs: Vec<(&Root, T)> = self.roots.iter().zip(l).collect();
        pairs.sort_by_key(|&(r, _)| r);
        join_all(pairs.into_iter().map(|(r, v)| f(r, v)).collect())
            .await
            .into_iter()
            .collect()
    }

    /// `Globals.allRootsMap`: runs `f` on each root, concurrently, in
    /// canonical order, and returns the results in the user's order.
    pub async fn map<'a, T, F, Fut>(&'a self, f: F) -> Result<Vec<T>, Error>
    where
        F: Fn(&'a Root) -> Fut,
        Fut: Future<Output = Result<T, Error>> + 'a,
    {
        let canonical = self.in_canonical_order();
        let order: Vec<&Root> = canonical
            .iter()
            .map(|r| self.roots.iter().find(|x| *x == r).expect("same roots"))
            .collect();
        let results = join_all(order.into_iter().map(f).collect())
            .await
            .into_iter()
            .collect::<Result<Vec<T>, Error>>()?;
        Ok(self.reorder_canonical_list_to_users_order(results))
    }
}

/// Runs futures concurrently on this thread (as `Lwt_util.map` does) and
/// returns their outputs in order.
async fn join_all<T, F: Future<Output = T>>(futures: Vec<F>) -> Vec<T> {
    let mut futures: Vec<Pin<Box<F>>> = futures.into_iter().map(Box::pin).collect();
    let mut outputs: Vec<Option<T>> = futures.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (f, out) in futures.iter_mut().zip(outputs.iter_mut()) {
            if out.is_none() {
                match f.as_mut().poll(cx) {
                    Poll::Ready(v) => *out = Some(v),
                    Poll::Pending => pending = true,
                }
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;
    outputs.into_iter().map(|o| o.expect("ready")).collect()
}

// ---- Propagation of preferences ----

/// `installPrefs` (`propagatePrefsTo`): loads the client's preference dump
/// on a host. (OCaml registers it with `registerRootCmdWithConnection`,
/// which sends no path, to get the connection's RPC version; only version
/// 1 is spoken.)
pub static INSTALL_PREFS: HostCmd<Vec<DumpedPref>, ()> = HostCmd::new(
    "installPrefs",
    |d, w, _| prefs::marshal_dumped(w, d),
    |r, _| prefs::unmarshal_dumped(r),
    |(), _, _| {},
    |_, _| Ok(()),
    |host, dump| Box::pin(async move { host.prefs().borrow_mut().load(&dump, 1) }),
);

/// `Globals.propagatePrefs`: sends the client's preferences to the roots'
/// hosts. Local roots share the client's, except in loopback mode, whose
/// host would otherwise keep the preferences it was created with.
pub async fn propagate_prefs(rpc: &Rpc, roots: &Roots) -> Result<(), Error> {
    for r in roots.in_canonical_order() {
        if r.is_local() && !rpc.is_loopback() {
            continue;
        }
        let dump = rpc.client_host().prefs().borrow().dump(1)?;
        rpc.call_host(&INSTALL_PREFS, &r.host, dump).await?;
    }
    Ok(())
}

/// Registers this module's commands.
pub fn register_commands(rpc: &mut Rpc) -> Result<(), Error> {
    rpc.registry_mut().register_host(&INSTALL_PREFS)
}

/// `path`: the paths to synchronise (all of each replica if none).
pub static PATHS: PrefDef<Vec<Path<Global>>> = PrefDef::new(
    "path",
    Category::Basic(Topic::Sync),
    Vec::new,
    "path to synchronize",
    "When no \\verb|path| preference is given, Unison will simply synchronize \
     the two entire replicas, beginning from the given pair of roots.  \
     If one or more \\verb|path| preferences are given, then Unison will \
     synchronize only these paths and their children.  (This is useful \
     for doing a fast sync of just one directory, for example.)  \
     Note that {\\tt path} preferences are interpreted literally---they \
     are not regular expressions.",
);

/// Lists a directory's children (`Os.childrenOf`).
pub type ChildrenOf<'a> = dyn Fn(&Fspath, &Path<Local>) -> Result<Vec<Name>, Error> + 'a;

/// `globPath`: expands a path ending in `*` to the children of its parent
/// in the first root, which must be local (`local_root`).
fn glob_path(
    local_root: Option<&Fspath>,
    p: &Path<Global>,
    children_of: &ChildrenOf<'_>,
) -> Result<Vec<Path<Global>>, Error> {
    let p = p.clone().force_local();
    match p.deconstruct_rev() {
        Some((n, parent)) if n.as_bytes() == b"*" => {
            let Some(fspath) = local_root else {
                return Err(Error::Fatal(format!(
                    "Path {} ends with *, but first root (after canonizing) is non-local",
                    String::from_utf8_lossy(p.as_bytes())
                )));
            };
            Ok(children_of(fspath, &parent)?
                .iter()
                .map(|c| parent.child(c).make_global())
                .collect())
        }
        _ => Ok(vec![p.make_global()]),
    }
}

/// `Globals.expandWildcardPaths`: replaces each path ending in `*` by the
/// children of its parent in the first root (`Os.childrenOf`, passed as
/// `children_of`).
pub fn expand_wildcard_paths(
    prefs: &mut Prefs,
    roots: &Roots,
    children_of: &ChildrenOf<'_>,
) -> Result<(), Error> {
    let canonical = roots.in_canonical_order();
    let local = match &canonical[..] {
        [
            Root {
                host: Host::Local,
                fspath,
            },
            _,
        ] => Some(fspath),
        _ => None,
    };
    let mut expanded = Vec::new();
    for p in prefs.read(&PATHS) {
        expanded.extend(glob_path(local, p, children_of)?);
    }
    prefs.set(&PATHS, expanded);
    Ok(())
}

// ---- Preferences and predicates ----

/// `batch`.
pub static BATCH: PrefDef<bool> = PrefDef::new(
    "batch",
    Category::Basic(Topic::Syncprocess),
    || false,
    "batch mode: ask no questions at all",
    "When this is set to {\\tt true}, the user \
     interface will ask no questions at all.  Non-conflicting changes \
     will be propagated; conflicts will be skipped.",
);

/// `confirmbigdel`.
pub static CONFIRM_BIG_DELETES: PrefDef<bool> = PrefDef::new(
    "confirmbigdel",
    Category::Advanced(Topic::Syncprocess),
    || true,
    "ask about whole-replica (or path) deletes",
    "When this is set to {\\tt true}, Unison will request an extra confirmation if it appears \
     that the entire replica has been deleted, before propagating the change.  If the {\\tt batch} \
     flag is also set, synchronization will be aborted.  When the {\\tt path} preference is used, \
     the same confirmation will be requested for top-level paths.  (At the moment, this flag only \
     affects the text user interface.)  See also the {\\tt mountpoint} preference.",
);

/// `ignore`.
pub static IGNORE: Pred = Pred::new(
    "ignore",
    Category::Basic(Topic::Sync),
    "add a pattern to the ignore list",
    "Including the preference \\texttt{-ignore \\ARG{pathspec}} causes Unison to \
     completely ignore paths that match \\ARG{pathspec} (as well as their \
     children).  This is useful for avoiding synchronizing temporary \
     files, object files, etc. The syntax of \\ARG{pathspec} is \
     described in \\sectionref{pathspec}{Path Specification}, and further \
     details on ignoring paths is found in \
     \\sectionref{ignore}{Ignoring Paths}.",
);

/// `ignorenot`.
pub static IGNORE_NOT: Pred = Pred::new(
    "ignorenot",
    Category::Basic(Topic::Sync),
    "add a pattern to the ignorenot list",
    "This preference overrides the preference \\texttt{ignore}.
      It gives a list of patterns
     (in the same format as
     \\verb|ignore|) for paths that should definitely {\\em not} be ignored,
     whether or not they happen to match one of the \\verb|ignore| patterns.
     \\par Note that the semantics of {\\tt ignore} and {\\tt ignorenot} is a
     little counter-intuitive.  When detecting updates, Unison examines
     paths in depth-first order, starting from the roots of the replicas
     and working downwards.  Before examining each path, it checks whether
     it matches {\\tt ignore} and does not match {\\tt ignorenot}; in this case
     it skips this path {\\em and all its descendants}.  This means that,
     if some parent of a given path matches an {\\tt ignore} pattern, then
     it will be skipped even if the path itself matches an {\\tt ignorenot}
     pattern.  In particular, putting {\\tt ignore = Path *} in your profile
     and then using {\\tt ignorenot} to select particular paths to be
     synchronized will not work.  Instead, you should use the {\\tt path}
     preference to choose particular paths to synchronize.",
);

/// `atomic`.
pub static ATOMIC: Pred = Pred::new(
    "atomic",
    Category::Advanced(Topic::Sync),
    "add a pattern to the atomic list",
    "This preference specifies paths for directories whose \
     contents will be considered as a group rather than individually when \
     they are both modified.  \
     The backups are also made atomically in this case.  The option \
     \\texttt{backupcurr} however has no effect on atomic directories.",
)
.local();

/// `merge`.
pub static MERGE: Pred = Pred::new(
    "merge",
    Category::Advanced(Topic::Sync),
    "add a pattern to the merge list",
    "This preference can be used to run a merge program which will create \
     a new version for each of the files and the backup, \
     with the last backup and both replicas. \
     The syntax of \\ARG{pathspec -> cmd} is \
     described in \\sectionref{pathspec}{Path Specification}, and further \
     details on Merging functions are present in \
     \\sectionref{merge}{Merging Conflicting Versions}.",
);

/// `someHostIsRunningWindows`.
pub static SOME_HOST_IS_RUNNING_WINDOWS: PrefDef<bool> = PrefDef::new(
    "someHostIsRunningWindows",
    Category::Internal(Internal::Pseudo),
    || false,
    "*",
    "",
);

/// `allHostsAreRunningWindows`.
pub static ALL_HOSTS_ARE_RUNNING_WINDOWS: PrefDef<bool> = PrefDef::new(
    "allHostsAreRunningWindows",
    Category::Internal(Internal::Pseudo),
    || false,
    "*",
    "",
);

/// `fat`.
pub static FAT_FILESYSTEM: PrefDef<bool> = PrefDef::new(
    "fat",
    Category::Advanced(Topic::Syncprocess),
    || false,
    "use appropriate options for FAT filesystems",
    "When this is set to {\\tt true}, Unison will use appropriate options \
     to synchronize efficiently and without error a replica located on a \
     FAT filesystem on a non-Windows machine: \
     do not synchronize permissions ({\\tt perms = 0}); \
     never use chmod ({\\tt dontchmod = true}); \
     treat filenames as case insensitive ({\\tt ignorecase = true}); \
     do not attempt to synchronize symbolic links ({\\tt links = false}); \
     ignore inode number changes when detecting updates \
     ({\\tt ignoreinodenumbers = true}).  \
     Any of these change can be overridden by explicitly setting \
     the corresponding preference in the profile.",
)
.local();

/// Registers this module's preferences, in OCaml's order.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&ROOT)?;
    prefs.register(&PATHS)?;
    prefs.register(&BATCH)?;
    prefs.register(&CONFIRM_BIG_DELETES)?;
    prefs.alias(&CONFIRM_BIG_DELETES, "confirmbigdeletes")?;
    IGNORE.register(prefs)?;
    IGNORE_NOT.register(prefs)?;
    ATOMIC.register(prefs)?;
    MERGE.register(prefs)?;
    prefs.register(&SOME_HOST_IS_RUNNING_WINDOWS)?;
    prefs.register(&ALL_HOSTS_ARE_RUNNING_WINDOWS)?;
    prefs.register(&FAT_FILESYSTEM)
}

/// `ignore` and `ignorenot` compiled together, for update detection.
#[derive(Debug, Clone)]
pub struct Ignore {
    ignore: std::sync::Arc<Compiled>,
    ignore_not: std::sync::Arc<Compiled>,
}

impl Ignore {
    /// The predicate for the current patterns and case mode.
    pub fn new(prefs: &Prefs, mode: Mode) -> Result<Ignore, Error> {
        Ok(Ignore {
            ignore: IGNORE.compiled(prefs, mode)?,
            ignore_not: IGNORE_NOT.compiled(prefs, mode)?,
        })
    }

    /// `Globals.shouldIgnore`: matches `ignore` and not `ignorenot`.
    pub fn test<S>(&self, p: &Path<S>) -> bool {
        self.ignore.test(p.as_bytes()) && !self.ignore_not.test(p.as_bytes())
    }
}

/// `Globals.shouldIgnore`.
pub fn should_ignore<S>(prefs: &Prefs, mode: Mode, p: &Path<S>) -> Result<bool, Error> {
    Ok(Ignore::new(prefs, mode)?.test(p))
}

/// `Globals.addRegexpToIgnore`: adds a pattern, first.
pub fn add_regexp_to_ignore(prefs: &mut Prefs, re: &str) {
    let mut patterns = vec![re.to_owned()];
    patterns.extend(IGNORE.patterns(prefs));
    IGNORE.intern(prefs, patterns);
}

/// `Globals.shouldMerge`.
pub fn should_merge(prefs: &Prefs, mode: Mode, p: &Path<Global>) -> Result<bool, Error> {
    MERGE.test(prefs, mode, p.as_bytes())
}

/// `Globals.mergeCmdForPath`: the merge command for a path, if any.
pub fn merge_cmd_for_path(
    prefs: &Prefs,
    mode: Mode,
    p: &Path<Global>,
) -> Result<Option<String>, Error> {
    Ok(MERGE
        .compiled(prefs, mode)?
        .assoc(p.as_bytes())
        .map(str::to_owned))
}
