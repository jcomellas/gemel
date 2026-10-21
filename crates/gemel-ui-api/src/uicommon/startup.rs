//! Choosing a profile, loading it, and setting up the roots.
//!
//! Ports the startup half of Unison `src/uicommon.ml`: `scanProfiles`,
//! `uiInitClRootsAndProfile`, `initPrefs`, `initRoots`, `connectRoots` and
//! `validateAndFixupPrefs`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! OCaml keeps the installed roots and what the command line gave in
//! globals; here they are in [`Ui`], with the client's [`Rpc`]. Not
//! ported: the self-tests (`selftest`) and turning on GC messages.

use std::cell::RefCell;
use std::path::Path as StdPath;
use std::rc::Rc;

use gemel_base::case;
use gemel_base::clroot::Clroot;
use gemel_base::path::Path;
use gemel_base::prefs::{BoolWithDefault, CmdLineError};
use gemel_base::trace::Trace;
use gemel_base::util::{self, Error};
use gemel_engine::archive;
use gemel_engine::files;
use gemel_engine::fswatch;
use gemel_engine::globals::{self, Roots};
use gemel_engine::recon;
use gemel_engine::stasher;
use gemel_engine::with_trace;
use gemel_engine::xferhint;
use gemel_fs::fileinfo::{self, FsConfig};
use gemel_fs::os::{self, TempNames};
use gemel_fs::props::{self, PropsConfig};
use gemel_fs::{osx, system};
use gemel_rpc::host::Host;
use gemel_rpc::registry::{Dispatch, RootCmd, Rpc};
use gemel_rpc::root::Root;

use super::{CONTACT_QUIETLY, PROFILE_KEY, PROFILE_LABEL, REPEAT, RUN_TESTS, RUN_TESTS_PREF_NAME};

// ---- The hosts ----

/// `architecture`: whether the root's host runs Windows, macOS and Cygwin
/// (never, here).
pub static ARCHITECTURE: RootCmd<(), (bool, bool, bool)> = RootCmd::new(
    "architecture",
    |(), _, _| {},
    |_, _| Ok(()),
    |(win, osx, cygwin), w, _| {
        w.bool(*win);
        w.bool(*osx);
        w.bool(*cygwin);
    },
    |r, _| Ok((r.bool()?, r.bool()?, r.bool()?)),
    |_, _, ()| Box::pin(async { Ok((cfg!(windows), cfg!(target_os = "macos"), false)) }),
);

/// The client's [`Rpc`], with local roots, `unison_dir` as the Unison
/// directory and `trace` as its trace. With `loopback`, root commands run
/// on a separate host, as on a server (see `gemel_rpc::registry`).
pub fn client_rpc(unison_dir: &StdPath, trace: Trace, loopback: bool) -> Result<Rpc, Error> {
    let mut prefs = crate::new_prefs()?;
    prefs.set_unison_dir(unison_dir);
    let client = Rc::new(Host::client(
        Rc::new(RefCell::new(prefs)),
        Rc::new(RefCell::new(crate::new_features()?)),
    ));
    client.set_unison_dir(unison_dir);
    // Before anything logs: a relative `logfile` is in the Unison directory.
    let mut trace = trace;
    trace.set_unison_dir(unison_dir);
    *client.state::<Trace>().borrow_mut() = trace;
    let dispatch = if loopback {
        let server = Host::loopback_server(
            &client.prefs().borrow(),
            &client.features().borrow(),
            crate::new_prefs()?,
            crate::new_features()?,
        )?;
        server.set_unison_dir(unison_dir);
        Dispatch::Loopback {
            client,
            server: Rc::new(server),
        }
    } else {
        Dispatch::Direct(client)
    };
    let mut rpc = Rpc::new(dispatch);
    crate::register_commands(&mut rpc)?;
    Ok(rpc)
}

/// `validateAndFixupPrefs`: the preferences that depend on the hosts.
/// If a host is Windows or macOS, names are compared without case; on
/// macOS, resource forks are synchronised; on Windows, only the write
/// permission is.
pub async fn validate_and_fixup_prefs(rpc: &Rpc, roots: &Roots) -> Result<(), Error> {
    PropsConfig::validate(&rpc.client_host().prefs().borrow())?;
    let archs = roots
        .map(|r: &Root| rpc.call_root(&ARCHITECTURE, r, ()))
        .await?;
    let some_win = archs.iter().any(|a| a.0);
    let all_win = archs.iter().all(|a| a.0);
    let some_cygwin = archs.iter().any(|a| a.2);
    let some_osx = archs.iter().any(|a| a.1);
    let some_insensitive = some_win || some_osx || some_cygwin;
    let mut prefs = rpc.client_host().prefs().borrow_mut();
    if *prefs.read(&globals::FAT_FILESYSTEM) {
        prefs.override_default(&props::PERMS, 0);
        prefs.override_default(&props::DONT_CHMOD, true);
        prefs.override_default(&case::IGNORE_CASE, BoolWithDefault::True);
        prefs.override_default(&fileinfo::ALLOW_SYMLINKS, BoolWithDefault::False);
        prefs.override_default(&fileinfo::IGNORE_INODE_NUMBERS, true);
    }
    case::init(&mut prefs, some_insensitive, some_osx);
    props::init(&mut prefs, some_win || some_cygwin);
    osx::init(&mut prefs, some_osx);
    fileinfo::init(&mut prefs, some_win);
    prefs.set(&globals::SOME_HOST_IS_RUNNING_WINDOWS, some_win);
    prefs.set(&globals::ALL_HOSTS_ARE_RUNNING_WINDOWS, all_win);
    if prefs.read(&REPEAT).watcher() {
        prefs.set(&fswatch::USE_WATCHER, true);
    }
    rpc.client_host()
        .features()
        .borrow()
        .validate_enabled(&prefs)
}

// ---- Profiles ----

/// `profileInfo`: what the list of profiles shows of each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileInfo {
    /// Its `root` settings.
    pub roots: Vec<String>,
    /// Its `label`.
    pub label: Option<String>,
    /// Its `key`.
    pub key: Option<String>,
}

// ---- The session's startup ----

/// The client and what `uicommon` keeps between calls: the roots from the
/// command line, the installed roots, and the profiles found.
pub struct Ui {
    rpc: Rpc,
    /// The command line, without the program's name.
    args: Vec<String>,
    /// `cmdLineRawRoots`.
    cmd_line_raw_roots: Vec<String>,
    /// `Globals.roots`, once `initRoots` has installed them.
    roots: Option<Roots>,
    /// `initComplete`.
    init_complete: bool,
    /// `profileKeymap`: the profiles with a `key`, by digit.
    pub profile_keymap: [Option<(String, ProfileInfo)>; 10],
    /// `profilesAndRoots`: every profile, after [`Ui::scan_profiles`].
    pub profiles_and_roots: Vec<(String, ProfileInfo)>,
}

impl Ui {
    /// A user interface's state, for the client `rpc` and the command line
    /// `args` (without the program's name).
    pub fn new(rpc: Rpc, args: Vec<String>) -> Ui {
        Ui {
            rpc,
            args,
            cmd_line_raw_roots: Vec::new(),
            roots: None,
            init_complete: false,
            profile_keymap: Default::default(),
            profiles_and_roots: Vec::new(),
        }
    }

    /// The client.
    pub fn rpc(&self) -> &Rpc {
        &self.rpc
    }

    /// The installed roots (`Globals.roots`), after
    /// [`Ui::connect_roots`].
    pub fn roots(&self) -> Option<&Roots> {
        self.roots.as_ref()
    }

    /// The command line.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    fn unison_dir(&self) -> std::path::PathBuf {
        self.rpc.client_host().unison_dir()
    }

    fn log_only(&self, s: &str) -> Result<(), Error> {
        with_trace(&self.rpc, |t, p| t.log_only(p, s))
    }

    /// The command line's settings, by preference name.
    fn scan_cmd_line(&self) -> Result<std::collections::BTreeMap<String, Vec<String>>, Error> {
        self.rpc
            .client_host()
            .prefs()
            .borrow()
            .scan_cmd_line(&self.args)
            .map_err(|e| Error::Fatal(e.to_string()))
    }

    /// `provideProfileKey`.
    fn provide_profile_key(
        &mut self,
        filename: &str,
        k: &str,
        profile: &str,
        info: &ProfileInfo,
    ) -> Result<(), Error> {
        let not_a_digit = || {
            Error::Fatal(format!(
                "Error scanning profile {filename}:\nValue of 'key' preference must be a \
                 single digit (0-9), not {k}"
            ))
        };
        let i = util::int_of_string(k).ok_or_else(not_a_digit)?;
        if !(0..=9).contains(&i) {
            return Err(not_a_digit());
        }
        match &self.profile_keymap[i as usize] {
            None => {
                self.profile_keymap[i as usize] = Some((profile.to_owned(), info.clone()));
                Ok(())
            }
            Some((other, _)) => Err(Error::Fatal(format!(
                "Error scanning profile {filename}:\nshortcut key {k} is already bound to \
                 profile {other}"
            ))),
        }
    }

    /// `scanProfiles`: finds the profiles in the Unison directory, with
    /// their roots, labels and keys. One that can't be read is skipped,
    /// with a warning; two with the same key are an error.
    pub fn scan_profiles(&mut self) -> Result<(), Error> {
        let dir = self.unison_dir();
        os::create_unison_dir(&dir)?;
        self.profile_keymap = Default::default();
        let files = files::ls(&dir.to_string_lossy(), "*.prf")?;
        let mut found = Vec::new();
        for f in files
            .iter()
            .filter(|n| !(n.starts_with(".#") || n.starts_with(os::TEMP_FILE_PREFIX)))
        {
            let f = f.strip_suffix(".prf").unwrap_or(f);
            let (filename, lines) = {
                let mut prefs = self.rpc.client_host().prefs().borrow_mut();
                let filename = prefs.profile_pathname(f, true)?.display().to_string();
                (filename, prefs.read_a_file(f, true, true))
            };
            let lines = match lines {
                Ok(lines) => lines,
                Err(Error::Fatal(s)) => {
                    self.rpc.client_host().warn(&format!(
                        "Error when reading list of profiles.\nSkipping file with error: \
                         {filename}\n\n{s}"
                    ));
                    continue;
                }
                Err(e) => return Err(e),
            };
            let assoc = |name: &str| {
                lines
                    .iter()
                    .find(|l| l.name == name)
                    .map(|l| l.value.clone())
            };
            let info = ProfileInfo {
                roots: lines
                    .iter()
                    .filter(|l| l.name == "root")
                    .map(|l| l.value.clone())
                    .collect(),
                label: assoc(PROFILE_LABEL.name()),
                key: assoc(PROFILE_KEY.name()),
            };
            if let Some(k) = &info.key {
                self.provide_profile_key(&filename, k, f, &info)?;
            }
            found.push((f.to_owned(), info));
        }
        self.profiles_and_roots = found;
        Ok(())
    }

    /// `uiInitClRootsAndProfile`: the profile to use, from the command line
    /// (`None` if the user should choose one), and the roots it gives. An
    /// error is a message for the user.
    pub fn ui_init_cl_roots_and_profile(&mut self) -> Result<Option<String>, String> {
        os::create_unison_dir(&self.unison_dir()).map_err(|e| e.to_string())?;
        let args = self.scan_cmd_line().map_err(|e| e.to_string())?;
        // Each list has the last setting first.
        let rest = args.get("rest").map(Vec::as_slice).unwrap_or_default();
        let clprofile = match rest {
            [] => None,
            [profile] => Some(profile.clone()),
            [root2, root1] => {
                self.cmd_line_raw_roots = vec![root1.clone(), root2.clone()];
                None
            }
            [root2, root1, profile] => {
                self.cmd_line_raw_roots = vec![root1.clone(), root2.clone()];
                Some(profile.clone())
            }
            _ => {
                return Err(format!(
                    "{} was invoked incorrectly (too many roots)",
                    util::MY_NAME
                ));
            }
        };
        match clprofile {
            // The user chooses a profile, or makes one.
            None if self.cmd_line_raw_roots.is_empty() => Ok(None),
            // Roots on the command line: the default profile.
            None => Ok(Some("default".to_owned())),
            Some(n) => {
                let f = self
                    .rpc
                    .client_host()
                    .prefs()
                    .borrow()
                    .profile_pathname(&n, true)
                    .map_err(|e| e.to_string())?;
                if f.exists() {
                    Ok(Some(n))
                } else {
                    Err(format!(
                        "Profile '{n}' does not exist (looking for file {})",
                        f.display()
                    ))
                }
            }
        }
    }

    /// `clearClRoots`: forgets the roots given on the command line.
    pub fn clear_cl_roots(&mut self) {
        self.cmd_line_raw_roots.clear();
    }

    /// `initPrefs`: loads the profile `profile_name` (made empty if it
    /// doesn't exist), then the command line over it. Without roots in
    /// either, asks for them with `prompt_for_roots` and adds them to the
    /// profile. Checks the roots, and syncs the whole replicas if no `path`
    /// is given.
    pub async fn init_prefs(
        &mut self,
        profile_name: &str,
        prompt_for_roots: impl AsyncFnOnce() -> Option<(String, String)>,
    ) -> Result<(), Error> {
        self.init_complete = false;
        // Roots left from another profile can't stay installed if this one
        // fails part way.
        self.roots = None;
        let test_flag_present = self.scan_cmd_line()?.contains_key(RUN_TESTS_PREF_NAME);
        let client = self.rpc.client_host().clone();
        {
            let mut prefs = client.prefs().borrow_mut();
            prefs.reset_to_defaults();
            globals::set_raw_roots(&mut prefs, &self.cmd_line_raw_roots);
            prefs.set_profile_name(Some(profile_name.to_owned()));
            // With `selftest`, no profile is read, so that it can't make the
            // tests fail.
            if !test_flag_present {
                // Only `default` can be missing: other names were checked.
                if !prefs.profile_pathname(profile_name, true)?.exists() {
                    prefs.add_comment("Unison preferences file")?;
                }
                prefs.load_the_file()?;
                if *prefs.read(&RUN_TESTS) {
                    return Err(Error::Fatal(
                        "The 'test' flag should only be given on the command line".to_owned(),
                    ));
                }
            }
            // The command line overrides the profile.
            prefs.parse_cmd_line(&self.args).map_err(|e| match e {
                CmdLineError::Fatal(e) => e,
                CmdLineError::Usage(s) => Error::Fatal(s.to_string()),
            })?;
            for w in prefs.take_warnings() {
                client.warn(&w);
            }
        }
        self.log_only(&format!(
            "\n{} log started at {}\n\n",
            util::capitalize_ascii(util::MY_NAME_AND_VERSION),
            system::time2string(system::gettimeofday())
        ))?;
        if globals::raw_roots(&client.prefs().borrow()).is_empty() {
            let Some((r1, r2)) = prompt_for_roots().await else {
                return Err(Error::Fatal(
                    "no roots given on command line or in profile".to_owned(),
                ));
            };
            let mut prefs = client.prefs().borrow_mut();
            // The first comes out on the left.
            globals::set_raw_roots(&mut prefs, &[r1.clone(), r2.clone()]);
            // And they are saved in the profile.
            prefs.add("root", &r1)?;
            prefs.add("root", &r2)?;
        }
        let raw = globals::raw_roots(&client.prefs().borrow());
        self.log_only("Roots:\n")?;
        for r in &raw {
            self.log_only(&format!("  {r}\n"))?;
        }
        self.log_only("\n")?;
        let mut prefs = client.prefs().borrow_mut();
        let parsed = globals::parsed_cl_raw_roots(&prefs)
            .map_err(|e| Error::Fatal(format!("There's a problem with one of the roots:\n{e}")))?;
        let num_remote = parsed
            .iter()
            .filter(|r| !matches!(r, Clroot::ConnectLocal(_)))
            .count();
        if num_remote > 1 {
            return Err(Error::Fatal(
                "cannot synchronize more than one remote root".to_owned(),
            ));
        }
        recon::check_that_preferred_root_is_valid(&prefs)?;
        // Between local roots, the table of copies isn't needed.
        if num_remote == 0 {
            prefs.set(&xferhint::XFER_BY_COPYING, false);
        }
        // Without paths, the whole replicas.
        if prefs.read(&globals::PATHS).is_empty() {
            prefs.set(&globals::PATHS, vec![Path::empty()]);
        }
        self.init_complete = true;
        Ok(())
    }

    /// `initRoots`: installs the roots (canonical, local first), recovers
    /// from an interrupted commit, expands the paths ending in `*`, sets
    /// the preferences that depend on the hosts and sends them to the
    /// servers. `display_wait_message` is shown first unless
    /// `contactquietly` or `terse`.
    async fn init_roots(&mut self, display_wait_message: &mut dyn FnMut()) -> Result<(), Error> {
        let rpc = &self.rpc;
        let client = rpc.client_host();
        {
            let prefs = client.prefs().borrow();
            if !(*prefs.read(&CONTACT_QUIETLY) || *prefs.read(&gemel_base::trace::TERSE)) {
                display_wait_message();
            }
        }
        let raw = globals::raw_roots(&client.prefs().borrow());
        let roots = Roots::install(rpc, &raw).await?;
        files::process_commit_logs(rpc, &roots).await?;
        archive::store_roots_name(rpc, &roots)?;
        let has_remote = !roots.in_canonical_order()[1].is_local();
        let quiet = {
            let prefs = client.prefs().borrow();
            *prefs.read(&CONTACT_QUIETLY) || *prefs.read(&gemel_base::trace::TERSE)
        };
        if has_remote && !quiet {
            let name = client
                .prefs()
                .borrow()
                .read(&archive::ROOTS_NAME)
                .replace(", ", " -> ");
            with_trace(rpc, |t, p| t.status(p, &format!("Connected [{name}]\n")))?;
        }
        with_trace(rpc, |t, p| {
            t.debug(p, "startup", || {
                let mut s = String::from("Roots: \n");
                for r in &raw {
                    s += &format!("        {r}\n");
                }
                s += "  i.e. \n";
                for r in &raw {
                    match gemel_base::clroot::parse_root(r) {
                        Ok(c) => s += &format!("        {c}\n"),
                        Err(e) => s += &format!("        {e}\n"),
                    }
                }
                s += "  i.e. (in canonical order)\n";
                for r in roots.in_canonical_order() {
                    s += &format!("       {r}\n");
                }
                s
            })
        });
        {
            let names = rpc.local_host().state::<TempNames>().borrow().clone();
            let c = FsConfig::from_prefs(&client.prefs().borrow());
            let children = |f: &gemel_fs::fspath::Fspath, p: &Path<gemel_base::path::Local>| {
                os::children_of(f, p, &names, &c)
            };
            globals::expand_wildcard_paths(&mut client.prefs().borrow_mut(), &roots, &children)?;
        }
        validate_and_fixup_prefs(rpc, &roots).await?;
        globals::propagate_prefs(rpc, &roots).await?;
        // After the preferences are sent, which would replace the servers'
        // patterns; and each time a profile is loaded.
        stasher::init_backups(rpc, &roots).await?;
        self.roots = Some(roots);
        Ok(())
    }

    /// `connectRoots`: installs the roots, after [`Ui::init_prefs`], unless
    /// they are already (and connected).
    pub async fn connect_roots(
        &mut self,
        display_wait_message: &mut dyn FnMut(),
    ) -> Result<(), Error> {
        let num_roots = globals::raw_roots(&self.rpc.client_host().prefs().borrow()).len();
        if !(self.init_complete && num_roots > 1) {
            return Ok(());
        }
        // Remote roots come with milestone 3; local ones are always
        // connected.
        let num_conn = self
            .roots
            .as_ref()
            .map_or(0, |r| r.list().iter().filter(|r| r.is_local()).count());
        if num_conn < num_roots {
            self.init_roots(display_wait_message).await?;
        }
        Ok(())
    }
}
