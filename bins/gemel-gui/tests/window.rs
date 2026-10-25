//! Drives the real window on Slint's headless testing backend with
//! synthetic input: on a real session, through `uigtk3`'s flows, and
//! without one, fed events, to check what is shown and the accessibility
//! properties that Slint's AccessKit bridge turns into what
//! VoiceOver/Narrator/AT-SPI see.

mod support;

use std::path::Path;
use std::time::Duration;

use gemel_gui::App;
use gemel_ui_api::session::{Command, Event, Progress, SetAction, Summary};
use gemel_ui_api::uicommon::Action;
use i_slint_backend_testing::{AccessibleRole, ElementQuery};
use slint::platform::Key;
use slint::{ComponentHandle, LogicalSize, Model};

use support::{click, fake_item, fake_updates, init, items, press, type_text, wait_until};

const WAIT: Duration = Duration::from_secs(60);

#[test]
fn items_are_virtualised_selectable_and_accessible() {
    init();
    let app = App::detached().unwrap();
    let ui = app.ui();
    ui.window().set_size(LogicalSize::new(1000.0, 700.0));
    ui.show().unwrap();
    let fake = (0..50_000).map(|i| {
        let action = if i % 7 == 3 {
            Action::Skip(false)
        } else {
            Action::LtoR(false)
        };
        fake_item(&format!("dir/file-{i:05}"), action)
    });
    app.handle(Event::Updates(fake_updates(fake.collect())));

    let list = ElementQuery::from_root(ui)
        .match_descendants()
        .match_accessible_role(AccessibleRole::List)
        .find_first()
        .expect("list");
    assert_eq!(list.accessible_item_count(), Some(50_000));

    // Only the visible rows are instantiated; the first interesting item
    // is selected (`nextInteresting`), and its details shown.
    let rows = items(ui);
    assert!((10..100).contains(&rows.len()), "{} row items", rows.len());
    assert_eq!(
        rows[0].label,
        "dir/file-00000, left new file, left to right, right unchanged"
    );
    assert!(rows[0].selected);
    assert_eq!(ui.get_details_path(), "dir/file-00000");
    assert!(ui.get_actions_enabled() && ui.get_go_enabled());

    // Click row 0, shift-click row 3, cmd-click row 1: rows 0, 2, 3.
    // (Slint reports Cmd as `control` on macOS.)
    click(ui, rows[0].centre, None);
    click(ui, rows[3].centre, Some(Key::Shift));
    click(ui, rows[1].centre, Some(Key::Control));
    assert_eq!(app.table().selected(), [0, 2, 3]);
    let flags: Vec<bool> = items(ui).iter().take(4).map(|r| r.selected).collect();
    assert_eq!(flags, [true, false, true, true]);
    // Several selected: no details.
    assert_eq!(ui.get_details_path(), "");

    // `>` goes to the session for each item selected; its answer shows.
    type_text(ui, ">");
    let set = |i| Command::SetAction(i, SetAction::Right);
    assert_eq!(
        format!("{:?}", app.sent()),
        format!("{:?}", [set(0), set(2), set(3)])
    );
    let mut changed = fake_item("dir/file-00003", Action::LtoR(false));
    changed.changed_from_default = true;
    app.handle(Event::Item(3, changed));
    assert!(items(ui)[3].label.contains("left to right"));

    // The keys go to the dialog, not the table, while one is up.
    app.handle(Event::Warning("Watch out".to_owned()));
    assert_eq!(ui.get_dialog(), 1);
    type_text(ui, "<");
    assert_eq!(app.sent().len(), 3);
    assert!(press(ui, "Close"));
    assert_eq!(ui.get_dialog(), 0);
}

/// Replicas `a` and `b` and a Unison directory in `dir`, with `files` in
/// `a` and `b`, synced once through the window.
fn synced(dir: &Path, files: &[&str]) {
    for r in ["a", "b", "unison"] {
        std::fs::create_dir_all(dir.join(r)).unwrap();
    }
    for f in files {
        std::fs::write(dir.join("a").join(f), "same").unwrap();
        std::fs::write(dir.join("b").join(f), "same").unwrap();
    }
    support::window_sync(dir).unwrap();
}

fn roots(dir: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["a", "b"]
        .iter()
        .map(|r| dir.join(r).display().to_string())
        .collect();
    args.extend(["-log=false", "-times"].map(String::from));
    args
}

/// The window starts on the roots given, shows the items with the first
/// interesting one selected and its details, takes an action through the
/// session, and propagates on Go.
#[test]
fn a_sync_goes_through_the_session() {
    let work = support::Work::new("window-sync");
    let dir = &work.0;
    synced(dir, &["both"]);
    std::fs::write(dir.join("a/both"), "a's").unwrap();
    std::fs::write(dir.join("b/both"), "b's").unwrap();
    std::fs::write(dir.join("a/new"), "new in a").unwrap();

    let (app, out) = support::window(&dir.join("unison"), roots(dir));
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(
        wait_until(WAIT, || app.started() && !app.busy()),
        "{}",
        out.text()
    );
    assert_eq!(
        ui.get_status_text().trim_end(),
        "Check and/or adjust selected actions; then press Go"
    );
    assert_eq!(
        (ui.get_replica1_heading(), ui.get_replica2_heading()),
        (" a ".into(), " b ".into())
    );
    let rows = items(ui);
    assert_eq!(rows.len(), 2);
    assert!(
        rows[0]
            .label
            .starts_with("both, left changed, skip, right changed")
    );
    assert!(rows[0].selected);
    assert_eq!(ui.get_details_path(), "both");
    assert!(
        ui.get_details_text().contains("changed"),
        "{}",
        ui.get_details_text()
    );

    // Right to left on the conflict, then on to the next item.
    type_text(ui, "<");
    assert!(wait_until(WAIT, || {
        app.table().row(0).item.action == Action::RtoL(false)
    }));
    assert!(app.table().row(0).item.changed_from_default);
    assert_eq!(app.table().selected(), [1]);

    assert!(press(ui, "Go"));
    assert!(wait_until(WAIT, || app.summary().is_some() && !app.busy()));
    let summary = app.summary().unwrap();
    assert_eq!((summary.transferred, summary.failed.len()), (2, 0));
    assert!(
        ui.get_status_text().starts_with("Synchronization complete"),
        "{}",
        ui.get_status_text()
    );
    assert!(
        items(ui)[0].label.ends_with(", done"),
        "{}",
        items(ui)[0].label
    );
    assert_eq!(std::fs::read_to_string(dir.join("a/both")).unwrap(), "b's");
    assert_eq!(
        std::fs::read_to_string(dir.join("b/new")).unwrap(),
        "new in a"
    );
    app.quit(0);
}

/// A profile without roots asks for them (`getFirstRoot`,
/// `getSecondRoot`).
#[test]
fn roots_are_asked_for() {
    let work = support::Work::new("window-roots");
    let dir = &work.0;
    for r in ["a", "b", "unison"] {
        std::fs::create_dir_all(dir.join(r)).unwrap();
    }
    std::fs::write(dir.join("a/f"), "f").unwrap();
    std::fs::write(dir.join("unison/p.prf"), "log = false\n").unwrap();
    let (app, _) = support::window(&dir.join("unison"), vec!["p".to_owned()]);
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(wait_until(WAIT, || ui.get_dialog() == 2));
    ui.set_root_dir(dir.join("a").display().to_string().into());
    assert!(press(ui, "OK"));
    assert_eq!(ui.get_dialog(), 3);
    // A socket needs a port.
    ui.set_root_kind(2);
    ui.set_root_host("localhost".into());
    assert!(press(ui, "OK"));
    assert_eq!(
        (ui.get_dialog(), ui.get_root_error()),
        (3, "Please enter a port".into())
    );
    ui.set_root_kind(0);
    ui.set_root_dir(dir.join("b").display().to_string().into());
    assert!(press(ui, "OK"));
    // No archives yet: a warning, then the items.
    assert!(wait_until(WAIT, || ui.get_dialog() == 1));
    assert_eq!(ui.get_dialog_title(), "Warning");
    assert!(press(ui, "OK"));
    assert!(wait_until(WAIT, || app.started() && !app.busy()));
    assert_eq!(app.table().len(), 1);
    assert_eq!(ui.get_profile_text(), "Profile: p");
    assert!(
        ui.get_window_title().starts_with("Gemel [p]"),
        "{}",
        ui.get_window_title()
    );
    app.quit(0);
}

/// Without a profile or roots, the window says it can't choose one yet,
/// and Quit ends the program.
#[test]
fn without_a_profile_the_window_ends() {
    let work = support::Work::new("window-none");
    std::fs::create_dir_all(work.0.join("unison")).unwrap();
    let (app, _) = support::window(&work.0.join("unison"), Vec::new());
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(wait_until(WAIT, || ui.get_dialog() == 1));
    assert_eq!(ui.get_dialog_title(), "Fatal error");
    assert!(press(ui, "Quit"));
    assert_eq!(app.exit_code(), Some(1));
}

/// With `-batch`, the window propagates once update detection is done,
/// without waiting for Go ("Detect Updates and Proceed").
#[test]
fn batch_mode_syncs_without_go() {
    let work = support::Work::new("window-batch");
    let dir = &work.0;
    synced(dir, &["f"]);
    std::fs::write(dir.join("a/new"), "new in a").unwrap();
    let mut args = roots(dir);
    args.push("-batch".to_owned());
    let (app, out) = support::window(&dir.join("unison"), args);
    let _ticks = app.start_ticks();
    assert!(
        wait_until(WAIT, || app.summary().is_some() && !app.busy()),
        "{}",
        out.text()
    );
    assert_eq!(app.summary().unwrap().transferred, 1);
    assert_eq!(
        std::fs::read_to_string(dir.join("b/new")).unwrap(),
        "new in a"
    );
    app.quit(0);
}

/// Deleting the whole of a replica is confirmed first (`confirmbigdeletes`),
/// even in batch mode: Quit ends the program, and OK leaves Go to press.
#[test]
fn big_deletions_are_confirmed() {
    let work = support::Work::new("window-big-deletes");
    let dir = &work.0;
    synced(dir, &["f", "g"]);
    std::fs::remove_file(dir.join("a/f")).unwrap();
    std::fs::remove_file(dir.join("a/g")).unwrap();
    for (button, batch) in [("Quit", false), ("OK", true)] {
        let mut args = roots(dir);
        if batch {
            args.push("-batch".to_owned());
        }
        let (app, out) = support::window(&dir.join("unison"), args);
        let ui = app.ui();
        let _ticks = app.start_ticks();
        assert!(wait_until(WAIT, || ui.get_dialog() == 1), "{}", out.text());
        assert_eq!(ui.get_dialog_title(), "Warning");
        assert!(press(ui, button));
        if button == "Quit" {
            assert_eq!(app.exit_code(), Some(0));
            continue;
        }
        assert!(wait_until(WAIT, || !app.busy() && ui.get_go_enabled()));
        assert!(app.summary().is_none());
        assert!(dir.join("b/f").exists());
        app.quit(0);
    }
}

/// With `confirmmerge`, a merge's result is shown, and kept on Yes.
#[cfg(unix)]
#[test]
fn merges_are_confirmed() {
    let work = support::Work::new("window-merge");
    let dir = &work.0;
    synced(dir, &["README"]);
    // Not the length they had, so that the change is seen within the
    // second.
    std::fs::write(dir.join("a/README"), "a's change\n").unwrap();
    std::fs::write(dir.join("b/README"), "b's change\n").unwrap();
    let mut args = roots(dir);
    args.extend(
        [
            "-merge",
            "Name README -> cat CURRENT1 CURRENT2 > NEW",
            "-confirmmerge",
        ]
        .map(String::from),
    );
    let (app, out) = support::window(&dir.join("unison"), args);
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(
        wait_until(WAIT, || app.started() && !app.busy()),
        "{}",
        out.text()
    );
    assert_eq!(app.table().row(0).item.action, Action::Merge);
    assert!(press(ui, "Go"));
    assert!(wait_until(WAIT, || ui.get_dialog() == 1), "{}", out.text());
    assert!(
        ui.get_dialog_message()
            .starts_with("Do you want to commit the changes to the replicas ?"),
        "{}",
        ui.get_dialog_message()
    );
    assert!(press(ui, "Yes"));
    assert!(wait_until(WAIT, || app.summary().is_some() && !app.busy()));
    assert_eq!(app.summary().unwrap().transferred, 1);
    for r in ["a", "b"] {
        assert_eq!(
            std::fs::read_to_string(dir.join(r).join("README")).unwrap(),
            "a's change\nb's change\n"
        );
    }
    app.quit(0);
}

/// A cell of the statistics window's table: row `r` (the reception rate,
/// the data received, the file data written), column `c` (the client,
/// the server, the total).
fn stat(app: &App, r: usize, c: usize) -> String {
    let row = app.stats_window().get_table().row_data(r).unwrap();
    row.row_data(c).unwrap().to_string()
}

/// A sync, without a session: Stop while it runs, the progress bar with
/// the time left and the rate, the file data written, then the summary.
#[test]
fn a_sync_shows_its_progress_and_summary() {
    init();
    let app = App::detached().unwrap();
    let ui = app.ui();
    ui.show().unwrap();
    app.handle(Event::Updates(fake_updates(vec![
        fake_item("a", Action::LtoR(false)),
        fake_item("b", Action::LtoR(false)),
    ])));
    assert!(!ui.get_stop_enabled());
    assert!(press(ui, "Go"));
    assert!(ui.get_stop_enabled() && !ui.get_go_enabled());
    assert!(press(ui, "Stop"));
    let sent = format!("{:?}", app.sent());
    assert!(sent.ends_with("Synchronize, Abort]"), "{sent}");

    let progress = |transferred| Progress {
        item: 0,
        item_transferred: transferred,
        item_length: 100,
        item_progress: format!("{transferred:3}%"),
        transferred,
        length: 200,
        items_done: 0,
        items: 2,
        rate: 1000.,
        eta: "00:01".to_owned(),
    };
    app.handle(Event::Progress(progress(50)));
    assert_eq!(ui.get_progress_fraction(), 0.25);
    // The text, at most twice a second.
    assert_eq!(ui.get_progress_text(), " ");
    std::thread::sleep(Duration::from_millis(550));
    app.handle(Event::Progress(progress(60)));
    assert_eq!(ui.get_progress_text(), "00:01 remaining  (1.0 KiB/s)");
    assert_eq!(app.table().row(0).status_text(), " 60%");

    let summary = Summary {
        transferred: 1,
        failed: vec![(1, "disk full".to_owned())],
        ..Summary::default()
    };
    app.handle(Event::Synchronized(summary));
    assert!(!ui.get_stop_enabled());
    assert!(format!("{:?}", app.sent()).ends_with("Commit]"));
    app.handle(Event::Committed);
    assert_eq!(
        ui.get_status_text().trim_end(),
        "Synchronization complete         1 failure"
    );
    assert_eq!(
        (ui.get_dialog(), ui.get_dialog_title()),
        (4, "Synchronization summary".into())
    );
    assert_eq!(
        ui.get_dialog_message(),
        "The replicas are not fully synchronized.\nThere was one failure."
    );
    let entry = ui.get_summary_entries().row_data(0).unwrap();
    assert_eq!(
        (entry.path.as_str(), entry.description.as_str()),
        ("b", "transport failure")
    );
    assert!(press(ui, "OK"));
    assert_eq!(ui.get_dialog(), 0);

    // Both items' bytes were written on the client's side (local roots).
    ui.invoke_show_statistics();
    assert_eq!(stat(&app, 2, 0), "60 B");
    assert_eq!(stat(&app, 2, 1), "0 B");
}

/// Rescan finds what changed since; "Recheck Unsynchronized Items" looks
/// only at the items not synchronised.
#[test]
fn rescan_and_recheck() {
    let work = support::Work::new("window-rescan");
    let dir = &work.0;
    synced(dir, &["both"]);
    std::fs::write(dir.join("a/both"), "a's").unwrap();
    std::fs::write(dir.join("b/both"), "b's").unwrap();
    std::fs::write(dir.join("a/new"), "new in a").unwrap();
    let (app, out) = support::window(&dir.join("unison"), roots(dir));
    let ui = app.ui();
    let _ticks = app.start_ticks();
    let ready = || app.started() && !app.busy() && ui.get_dialog() == 0;
    assert!(wait_until(WAIT, ready), "{}", out.text());
    assert_eq!(app.table().len(), 2);

    // The conflict is skipped.
    assert!(press(ui, "Go"));
    assert!(wait_until(WAIT, || ui.get_dialog() == 4), "{}", out.text());
    assert!(
        ui.get_dialog_message()
            .ends_with("There was one skipped item.")
    );
    assert!(press(ui, "OK"));
    assert!(wait_until(WAIT, ready));

    // Something else changes: a recheck doesn't see it, a rescan does.
    std::fs::write(dir.join("a/other"), "other in a").unwrap();
    type_text(ui, "f");
    assert!(wait_until(WAIT, ready), "{}", out.text());
    let paths = |app: &App| -> Vec<String> {
        (app.table().rows().iter())
            .map(|r| r.item.path_string.clone())
            .collect()
    };
    assert_eq!(paths(&app), ["both"]);
    assert!(press(ui, "Rescan"));
    assert!(wait_until(WAIT, ready), "{}", out.text());
    assert_eq!(paths(&app), ["both", "other"]);
    app.quit(0);
}

/// A server on a socket, killed when dropped.
struct Server(std::process::Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `gemel-gui -socket` serving `dir`, once it says it has started, and
/// its port.
fn socket_server(dir: &Path) -> (Server, u16) {
    use std::io::BufRead;
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = std::process::Command::new(support::gemel_gui())
        .args(["-socket", &port.to_string(), "-listen", "127.0.0.1"])
        .current_dir(dir)
        .env("UNISON", dir.join("unison-server"))
        .env("HOME", dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut r = std::io::BufReader::new(child.stderr.take().unwrap());
    let server = Server(child);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = r.read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(20)).unwrap();
    assert_eq!(line.trim_end(), "server started");
    (server, port)
}

/// With a root on a server, the statistics window counts what is
/// received from it.
#[test]
fn statistics_count_what_comes_from_the_server() {
    let work = support::Work::new("window-stats");
    let dir = &work.0;
    for r in ["a", "b", "unison", "unison-server"] {
        std::fs::create_dir_all(dir.join(r)).unwrap();
    }
    std::fs::write(dir.join("b/f"), "from the server").unwrap();
    let (_server, port) = socket_server(dir);
    let args = vec![
        dir.join("a").display().to_string(),
        format!("socket://127.0.0.1:{port}/b"),
        "-log=false".to_owned(),
        "-times".to_owned(),
    ];
    let (app, out) = support::window(&dir.join("unison"), args);
    let ui = app.ui();
    let _ticks = app.start_ticks();
    // No archives yet: a warning first.
    assert!(wait_until(WAIT, || ui.get_dialog() == 1), "{}", out.text());
    assert!(press(ui, "OK"));
    assert!(
        wait_until(WAIT, || app.started() && !app.busy()),
        "{}",
        out.text()
    );
    assert!(press(ui, "Go"));
    assert!(wait_until(WAIT, || app.summary().is_some() && !app.busy()));
    assert_eq!(
        std::fs::read_to_string(dir.join("a/f")).unwrap(),
        "from the server"
    );
    // Sampled twice a second.
    assert!(wait_until(WAIT, || stat(&app, 1, 0) != "0 B"));
    assert_ne!(stat(&app, 1, 1), "0 B");
    app.quit(0);
}
