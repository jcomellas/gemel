//! Draws the window's cards and its other windows with Slint's software
//! renderer, through `uigtk3`'s flows as the window tests drive them, as
//! PNGs in `target/tmp/screenshots`: to look at the GUI on each system
//! without running it (CI keeps them). The menu bar is Slint's own, drawn
//! in the window, as on Linux and Windows; macOS's native one and `rfd`'s
//! pickers aren't drawn. Each test checks that what it drew isn't
//! blank, and that a card's controls lie within it, in the system's
//! fonts.

mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use gemel_gui::App;
use gemel_gui::ui::AppWindow;
use gemel_ui_api::session::{Event, Progress, Summary};
use gemel_ui_api::uicommon::Action;
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, LogicalSize, Model, SharedString};

use support::{
    click, fake_item, fake_updates, init_drawn, items, press, profiles, roots, synced, type_text,
    wait_until,
};

const WAIT: Duration = Duration::from_secs(60);

/// Where the PNGs go.
fn shots() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Draws `w` as `name.png`, at its size, or `size` if it has none yet,
/// once what moves has stopped.
fn shoot(w: &impl ComponentHandle, name: &str, size: (f32, f32)) {
    let window = w.window();
    if window.size().width == 0 {
        window.set_size(LogicalSize::new(size.0, size.1));
    }
    // The style's animations (a button's colours as it is enabled or
    // not), done.
    support::pump(Duration::from_millis(500));
    let shot = window.take_snapshot().unwrap();
    let pixels = shot.as_slice();
    assert!(
        pixels.iter().any(|p| *p != pixels[0]),
        "{name} is drawn in one colour"
    );
    let file = std::fs::File::create(shots().join(format!("{name}.png"))).unwrap();
    let mut png = png::Encoder::new(std::io::BufWriter::new(file), shot.width(), shot.height());
    png.set_color(png::ColorType::Rgba);
    png.set_depth(png::BitDepth::Eight);
    let mut png = png.write_header().unwrap();
    png.write_image_data(shot.as_bytes()).unwrap();
    png.finish().unwrap();
}

/// The main window's size: its preferred one, as it opens.
const MAIN: (f32, f32) = (900., 640.);

/// Draws the main window as `name.png`, once its card's controls (its
/// buttons, fields, lists and choices) are found to lie within it.
fn shoot_main(ui: &AppWindow, name: &str) {
    shoot(ui, name, MAIN);
    let rect = |e: &ElementHandle| {
        let (p, s) = (e.absolute_position(), e.size());
        (p.x, p.y, p.x + s.width, p.y + s.height)
    };
    let cards = ElementQuery::from_root(ui)
        .match_descendants()
        .match_accessible_role(AccessibleRole::Groupbox)
        .find_all();
    for card in cards {
        let c = rect(&card);
        let controls = card
            .query_descendants()
            .match_predicate(|e| {
                matches!(
                    e.accessible_role(),
                    Some(
                        AccessibleRole::Button
                            | AccessibleRole::TextInput
                            | AccessibleRole::Combobox
                            | AccessibleRole::Checkbox
                            | AccessibleRole::RadioButton
                            | AccessibleRole::List
                    )
                )
            })
            .find_all();
        for e in controls {
            let r = rect(&e);
            assert!(
                r.0 >= c.0 - 0.5 && r.1 >= c.1 - 0.5 && r.2 <= c.2 + 0.5 && r.3 <= c.3 + 0.5,
                "{name}: {:?} {:?} at {r:?} isn't within its card, at {c:?}",
                e.accessible_role(),
                e.accessible_label()
            );
        }
    }
}

/// The top and the bottom of the text `label`.
fn text_span(ui: &AppWindow, label: &str) -> (f32, f32) {
    let e = ElementHandle::find_by_accessible_label(ui, label)
        .next()
        .unwrap_or_else(|| panic!("no {label:?}"));
    (
        e.absolute_position().y,
        e.absolute_position().y + e.size().height,
    )
}

fn key(k: Key) -> SharedString {
    SharedString::from(k)
}

/// The items, as found after a sync: a conflict, a file new in `a`, one
/// deleted in `b`, and a directory new in `b`. Then an action, the diff,
/// the statistics, and the window once synced.
#[test]
fn the_main_window() {
    init_drawn();
    let work = support::Work::new("shots-main");
    let dir = &work.0;
    synced(dir, &["both", "gone", "same"]);
    std::fs::write(dir.join("a/both"), "one\ntwo\nthree in a\n").unwrap();
    std::fs::write(dir.join("b/both"), "one\ntwo\nthree in b\nfour\n").unwrap();
    std::fs::write(dir.join("a/new"), "new in a").unwrap();
    std::fs::remove_file(dir.join("b/gone")).unwrap();
    std::fs::create_dir_all(dir.join("b/photos/2026")).unwrap();
    std::fs::write(dir.join("b/photos/2026/beach.jpg"), "jpg").unwrap();
    let mut args = roots(dir);
    // `diff` isn't on Windows' PATH.
    if cfg!(windows) {
        args.extend(["-diff", "echo"].map(String::from));
    }
    let (app, out) = support::window(&dir.join("unison"), args);
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(
        wait_until(WAIT, || app.started() && !app.busy()),
        "{}",
        out.text()
    );
    shoot_main(ui, "main-1-items");

    // Right to left on the conflict, which then shows as changed, then on
    // to the next item.
    type_text(ui, "<");
    assert!(wait_until(WAIT, || {
        app.table().row(0).item.changed_from_default
    }));
    shoot_main(ui, "main-2-action");
    ui.invoke_select_all();
    shoot_main(ui, "main-3-all-selected");

    click(ui, items(ui)[0].centre, None);
    type_text(ui, "d");
    assert!(
        wait_until(WAIT, || !app.diff_windows().is_empty() && !app.busy()),
        "{}",
        out.text()
    );
    shoot(&app.diff_windows()[0], "diff", (640., 420.));

    assert!(press(ui, "Go"));
    assert!(wait_until(WAIT, || app.summary().is_some() && !app.busy()));
    shoot_main(ui, "main-4-synced");
    ui.invoke_show_statistics();
    shoot(app.stats_window(), "statistics", (400., 200.));
    app.quit(0);
}

/// Without a session: a sync's progress, its summary with a failure, a
/// warning, an error, and asking before quitting while working.
#[test]
fn the_dialogs() {
    init_drawn();
    let app = App::detached().unwrap();
    let ui = app.ui();
    ui.show().unwrap();
    app.handle(Event::Updates(fake_updates(vec![
        fake_item("documents/report.odt", Action::LtoR(false)),
        fake_item("documents/notes.txt", Action::LtoR(false)),
        fake_item("music/track.flac", Action::Skip(false)),
    ])));
    assert!(press(ui, "Go"));
    let progress = |transferred| Progress {
        item: 0,
        item_transferred: transferred,
        item_length: 100,
        item_progress: format!("{transferred:3}%"),
        transferred,
        length: 200,
        items_done: 0,
        items: 2,
        rate: 1_500_000.,
        eta: "00:42".to_owned(),
    };
    app.handle(Event::Progress(progress(40)));
    // The text, at most twice a second.
    std::thread::sleep(Duration::from_millis(550));
    app.handle(Event::Progress(progress(60)));
    shoot_main(ui, "main-5-progress");

    // Asking before quitting while working.
    ui.window().dispatch_event(WindowEvent::CloseRequested);
    assert_eq!(ui.get_dialog_title(), "Premature exit");
    shoot_main(ui, "dialog-premature-exit");
    assert!(press(ui, "No"));

    let summary = Summary {
        transferred: 1,
        failed: vec![(1, "No space left on device".to_owned())],
        ..Summary::default()
    };
    app.handle(Event::Synchronized(summary));
    app.handle(Event::Committed);
    assert_eq!(ui.get_dialog(), 4);
    shoot_main(ui, "dialog-summary");
    assert!(press(ui, "Show details"));
    shoot_main(ui, "dialog-summary-details");
    assert!(press(ui, "OK"));

    app.handle(Event::Warning(
        "The archive file is missing on some hosts.\n\
         For safety, the remaining copies should be removed."
            .to_owned(),
    ));
    shoot_main(ui, "dialog-warning");
    assert!(press(ui, "OK") || press(ui, "Close"));
    app.handle(Event::Error(
        "Fatal error: Connection to the server lost".to_owned(),
    ));
    shoot_main(ui, "dialog-error");
}

/// A profile without roots asks for them: the first, then the second, a
/// local directory, over ssh or on a socket.
#[test]
fn the_roots_dialogs() {
    init_drawn();
    let work = support::Work::new("shots-roots");
    let dir = &work.0;
    for r in ["a", "unison"] {
        std::fs::create_dir_all(dir.join(r)).unwrap();
    }
    std::fs::write(dir.join("unison/p.prf"), "log = false\n").unwrap();
    let (app, out) = support::window(&dir.join("unison"), vec!["p".to_owned()]);
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(wait_until(WAIT, || ui.get_dialog() == 2), "{}", out.text());
    shoot_main(ui, "roots-1-first");
    ui.set_root_dir(dir.join("a").display().to_string().into());
    assert!(press(ui, "OK"));
    assert_eq!(ui.get_dialog(), 3);
    shoot_main(ui, "roots-2-second");
    ui.set_root_kind(1);
    ui.set_root_host("server.example.org".into());
    ui.set_root_user("me".into());
    ui.set_root_dir("/home/me/files".into());
    shoot_main(ui, "roots-3-ssh");
    ui.set_root_kind(2);
    shoot_main(ui, "roots-4-socket");
    app.quit(0);
}

/// The profile manager, the assistant's pages, and the editor: its list,
/// each kind of preference edited, the tree preferences are added from,
/// and deleting one.
#[test]
fn the_profile_dialogs() {
    init_drawn();
    let work = support::Work::new("shots-profiles");
    let dir = &work.0;
    profiles(dir, &["work", "home"]);
    let (app, out) = support::window(&dir.join("unison"), Vec::new());
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(wait_until(WAIT, || ui.get_dialog() == 6), "{}", out.text());
    shoot_main(ui, "manager-1");
    // The list, between its label and the summary.
    let list = ElementQuery::from_root(ui)
        .match_descendants()
        .match_accessible_role(AccessibleRole::List)
        .match_predicate(|e| e.accessible_label().as_deref() == Some("Profiles"))
        .find_first()
        .unwrap();
    let (top, bottom) = (
        list.absolute_position().y,
        list.absolute_position().y + list.size().height,
    );
    assert!(text_span(ui, "Select a profile:").1 <= top + 0.5);
    assert!(bottom <= text_span(ui, "Summary").0 + 0.5);
    ui.invoke_profile_clicked(1);
    shoot_main(ui, "manager-2-selected");

    // The assistant, a page at a time, over ssh.
    assert!(press(ui, "Add"));
    shoot_main(ui, "assistant-0");
    assert!(press(ui, "Next"));
    ui.set_wiz_name("laptop".into());
    ui.set_wiz_label("The laptop's documents".into());
    ui.invoke_wizard_changed();
    shoot_main(ui, "assistant-1");
    assert!(press(ui, "Next"));
    shoot_main(ui, "assistant-2-local");
    ui.set_wiz_kind(1);
    ui.invoke_wizard_changed();
    shoot_main(ui, "assistant-2-ssh");
    ui.set_wiz_host("laptop.example.org".into());
    ui.set_wiz_user("me".into());
    ui.invoke_wizard_changed();
    while ui.get_wiz_page() < 5 {
        let page = ui.get_wiz_page();
        if page == 3 {
            ui.set_wiz_first(dir.join("work-a").display().to_string().into());
            ui.set_wiz_second("Documents".into());
            ui.invoke_wizard_changed();
        }
        if page > 2 {
            shoot_main(ui, &format!("assistant-{page}"));
        }
        assert!(press(ui, "Next"), "page {page}");
    }
    shoot_main(ui, "assistant-5");
    assert!(press(ui, "Cancel"));
    assert!(wait_until(WAIT, || ui.get_dialog() == 6), "{}", out.text());

    // The editor, on `home`.
    ui.invoke_profile_clicked(0);
    assert!(press(ui, "Edit"));
    assert!(wait_until(WAIT, || ui.get_dialog() == 8), "{}", out.text());
    shoot_main(ui, "editor-1");
    let row = |name: &str| {
        ui.get_editor_rows()
            .iter()
            .position(|r| r.name == name)
            .unwrap() as i32
    };
    ui.invoke_editor_clicked(row("root"));
    shoot_main(ui, "editor-2-documented");
    assert!(press(ui, "Edit"));
    shoot_main(ui, "editor-3-list");
    assert!(press(ui, "Cancel"));
    ui.invoke_editor_clicked(row("log"));
    assert!(press(ui, "Edit"));
    shoot_main(ui, "editor-4-boolean");
    assert!(press(ui, "Cancel"));
    ui.invoke_editor_clicked(row("label"));
    assert!(press(ui, "Edit"));
    shoot_main(ui, "editor-5-text");
    assert!(press(ui, "Cancel"));

    assert!(press(ui, "Add"));
    assert_eq!(ui.get_dialog(), 10);
    shoot_main(ui, "editor-6-tree");
    assert!(press(ui, "Expand all"));
    let ignore = ui
        .get_tree_rows()
        .iter()
        .position(|r| r.title == "ignore")
        .unwrap();
    ui.invoke_tree_clicked(ignore as i32);
    shoot_main(ui, "editor-7-tree-documented");
    // The longest documentation, at its end.
    let catalog = gemel_ui_api::profile_editor::Catalog::new().unwrap();
    let longest = ui
        .get_tree_rows()
        .iter()
        .enumerate()
        .max_by_key(|(_, r)| catalog.documentation(&r.title).1.len())
        .unwrap()
        .0;
    ui.invoke_tree_clicked(longest as i32);
    shoot_main(ui, "editor-8-tree-long");
    let doc = slint::LogicalPosition::new(450., 500.);
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position: doc });
    ui.window().dispatch_event(WindowEvent::PointerScrolled {
        position: doc,
        delta_x: 0.,
        delta_y: -5000.,
    });
    shoot_main(ui, "editor-9-tree-long-end");
    type_text(ui, &key(Key::Escape));

    ui.invoke_editor_clicked(row("label"));
    assert!(press(ui, "Delete"));
    assert_eq!(ui.get_dialog(), 1);
    shoot_main(ui, "editor-10-delete");
    app.quit(0);
}

/// The manual's window, at each of its pages, and down one.
#[test]
fn the_manual() {
    init_drawn();
    let work = support::Work::new("shots-manual");
    profiles(&work.0, &["a"]);
    let (app, out) = support::window(&work.0.join("unison"), Vec::new());
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(wait_until(WAIT, || ui.get_dialog() == 6), "{}", out.text());
    assert!(press(ui, "Help"));
    let w = app.doc_window();
    for i in 0..ui.get_doc_topics().row_count() {
        w.invoke_select(i as i32);
        shoot(w, &format!("manual-{i:02}"), (900., 520.));
    }
    w.set_scroll(-600.);
    shoot(w, "manual-10-down", (900., 520.));
    app.quit(0);
}

/// ssh's password prompt, in a dialog of its own.
#[cfg(unix)]
#[test]
fn the_password_dialog() {
    init_drawn();
    let work = support::Work::new("shots-password");
    let dir = &work.0;
    for r in ["a", "unison"] {
        std::fs::create_dir_all(dir.join(r)).unwrap();
    }
    let shim = dir.join("ssh");
    std::fs::write(
        &shim,
        "#!/bin/sh\n\
         printf \"me@server.example.org's password: \" > /dev/tty\n\
         read -r pw < /dev/tty\n\
         exit 255\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let args = vec![
        dir.join("a").display().to_string(),
        "ssh://server.example.org/files".to_owned(),
        "-sshcmd".to_owned(),
        shim.display().to_string(),
        "-log=false".to_owned(),
    ];
    let (app, out) = support::window(&dir.join("unison"), args);
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(
        wait_until(WAIT, || ui.get_dialog() == 5
            && ui.get_dialog_message().ends_with("password: ")),
        "{}",
        out.text()
    );
    shoot_main(ui, "dialog-password");
    app.quit(0);
}
