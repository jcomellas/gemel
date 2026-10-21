//! The startup sequence (`uicommon`'s profiles, `initPrefs` and
//! `connectRoots`), and whole syncs from a profile, checked against OCaml
//! Unison when `GEMEL_REFERENCE_UNISON` names it.

#[path = "../../gemel-engine/tests/support/mod.rs"]
mod support;

use std::cell::{Cell, RefCell};
use std::path::{Path as StdPath, PathBuf};
use std::rc::Rc;

use gemel_base::trace::Trace;
use gemel_base::util::{Error, FileId};
use gemel_engine::transport::{self, Task};
use gemel_engine::{archive, common, globals, recon, update};
use gemel_ui_api::uicommon::Format;
use gemel_ui_api::uicommon::startup::{ProfileInfo, Ui, client_rpc};
use support::sync::{Output, T0};
use support::{TempDir, block_on, listing, settle, write};

/// Replicas `a` and `b` in a directory, with the Unison directory `u`.
struct Pair {
    dir: TempDir,
}

impl Pair {
    fn new(tag: &str) -> Pair {
        let dir = TempDir::new(tag);
        for d in ["a", "b", "u"] {
            std::fs::create_dir_all(dir.0.join(d)).unwrap();
        }
        Pair { dir }
    }

    fn base(&self) -> &StdPath {
        &self.dir.0
    }

    fn a(&self) -> PathBuf {
        self.base().join("a")
    }

    fn b(&self) -> PathBuf {
        self.base().join("b")
    }

    fn u(&self) -> PathBuf {
        self.base().join("u")
    }

    fn root(&self, side: &str) -> String {
        self.base().join(side).to_str().unwrap().to_owned()
    }

    /// Writes the profile `name`, with this pair's roots and `lines`.
    fn profile(&self, name: &str, lines: &[&str]) {
        let mut text = format!("root = {}\nroot = {}\n", self.root("a"), self.root("b"));
        for l in lines {
            text += l;
            text.push('\n');
        }
        std::fs::write(self.u().join(format!("{name}.prf")), text).unwrap();
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.u().join("unison.log")).unwrap_or_default()
    }
}

/// The client for `pair`, its output, and the warnings it shows.
fn new_ui(pair: &Pair, args: &[&str], loopback: bool) -> (Ui, Output, Rc<RefCell<Vec<String>>>) {
    let output = Output::default();
    let trace = Trace::with_sinks(Box::new(output.clone()), Box::new(output.clone()));
    let rpc = client_rpc(&pair.u(), trace, loopback).unwrap();
    let warnings = Rc::new(RefCell::new(Vec::new()));
    let w = warnings.clone();
    rpc.client_host()
        .set_warn_printer(Some(Rc::new(move |s: &str| {
            w.borrow_mut().push(s.to_owned())
        })));
    let ui = Ui::new(rpc, args.iter().map(|s| s.to_string()).collect());
    (ui, output, warnings)
}

/// `Uitext.synchronizeOnce` in batch mode: finds the updates, reconciles
/// them (keeping errors deep in a directory in its item), and propagates
/// everything but conflicts and problems, deletions last, then commits.
/// The items' details (`details2string`, which OCaml's batch mode prints).
async fn sync(ui: &Ui) -> Result<Vec<String>, Error> {
    let (rpc, roots) = (ui.rpc(), ui.roots().expect("connected"));
    let updates = update::find_updates(rpc, roots, false, None).await?;
    let (items, _, _) = recon::reconcile_all(rpc, roots, true, updates).await?;
    let fmt = Format::of(rpc);
    let details = items
        .iter()
        .map(|ri| fmt.details(roots.pair(), ri, "  "))
        .collect();
    let found = archive::found_archives(rpc);
    if !found {
        update::commit_updates(rpc, roots).await?;
    }
    if items.is_empty() {
        if found {
            update::commit_updates(rpc, roots).await?;
        }
        return Ok(details);
    }
    if items.iter().all(common::problematic) {
        update::commit_updates(rpc, roots).await?;
        return Ok(details);
    }
    transport::log_start(rpc)?;
    for deletions in [false, true] {
        transport::transport_items(
            rpc,
            roots,
            &items,
            |ri| common::is_deletion(ri) == deletions,
            |i, ri| -> Task<'_, Result<(), Error>> {
                Box::pin(async move {
                    let r =
                        transport::transport_item(rpc, roots, ri, FileId(i as i64), &mut |_, _| {
                            true
                        })
                        .await;
                    match r {
                        Err(Error::Transient(_)) => Ok(()),
                        r => r,
                    }
                })
            },
        )
        .await?;
    }
    transport::log_finish(rpc)?;
    update::commit_updates(rpc, roots).await?;
    Ok(details)
}

/// `gemel <args>` in batch mode on `pair`: the items' details.
fn gemel(pair: &Pair, args: &[&str], loopback: bool) -> Vec<String> {
    let (mut ui, _, _) = new_ui(pair, args, loopback);
    block_on(async {
        let profile = ui
            .ui_init_cl_roots_and_profile()
            .map_err(Error::Fatal)?
            .expect("a profile");
        ui.init_prefs(&profile, async || None).await?;
        ui.connect_roots(&mut || {}).await?;
        sync(&ui).await
    })
    .unwrap()
}

/// OCaml Unison, run as `unison <args> -batch -ui text` on `pair`: its
/// output. `None` without `GEMEL_REFERENCE_UNISON`.
fn ocaml(pair: &Pair, args: &[&str]) -> Option<String> {
    let unison = std::env::var_os("GEMEL_REFERENCE_UNISON")?;
    let out = std::process::Command::new(unison)
        .args(args)
        .args(["-batch", "-ui", "text"])
        .env("UNISON", pair.u())
        .env("HOME", pair.base())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    Some(String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr))
}

/// The log, with the pair's directory shown as `S`, the times left out,
/// and without what the text interface logs (its summaries, phase 8).
fn startup_log(pair: &Pair) -> Vec<String> {
    let canonical = std::fs::canonicalize(pair.base()).unwrap();
    let names = [
        canonical.to_str().unwrap().to_owned(),
        pair.base().to_str().unwrap().to_owned(),
    ];
    let mut out: Vec<String> = Vec::new();
    for line in pair.log().lines() {
        let mut line = line.to_owned();
        for n in &names {
            line = line.replace(n, "S");
        }
        if line.contains(" log started at ") {
            out.push("(log started)".to_owned());
        } else if line.contains(" started propagating changes at ") {
            out.push("(start)".to_owned());
        } else if line.contains(" finished propagating changes at ") {
            out.push("(finish)".to_owned());
        } else if line.contains(" items will be synced, ") {
            // And the blank line before it.
            assert_eq!(out.pop().as_deref(), Some(""));
        } else if !(line.contains(" to be synced from ")
            || line.starts_with("Synchronization complete at ")
            || line.starts_with("  skipped: "))
        {
            out.push(line);
        }
    }
    out
}

const NOTHING_TO_DO: &str = "Nothing to do: replicas have not changed since last sync.";

/// Syncs a pair with gemel, and with OCaml a twin pair, from the profile
/// `p` (with `lines`): after `build` on both replicas, then after
/// `change`. Both must leave the same replicas and logs, and each item's
/// details must be OCaml's. Then OCaml finds nothing to do on gemel's
/// pair, so its archives agree with the replicas.
fn sync_like_ocaml(lines: &[&str], build: fn(&StdPath, &str), change: fn(&StdPath, &str)) {
    for loopback in [false, true] {
        let (g, o) = (Pair::new("ui-gemel"), Pair::new("ui-ocaml"));
        for p in [&g, &o] {
            p.profile("p", lines);
            build(&p.a(), "a");
            build(&p.b(), "b");
        }
        for round in 0..2 {
            if round == 1 {
                for p in [&g, &o] {
                    change(&p.a(), "a");
                    change(&p.b(), "b");
                }
            }
            // The same directory times in both pairs.
            settle(&[&g.a(), &g.b(), &o.a(), &o.b()]);
            let details = gemel(&g, &["p", "-batch"], loopback);
            let Some(out) = ocaml(&o, &["p"]) else {
                continue;
            };
            let what = format!("loopback {loopback}, round {round}");
            for d in &details {
                assert!(out.contains(d), "{what}: {d:?} not in\n{out}");
            }
            assert_eq!(listing(&g.a()), listing(&o.a()), "{what}");
            assert_eq!(listing(&g.b()), listing(&o.b()), "{what}");
            assert_eq!(startup_log(&g), startup_log(&o), "{what}");
        }
        for fastcheck in ["true", "false"] {
            let flag = format!("-fastcheck={fastcheck}");
            if let Some(out) = ocaml(&g, &["p", &flag]) {
                assert!(out.contains(NOTHING_TO_DO), "{fastcheck}: {out}");
            }
        }
    }
}

#[test]
fn a_profile_syncs_like_ocaml() {
    sync_like_ocaml(
        &["label = a test", "ignore = Name *.tmp", "times = true"],
        |r, side| {
            write(r, "same", "same", T0);
            write(r, "x.tmp", side, T0);
            if side == "a" {
                write(r, "f", "from a", T0 + 1.0);
                write(r, "sub/g", "deep", T0 + 2.0);
            } else {
                write(r, "h", "from b", T0 + 3.0);
            }
        },
        |r, side| {
            if side == "a" {
                std::fs::remove_file(r.join("f")).unwrap();
                write(r, "sub/new", "new in a", T0 + 4.0);
            } else {
                write(r, "h", "changed in b", T0 + 5.0);
                write(r, "dir/k", "in a new dir", T0 + 6.0);
            }
        },
    );
}

#[test]
fn wildcard_paths_sync_like_ocaml() {
    // `top/*` is the children of `top` in the first root, when the paths
    // are expanded: `z`, only in `b`, isn't synced.
    sync_like_ocaml(
        &["path = top/*", "times = true"],
        |r, side| {
            write(r, "outside", side, T0);
            if side == "a" {
                write(r, "top/x", "x", T0 + 1.0);
                write(r, "top/y/deep", "y", T0 + 2.0);
            } else {
                write(r, "top/z", "z", T0 + 3.0);
            }
        },
        |r, side| {
            if side == "b" {
                write(r, "top/x", "x changed", T0 + 4.0);
            }
        },
    );
}

#[test]
fn command_line_roots_use_the_default_profile() {
    let pair = Pair::new("ui-cl-roots");
    let (a, b) = (pair.root("a"), pair.root("b"));
    let (mut ui, _, _) = new_ui(&pair, &[&a, &b], false);
    assert_eq!(
        ui.ui_init_cl_roots_and_profile(),
        Ok(Some("default".to_owned()))
    );
    block_on(ui.init_prefs("default", async || panic!("roots were given"))).unwrap();
    let prefs = ui.rpc().client_host().prefs().borrow();
    assert_eq!(globals::raw_roots(&prefs), [a.clone(), b.clone()]);
    // The profile is made, empty. (Lines are added after a newline.)
    assert_eq!(
        std::fs::read_to_string(pair.u().join("default.prf")).unwrap(),
        "\n# Unison preferences file"
    );
    drop(prefs);

    // A profile, then the roots.
    std::fs::write(pair.u().join("p.prf"), "label = mine\n").unwrap();
    let (mut ui, _, _) = new_ui(&pair, &["p", &b, &a], false);
    assert_eq!(ui.ui_init_cl_roots_and_profile(), Ok(Some("p".to_owned())));
    block_on(ui.init_prefs("p", async || None)).unwrap();
    let prefs = ui.rpc().client_host().prefs().borrow();
    assert_eq!(globals::raw_roots(&prefs), [b.clone(), a.clone()]);
    assert_eq!(prefs.read(&gemel_ui_api::uicommon::PROFILE_LABEL), "mine");
    drop(prefs);

    // Roots in both are too many, as in OCaml.
    pair.profile("p", &[]);
    let (mut ui, _, _) = new_ui(&pair, &["p", &b, &a], false);
    ui.ui_init_cl_roots_and_profile().unwrap();
    let e = block_on(async {
        ui.init_prefs("p", async || None).await?;
        ui.connect_roots(&mut || {}).await
    })
    .unwrap_err()
    .to_string();
    assert!(
        e.starts_with("Wrong number of roots: 2 expected, but 4 provided ("),
        "{e}"
    );
    assert!(e.contains(&format!("({b}, {a}, {a}, {b})")), "{e}");

    let (mut ui, _, _) = new_ui(&pair, &["p", "x", "y", "z"], false);
    assert_eq!(
        ui.ui_init_cl_roots_and_profile(),
        Err("gemel was invoked incorrectly (too many roots)".to_owned())
    );
    let (mut ui, _, _) = new_ui(&pair, &["missing"], false);
    let e = ui.ui_init_cl_roots_and_profile().unwrap_err();
    assert!(
        e.starts_with("Profile 'missing' does not exist (looking for file "),
        "{e}"
    );
    let (mut ui, _, _) = new_ui(&pair, &[], false);
    assert_eq!(ui.ui_init_cl_roots_and_profile(), Ok(None));
}

#[test]
fn roots_asked_for_are_added_to_the_profile() {
    let pair = Pair::new("ui-prompt");
    std::fs::write(pair.u().join("p.prf"), "label = no roots\n").unwrap();
    let (a, b) = (pair.root("a"), pair.root("b"));
    let (mut ui, _, _) = new_ui(&pair, &["p"], false);
    let asked = Cell::new(false);
    block_on(ui.init_prefs("p", async || {
        asked.set(true);
        Some((a.clone(), b.clone()))
    }))
    .unwrap();
    assert!(asked.get());
    assert_eq!(
        std::fs::read_to_string(pair.u().join("p.prf")).unwrap(),
        format!("label = no roots\n\nroot = {a}\nroot = {b}")
    );

    std::fs::write(pair.u().join("q.prf"), "").unwrap();
    let (mut ui, _, _) = new_ui(&pair, &["q"], false);
    assert_eq!(
        block_on(ui.init_prefs("q", async || None)),
        Err(Error::Fatal(
            "no roots given on command line or in profile".to_owned()
        ))
    );
}

#[test]
fn roots_are_installed_once_per_profile() {
    let pair = Pair::new("ui-connect");
    pair.profile("p", &[]);
    let (mut ui, _, _) = new_ui(&pair, &["p"], false);
    let waits = Cell::new(0);
    let mut wait = || waits.set(waits.get() + 1);
    block_on(async {
        // Nothing to connect before the profile is loaded.
        ui.connect_roots(&mut wait).await.unwrap();
        assert!(ui.roots().is_none());
        ui.init_prefs("p", async || None).await.unwrap();
        ui.connect_roots(&mut wait).await.unwrap();
        assert!(ui.roots().is_some());
        ui.connect_roots(&mut wait).await.unwrap();
        assert_eq!(waits.get(), 1);
        // Loading the profile again uninstalls them.
        ui.init_prefs("p", async || None).await.unwrap();
        assert!(ui.roots().is_none());
        ui.connect_roots(&mut wait).await.unwrap();
        assert_eq!(waits.get(), 2);
    });
    // Unless `contactquietly`.
    let (mut ui, _, _) = new_ui(&pair, &["p", "-contactquietly"], false);
    block_on(async {
        ui.init_prefs("p", async || None).await.unwrap();
        ui.connect_roots(&mut wait).await.unwrap();
    });
    assert_eq!(waits.get(), 2);
}

#[test]
fn the_log_starts_with_the_roots() {
    let pair = Pair::new("ui-log");
    pair.profile("p", &[]);
    let (mut ui, _, _) = new_ui(&pair, &["p"], false);
    block_on(ui.init_prefs("p", async || None)).unwrap();
    let log = pair.log();
    let (a, b) = (pair.root("a"), pair.root("b"));
    let started = format!(
        "\n{} log started at ",
        gemel_base::util::capitalize_ascii(gemel_base::util::MY_NAME_AND_VERSION)
    );
    assert!(log.starts_with(&started), "{log:?}");
    assert!(
        log.ends_with(&format!("\n\nRoots:\n  {a}\n  {b}\n\n")),
        "{log:?}"
    );
}

#[test]
fn profiles_are_listed_with_their_keys() {
    let pair = Pair::new("ui-profiles");
    let u = pair.u();
    std::fs::write(
        u.join("one.prf"),
        "root = /x\nroot = /y\nlabel = first\nkey = 1\n",
    )
    .unwrap();
    std::fs::write(u.join("two.prf"), "root = /z\n").unwrap();
    std::fs::write(u.join("bad.prf"), "no equals sign\n").unwrap();
    std::fs::write(u.join(".#one.prf"), "key = 1\n").unwrap();
    std::fs::write(u.join("other.txt"), "key = 1\n").unwrap();
    let (mut ui, _, warnings) = new_ui(&pair, &[], false);
    ui.scan_profiles().unwrap();
    let one = ProfileInfo {
        roots: vec!["/x".to_owned(), "/y".to_owned()],
        label: Some("first".to_owned()),
        key: Some("1".to_owned()),
    };
    let two = ProfileInfo {
        roots: vec!["/z".to_owned()],
        label: None,
        key: None,
    };
    assert_eq!(
        ui.profiles_and_roots,
        [("one".to_owned(), one.clone()), ("two".to_owned(), two)]
    );
    assert_eq!(ui.profile_keymap[1], Some(("one".to_owned(), one)));
    let w = warnings.borrow();
    assert_eq!(w.len(), 1);
    assert!(
        w[0].starts_with("Error when reading list of profiles.\nSkipping file with error: "),
        "{w:?}"
    );
    drop(w);

    // Two profiles can't have the same key, and a key is a digit.
    std::fs::write(u.join("three.prf"), "key = 1\n").unwrap();
    let e = ui.scan_profiles().unwrap_err().to_string();
    assert!(
        e.ends_with("shortcut key 1 is already bound to profile one"),
        "{e}"
    );
    std::fs::write(u.join("three.prf"), "key = 12\n").unwrap();
    let e = ui.scan_profiles().unwrap_err().to_string();
    assert!(e.ends_with("must be a single digit (0-9), not 12"), "{e}");
}

/// The items a scan of `ui`'s roots finds, by path.
async fn items(ui: &Ui) -> Vec<gemel_engine::common::ReconItem> {
    let (rpc, roots) = (ui.rpc(), ui.roots().unwrap());
    let updates = update::find_updates(rpc, roots, false, None).await.unwrap();
    recon::reconcile_all(rpc, roots, true, updates)
        .await
        .unwrap()
        .0
}

fn path_of(ri: &gemel_engine::common::ReconItem) -> String {
    gemel_ui_api::uicommon::path_string(&ri.path1)
}

#[test]
fn an_ignored_path_is_added_to_the_profile_and_sent_to_the_servers() {
    use gemel_ui_api::uicommon::{add_ignore_pattern, ignore_path};
    let pair = Pair::new("ui-ignore");
    pair.profile("p", &[]);
    write(&pair.a(), "x", "x", T0);
    write(&pair.a(), "y", "y", T0);
    // In loopback mode, the root's host scans with the preferences it was
    // sent.
    let (mut ui, output, _) = new_ui(&pair, &["p"], true);
    block_on(async {
        ui.init_prefs("p", async || None).await.unwrap();
        ui.connect_roots(&mut || {}).await.unwrap();
        let found: Vec<String> = items(&ui).await.iter().map(path_of).collect();
        assert_eq!(found, ["x", "y"]);
        let x = gemel_base::path::Path::from_bytes(b"x").unwrap();
        let (rpc, roots) = (ui.rpc(), ui.roots().unwrap());
        add_ignore_pattern(rpc, roots, &ignore_path(&x))
            .await
            .unwrap();
        let found: Vec<String> = items(&ui).await.iter().map(path_of).collect();
        assert_eq!(found, ["y"]);
        assert_eq!(
            add_ignore_pattern(rpc, roots, "Path ").await,
            Err(Error::Transient("Can't ignore the root path!".to_owned()))
        );
    });
    let profile = std::fs::read_to_string(pair.u().join("p.prf")).unwrap();
    assert!(profile.ends_with("\nignore = Path {x}"), "{profile:?}");
    assert!(
        output
            .take()
            .contains("ignore = Path {x}' added to profile ")
    );
}

#[cfg(unix)]
#[test]
fn diffs_are_of_where_the_files_are() {
    use gemel_engine::common::{ReconItem, Replicas, Status};
    use gemel_ui_api::uicommon::show_diffs;
    let pair = Pair::new("ui-diff");
    // `echo` shows the files it is given.
    pair.profile("p", &["moves-experimental = true", "diff = echo"]);
    for r in [pair.a(), pair.b()] {
        write(&r, "README", "read me", T0);
        write(&r, "d/f", "f", T0);
    }
    settle(&[&pair.a(), &pair.b()]);
    gemel(&pair, &["p", "-batch"], false);
    std::fs::rename(pair.a().join("README"), pair.a().join("READ-ME")).unwrap();
    write(&pair.b(), "README", "b's version", T0 + 30.0);
    std::fs::remove_dir_all(pair.a().join("d")).unwrap();
    write(&pair.a(), "d", "now a file", T0 + 40.0);
    settle(&[&pair.a(), &pair.b()]);
    let (mut ui, _, _) = new_ui(&pair, &["p"], false);
    let canonical = |p: PathBuf| std::fs::canonicalize(p).unwrap();
    let (a, b) = (canonical(pair.a()), canonical(pair.b()));
    block_on(async {
        ui.init_prefs("p", async || None).await.unwrap();
        ui.connect_roots(&mut || {}).await.unwrap();
        let (rpc, roots) = (ui.rpc(), ui.roots().unwrap());
        let found = items(&ui).await;
        let diff = async |ri: &ReconItem| {
            let (mut shown, mut errors) = (Vec::new(), Vec::new());
            show_diffs(
                rpc,
                roots,
                ri,
                &mut |_, text| shown.push(text.to_owned()),
                &mut |e| errors.push(e.to_owned()),
                FileId(0),
            )
            .await
            .unwrap();
            (shown, errors)
        };
        let moved = found.iter().find(|ri| path_of(ri) == "README").unwrap();
        let Replicas::Different(d) = &moved.replicas else {
            panic!("{moved:?}")
        };
        assert!(matches!(d.rc1.status, Status::MovedOut(..)), "{moved:?}");
        let files = format!(
            "{} {}",
            a.join("READ-ME").display(),
            b.join("README").display()
        );
        assert_eq!(diff(moved).await, (vec![files], vec![]));

        let dir = found.iter().find(|ri| path_of(ri) == "d").unwrap();
        assert_eq!(
            diff(dir).await,
            (
                vec![],
                vec!["Can't diff: path doesn't refer to a file in both replicas".to_owned()]
            )
        );
        let problem = ReconItem {
            replicas: Replicas::Problem("bad".to_owned()),
            ..dir.clone()
        };
        assert_eq!(
            diff(&problem).await,
            (
                vec![],
                vec!["Can't diff files: there was a problem during update detection".to_owned()]
            )
        );
    });
}
