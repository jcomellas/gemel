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
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::platform::Key;
use slint::{ComponentHandle, LogicalSize, Model, SharedString};

use support::{
    can_press, click, fake_item, fake_updates, init, items, manager_rows, press, profiles, roots,
    synced, type_text, wait_until,
};

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

/// The window's close button quits as Quit does (`safeExit`): at once
/// while nothing is going on, after asking while the window works.
#[test]
fn closing_the_window_quits_as_quit_does() {
    init();
    let close = |app: &App| {
        app.ui()
            .window()
            .dispatch_event(slint::platform::WindowEvent::CloseRequested)
    };
    let app = App::detached().unwrap();
    app.handle(Event::Updates(fake_updates(vec![fake_item(
        "f",
        Action::LtoR(false),
    )])));
    close(&app);
    assert_eq!(app.exit_code(), Some(0));

    let app = App::detached().unwrap();
    let ui = app.ui();
    app.handle(Event::Updates(fake_updates(vec![fake_item(
        "f",
        Action::LtoR(false),
    )])));
    assert!(press(ui, "Go"));
    assert!(app.busy());
    close(&app);
    assert_eq!(ui.get_dialog_title(), "Premature exit");
    assert!(press(ui, "No"));
    assert_eq!(app.exit_code(), None);
    close(&app);
    assert!(press(ui, "Yes"));
    assert_eq!(app.exit_code(), Some(0));
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
/// `getSecondRoot`, whose Help says how, over it, and leaves what was
/// typed).
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
    assert!(press(ui, "Help"));
    assert_eq!(
        (ui.get_dialog(), ui.get_dialog_title()),
        (1, "Picking roots".into())
    );
    let help = ui.get_dialog_message();
    assert!(
        help.starts_with("Gemel can synchronize a local directory"),
        "{help}"
    );
    assert!(
        help.contains("(Use \"gemel -socket xxx\" on the remote machine"),
        "{help}"
    );
    assert!(press(ui, "OK"));
    assert_eq!(
        (ui.get_dialog(), ui.get_root_kind(), ui.get_root_host()),
        (3, 2, "localhost".into())
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

/// Without a profile or roots, the profile manager opens (`getProfile`):
/// the profiles in order, a profile's roots once it is selected, and Quit,
/// which ends the program.
#[test]
fn without_a_profile_the_profile_manager_opens() {
    let work = support::Work::new("window-none");
    profiles(&work.0, &["b", "a"]);
    let (app, out) = support::window(&work.0.join("unison"), Vec::new());
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(wait_until(WAIT, || ui.get_dialog() == 6), "{}", out.text());
    assert_eq!(manager_rows(ui), ["a", "b"]);
    assert_eq!(ui.get_profiles().row_data(0).unwrap().label, "Label of a");
    assert!(!can_press(ui, "Open") && !can_press(ui, "Delete"));
    ui.invoke_profile_clicked(1);
    assert!(can_press(ui, "Open"));
    assert!(ui.get_profile_root1().ends_with("b-a"));
    assert!(ui.get_profile_root2().ends_with("b-b"));
    assert!(press(ui, "Quit"));
    assert_eq!(app.exit_code(), Some(0));
}

/// The manual (`documentation`, `addDocSection`): the Help menu's
/// sections, and the manual's window. Help in the profile manager opens
/// it at About, and a section opens it there. Each page is shown as far
/// down as it was. There is one window: a section it hasn't leaves it
/// where it is, until it is closed.
#[test]
fn the_manual_has_a_window_of_its_own() {
    let work = support::Work::new("window-doc");
    profiles(&work.0, &["a"]);
    let (app, out) = support::window(&work.0.join("unison"), Vec::new());
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(wait_until(WAIT, || ui.get_dialog() == 6), "{}", out.text());
    let topics: Vec<String> = ui
        .get_doc_topics()
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    assert_eq!(
        topics,
        [
            "About Gemel",
            "People",
            "Obtaining Unison",
            "Community, Maintenance, and Development",
            "Copying",
            "Acknowledgements",
            "Upgrading",
            "Tutorial",
            "Basic Concepts",
            "Invariants",
            "Running Unison",
        ]
    );
    let w = app.doc_window();
    let first_line = || w.get_lines().row_data(0).unwrap().to_string();
    assert!(!w.window().is_visible());
    assert!(press(ui, "Help"));
    assert!(w.window().is_visible());
    assert_eq!(w.get_current(), 0);
    assert_eq!(first_line(), "Gemel File Synchronizer");
    ui.invoke_documentation("running".into());
    assert_eq!(w.get_current(), 10);
    assert_eq!(first_line(), "Running Unison");
    w.set_scroll(-500.);
    // A tab, as a screen reader chooses it.
    let tab = ElementHandle::find_by_accessible_label(w, "Tutorial")
        .find(|e| e.accessible_role() == Some(AccessibleRole::Tab))
        .unwrap();
    tab.invoke_accessible_default_action();
    assert_eq!(w.get_current(), 7);
    assert_eq!(first_line(), "Tutorial");
    // The keys.
    type_text(w, &SharedString::from(Key::DownArrow));
    assert_eq!(w.get_current(), 8);
    type_text(w, &SharedString::from(Key::UpArrow));
    assert_eq!(w.get_current(), 7);
    // Each page is shown as far down as it was.
    assert_eq!(w.get_scroll(), 0.);
    ui.invoke_documentation("nosuch".into());
    assert_eq!(w.get_current(), 7);
    w.invoke_select(10);
    // The list may move it a little as it lays the page out again.
    assert!(w.get_scroll() < -400., "{}", w.get_scroll());
    // Escape closes, as Close does.
    type_text(w, &SharedString::from(Key::Escape));
    assert!(!w.window().is_visible());
    ui.invoke_documentation("nosuch".into());
    assert!(w.window().is_visible());
    assert_eq!(w.get_current(), 0);
    assert_eq!(w.get_scroll(), 0.);
    ui.invoke_documentation("running".into());
    assert_eq!(w.get_scroll(), 0.);
    app.quit(0);
}

/// The profile editor (`editProfile`), from Edit in the manager:
/// - the profile's preferences, by name, either way;
/// - a boolean changed (`editPreference`);
/// - a list added from the tree (`addPreference`);
/// - a preference deleted, once asked;
/// - Apply, which writes the profile as `uigtk3` does, then OK, back to
///   the manager with the profile selected;
/// - a profile that can't be saved, which is said so over the editor.
#[test]
fn the_profile_editor_writes_profiles_as_uigtk3_does() {
    let work = support::Work::new("window-editor");
    let dir = &work.0;
    profiles(dir, &["a", "b"]);
    let (app, out) = support::window(&dir.join("unison"), Vec::new());
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(wait_until(WAIT, || ui.get_dialog() == 6), "{}", out.text());
    assert!(!can_press(ui, "Edit"));
    // The manager's list takes Up and Down.
    let down = SharedString::from(Key::DownArrow);
    let up = SharedString::from(Key::UpArrow);
    let enter = SharedString::from(Key::Return);
    type_text(ui, &down);
    type_text(ui, &down);
    assert_eq!(ui.get_profile_selected(), 1);
    assert!(press(ui, "Edit"));
    assert!(wait_until(WAIT, || ui.get_dialog() == 8), "{}", out.text());
    assert_eq!(ui.get_editor_title(), "b - Profile Editor");
    let rows = || -> Vec<(String, String, String)> {
        ui.get_editor_rows()
            .iter()
            .map(|r| (r.name.into(), r.r#type.into(), r.value.into()))
            .collect()
    };
    let row = |name: &str| rows().iter().position(|r| r.0 == name).unwrap() as i32;
    let (ra, rb) = (
        dir.join("b-a").display().to_string(),
        dir.join("b-b").display().to_string(),
    );
    assert_eq!(
        rows(),
        [
            ("label".into(), "text".into(), "Label of b".into()),
            ("log".into(), "boolean".into(), "false".into()),
            ("root".into(), "text list".into(), format!("{ra}, {rb}")),
        ]
    );
    assert!(press(ui, "Name"));
    assert_eq!(rows()[0].0, "root");
    assert!(press(ui, "Name"));
    // Nothing selected, nothing documented.
    assert!(!ui.get_doc_shown() && !can_press(ui, "Edit"));
    // Down twice, to `log`; Enter edits it.
    type_text(ui, &down);
    type_text(ui, &down);
    assert_eq!(ui.get_editor_selected(), row("log"));
    assert!(ui.get_doc_shown());
    assert!(!ui.get_doc_short().is_empty());
    assert!(!app.documentation_markdown().is_empty());

    // A boolean.
    type_text(ui, &enter);
    assert_eq!(ui.get_dialog(), 9);
    assert_eq!((ui.get_pe_name(), ui.get_pe_kind()), ("log".into(), 0));
    assert!(!ui.get_pe_truth());
    // As a screen reader chooses them.
    let radio = |label: &str| {
        ElementHandle::find_by_accessible_label(ui, label)
            .find(|e| e.accessible_role() == Some(AccessibleRole::RadioButton))
            .unwrap()
    };
    let checked = || {
        (
            ui.get_pe_truth(),
            radio("True").accessible_checked(),
            radio("False").accessible_checked(),
        )
    };
    assert_eq!(checked(), (false, Some(false), Some(true)));
    for (label, truth) in [("True", true), ("False", false), ("True", true)] {
        radio(label).invoke_accessible_default_action();
        assert_eq!(checked(), (truth, Some(truth), Some(!truth)), "{label}");
    }
    assert!(press(ui, "OK"));
    assert_eq!(ui.get_dialog(), 8);
    assert_eq!(rows()[row("log") as usize].2, "true");
    assert!(ui.get_editor_modified());

    // A list, from the tree.
    // Escape closes the tree, its list having the keys.
    assert!(press(ui, "Add"));
    assert_eq!(ui.get_dialog(), 10);
    type_text(ui, &SharedString::from(Key::Escape));
    assert_eq!(ui.get_dialog(), 8);
    assert!(press(ui, "Add"));
    assert!(!ui.get_tree_can_add());
    // The tree's keys: Left collapses the first group, Right expands it,
    // then goes in.
    let tree_len = || ui.get_tree_rows().row_count();
    let expanded = tree_len();
    type_text(ui, &down);
    assert_eq!(ui.get_tree_selected(), 0);
    type_text(ui, &SharedString::from(Key::LeftArrow));
    assert!(tree_len() < expanded);
    type_text(ui, &SharedString::from(Key::RightArrow));
    assert_eq!((tree_len(), ui.get_tree_selected()), (expanded, 0));
    type_text(ui, &SharedString::from(Key::RightArrow));
    assert_eq!(ui.get_tree_selected(), 1);
    assert!(press(ui, "Expand all"));
    let ignore = ui
        .get_tree_rows()
        .iter()
        .position(|r| r.title == "ignore")
        .unwrap();
    ui.invoke_tree_clicked(ignore as i32);
    assert!(ui.get_tree_can_add() && ui.get_doc_shown());
    assert!(press(ui, "Add"));
    assert_eq!((ui.get_dialog(), ui.get_pe_kind()), (9, 2));
    assert_eq!(ui.get_pe_items().row_count(), 0);
    ui.set_pe_entry("Name *.tmp".into());
    assert!(press(ui, "Add"));
    assert_eq!(ui.get_pe_items().row_data(0).unwrap(), "Name *.tmp");
    ui.set_pe_entry("Name *.o".into());
    assert!(press(ui, "Add"));
    // Up goes to the value before, which is then the one edited.
    assert_eq!(ui.get_pe_item(), 1);
    type_text(ui, &up);
    assert_eq!(
        (ui.get_pe_item(), ui.get_pe_entry()),
        (0, "Name *.tmp".into())
    );
    type_text(ui, &down);
    assert!(press(ui, "Remove"));
    assert_eq!(ui.get_pe_items().row_count(), 1);
    assert!(press(ui, "OK"));
    assert_eq!(rows()[row("ignore") as usize].2, "Name *.tmp");

    // A line of text, which Enter takes.
    ui.invoke_editor_clicked(row("label"));
    assert!(press(ui, "Edit"));
    assert_eq!(
        (ui.get_pe_kind(), ui.get_pe_text()),
        (1, "Label of b".into())
    );
    ui.set_pe_text("Another".into());
    support::type_text(ui, &SharedString::from(Key::Return));
    assert_eq!(ui.get_dialog(), 8);
    assert_eq!(rows()[row("label") as usize].2, "Another");

    // Deleted, once asked.
    ui.invoke_editor_clicked(row("label"));
    assert!(press(ui, "Delete"));
    assert_eq!(ui.get_dialog(), 1);
    assert_eq!(
        ui.get_dialog_message(),
        "Do you really want to delete preference label?"
    );
    assert!(press(ui, "Delete"));
    assert_eq!(ui.get_dialog(), 8);
    assert!(!rows().iter().any(|r| r.0 == "label"));

    // Help: the manual, at Running Unison.
    assert!(press(ui, "Help"));
    assert_eq!(app.doc_window().get_current(), 10);
    let _ = app.doc_window().hide();

    // Written in the order read, then added.
    assert!(press(ui, "Apply"));
    assert!(
        wait_until(WAIT, || !ui.get_editor_modified()),
        "{}",
        out.text()
    );
    let prf = dir.join("unison/b.prf");
    assert_eq!(
        std::fs::read_to_string(&prf).unwrap(),
        format!(
            "# Unison preferences\nlog = true\nroot = {ra}\nroot = {rb}\nignore = Name *.tmp\n"
        )
    );
    assert_eq!(ui.get_dialog(), 8);
    assert!(press(ui, "OK"));
    assert!(wait_until(WAIT, || ui.get_dialog() == 6), "{}", out.text());
    assert_eq!(ui.get_profile_selected(), 1);

    // A profile that can't be saved: once read, it is a directory (which
    // even root can't write as a file).
    assert!(press(ui, "Edit"));
    assert!(wait_until(WAIT, || ui.get_dialog() == 8), "{}", out.text());
    std::fs::remove_file(&prf).unwrap();
    std::fs::create_dir(&prf).unwrap();
    ui.invoke_editor_clicked(row("log"));
    assert!(press(ui, "Edit"));
    ui.set_pe_truth(false);
    assert!(press(ui, "OK"));
    assert!(press(ui, "OK"));
    assert!(wait_until(WAIT, || ui.get_dialog() == 1), "{}", out.text());
    assert_eq!(ui.get_dialog_title(), "Could not save profile");
    assert!(
        ui.get_dialog_message()
            .starts_with("Error when saving profile: "),
        "{}",
        ui.get_dialog_message()
    );
    assert!(press(ui, "OK"));
    assert_eq!(ui.get_dialog(), 8);
    assert!(ui.get_editor_modified());
    assert!(press(ui, "Cancel"));
    assert!(wait_until(WAIT, || ui.get_dialog() == 6), "{}", out.text());
    app.quit(0);
}

/// The assistant (`createProfile`) makes a local profile, which the
/// manager selects, and which opens and syncs.
#[test]
fn the_assistant_makes_a_profile_that_syncs() {
    let work = support::Work::new("window-assistant");
    let dir = &work.0;
    profiles(dir, &[]);
    for r in ["a", "b"] {
        std::fs::create_dir_all(dir.join(r)).unwrap();
    }
    std::fs::write(dir.join("a/f"), "f").unwrap();
    let (app, out) = support::window(&dir.join("unison"), Vec::new());
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(wait_until(WAIT, || ui.get_dialog() == 6), "{}", out.text());
    assert!(press(ui, "Add"));
    assert_eq!(
        (ui.get_dialog(), ui.get_wiz_title()),
        (7, "Profile Creation".into())
    );
    assert!(press(ui, "Next"));
    // A name is needed.
    assert!(!can_press(ui, "Next"));
    ui.set_wiz_name("new".into());
    ui.set_wiz_label("A new one".into());
    ui.invoke_wizard_changed();
    assert!(press(ui, "Next"));
    assert_eq!(ui.get_wiz_title(), "Connection Setup");
    assert!(press(ui, "Next"));
    // Two directories, not the same.
    let (a, b) = (
        dir.join("a").display().to_string(),
        dir.join("b").display().to_string(),
    );
    ui.set_wiz_first(a.as_str().into());
    ui.set_wiz_second(a.as_str().into());
    ui.invoke_wizard_changed();
    assert!(!can_press(ui, "Next"));
    ui.set_wiz_second(b.as_str().into());
    ui.invoke_wizard_changed();
    while ui.get_wiz_page() < 5 {
        assert!(press(ui, "Next"));
    }
    assert!(ui.get_wiz_conclusion().contains("Apply"));
    assert!(press(ui, "Apply"));
    assert!(
        wait_until(WAIT, || ui.get_dialog() == 6 && manager_rows(ui) == ["new"]),
        "{}",
        out.text()
    );
    assert_eq!(ui.get_profile_selected(), 0);
    assert_eq!(
        std::fs::read_to_string(dir.join("unison/new.prf")).unwrap(),
        format!("# Unison preferences\nlabel = A new one\nroot = {a}\nroot = {b}\n")
    );
    assert!(press(ui, "Open"));
    // No archives yet: a warning, then the items.
    assert!(wait_until(WAIT, || ui.get_dialog() == 1), "{}", out.text());
    assert!(press(ui, "OK"));
    assert!(
        wait_until(WAIT, || app.started() && !app.busy()),
        "{}",
        out.text()
    );
    assert_eq!(app.table().len(), 1);
    assert_eq!(ui.get_profile_text(), "Profile: new (A new one)");
    app.quit(0);
}

/// Delete asks first, then removes the profile.
#[test]
fn profiles_are_deleted() {
    let work = support::Work::new("window-delete");
    profiles(&work.0, &["a", "b"]);
    let (app, out) = support::window(&work.0.join("unison"), Vec::new());
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(wait_until(WAIT, || ui.get_dialog() == 6), "{}", out.text());
    ui.invoke_profile_clicked(1);
    assert!(press(ui, "Delete"));
    assert_eq!(
        (ui.get_dialog(), ui.get_dialog_message()),
        (1, "Do you really want to delete profile b?".into())
    );
    assert!(press(ui, "Delete"));
    assert!(wait_until(WAIT, || ui.get_dialog() == 6 && manager_rows(ui) == ["a"]));
    assert!(!work.0.join("unison/b.prf").exists());
    app.quit(0);
}

/// Change Profile opens the manager, with Cancel, and another profile.
#[test]
fn another_profile_can_be_chosen() {
    let work = support::Work::new("window-change");
    profiles(&work.0, &["p", "q"]);
    let (app, out) = support::window(&work.0.join("unison"), vec!["p".to_owned()]);
    let ui = app.ui();
    let _ticks = app.start_ticks();
    let ready = || app.started() && !app.busy();
    let warned = |app: &App| {
        if app.ui().get_dialog() == 1 {
            press(app.ui(), "OK");
        }
    };
    assert!(
        wait_until(WAIT, || {
            warned(&app);
            ready()
        }),
        "{}",
        out.text()
    );
    assert_eq!(app.table().row(0).item.path_string, "p-file");
    ui.invoke_change_profile();
    assert!(wait_until(WAIT, || ui.get_dialog() == 6), "{}", out.text());
    assert!(can_press(ui, "Cancel") && !can_press(ui, "Quit"));
    ui.invoke_profile_clicked(1);
    assert!(press(ui, "Open"));
    assert!(
        wait_until(WAIT, || {
            warned(&app);
            ready() && ui.get_profile_text() == "Profile: q (Label of q)"
        }),
        "{}",
        out.text()
    );
    assert_eq!(app.table().row(0).item.path_string, "q-file");
    app.quit(0);
}

/// While a card is up, the main window's menus don't take the keys from
/// its text fields, as `uigtk3`'s modal dialogs leave the main window's
/// alone: Ctrl-A (Cmd-A on macOS) is the field's, not Select All.
#[test]
fn a_cards_text_fields_keep_their_keys() {
    let work = support::Work::new("window-card-keys");
    profiles(&work.0, &["p"]);
    std::fs::write(work.0.join("p-a/another"), "another").unwrap();
    let (app, out) = support::window(&work.0.join("unison"), vec!["p".to_owned()]);
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(
        wait_until(WAIT, || {
            if ui.get_dialog() == 1 {
                press(ui, "OK");
            }
            app.started() && !app.busy()
        }),
        "{}",
        out.text()
    );
    let select_all = || {
        let w = ui.window();
        for (k, down) in [
            (Key::Control.into(), true),
            (SharedString::from("a"), true),
            ("a".into(), false),
            (Key::Control.into(), false),
        ] {
            w.dispatch_event(if down {
                slint::platform::WindowEvent::KeyPressed { text: k }
            } else {
                slint::platform::WindowEvent::KeyReleased { text: k }
            });
        }
    };
    let selected = app.table().selected();
    assert_eq!(selected.len(), 1);
    // A text field, in the assistant's second page.
    ui.invoke_change_profile();
    assert!(wait_until(WAIT, || ui.get_dialog() == 6), "{}", out.text());
    assert!(press(ui, "Add"));
    assert!(press(ui, "Next"));
    select_all();
    assert_eq!(app.table().selected(), selected);
    assert!(press(ui, "Cancel"));
    assert!(press(ui, "Cancel"));
    assert_eq!(ui.get_dialog(), 0);
    // Without a card, it is Select All.
    select_all();
    assert_eq!(app.table().selected(), [0, 1]);
    app.quit(0);
}

/// An error starting is shown, and Close goes to the profile manager
/// (`initLoop`).
#[test]
fn an_error_starting_leads_to_the_profile_manager() {
    let work = support::Work::new("window-start-error");
    profiles(&work.0, &["a"]);
    let (app, out) = support::window(&work.0.join("unison"), vec!["missing".to_owned()]);
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(wait_until(WAIT, || ui.get_dialog() == 1), "{}", out.text());
    assert_eq!(ui.get_dialog_title(), "Error");
    assert!(press(ui, "Close"));
    assert!(wait_until(WAIT, || ui.get_dialog() == 6), "{}", out.text());
    assert_eq!(manager_rows(ui), ["a"]);
    assert!(can_press(ui, "Quit"));
    assert_eq!(app.exit_code(), None);
    app.quit(0);
}

/// A profile that can't be saved is said so, over the manager, and isn't
/// a fatal error; nor is closing an error once the items are shown.
#[test]
fn errors_are_answered_by_what_they_are_about() {
    init();
    let app = App::detached().unwrap();
    let ui = app.ui();
    ui.show().unwrap();
    app.handle(Event::Started(None));
    assert!(format!("{:?}", app.sent()).ends_with("ScanProfiles]"));
    app.handle(Event::Profiles(Vec::new()));
    assert_eq!(ui.get_dialog(), 6);
    assert!(press(ui, "Add"));
    ui.set_wiz_name("p".into());
    ui.set_wiz_first("/a".into());
    ui.set_wiz_second("/b".into());
    ui.invoke_wizard_changed();
    while ui.get_wiz_page() < 5 {
        assert!(press(ui, "Next"));
    }
    assert!(press(ui, "Apply"));
    assert!(format!("{:?}", app.sent()).contains("CreateProfile"));
    app.handle(Event::Error("/u/p.prf: File exists".to_owned()));
    assert_eq!(ui.get_dialog_title(), "Could not save profile");
    assert_eq!(
        ui.get_dialog_message(),
        "Error when saving profile: /u/p.prf: File exists"
    );
    assert!(press(ui, "OK"));
    assert_eq!(ui.get_dialog(), 6);
    assert!(press(ui, "Quit"));
    assert_eq!(app.exit_code(), Some(0));

    // Once the items are shown, Close goes on, and Quit ends with 1.
    let app = App::detached().unwrap();
    let ui = app.ui();
    app.handle(Event::Started(Some("p".to_owned())));
    app.handle(Event::Updates(fake_updates(vec![fake_item(
        "f",
        Action::LtoR(false),
    )])));
    app.handle(Event::Error("boom".to_owned()));
    assert_eq!(
        (ui.get_dialog(), ui.get_dialog_title()),
        (1, "Error".into())
    );
    assert!(press(ui, "Close"));
    assert_eq!(app.exit_code(), None);
    app.handle(Event::Error("boom".to_owned()));
    assert!(wait_until(WAIT, || ui.get_dialog() == 1
        && ui.get_dialog_title() == "Error"));
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

/// Ignoring and sorting renumber the items: until the session's answer,
/// nothing else that names an item is sent.
#[test]
fn ignore_and_sort_hold_the_items_until_they_are_done() {
    init();
    let app = App::detached().unwrap();
    let ui = app.ui();
    ui.show().unwrap();
    let item = |p: &str| fake_item(p, Action::LtoR(false));
    app.handle(Event::Updates(fake_updates(vec![
        item("a"),
        item("b"),
        item("c"),
        item("d"),
    ])));
    let rows = items(ui);
    click(ui, rows[1].centre, None);
    type_text(ui, "I");
    let sent = |app: &App| format!("{:?}", app.sent());
    assert!(sent(&app).ends_with("Ignore([1], Path)]"), "{}", sent(&app));
    let before = app.sent().len();
    type_text(ui, ">");
    type_text(ui, "N");
    assert_eq!(app.sent().len(), before);
    assert_eq!(
        ui.get_status_text().trim_end(),
        "Synchronizer is busy, please wait.."
    );
    // The answer: the selection stays on the same row, as `displayMain`
    // keeps it.
    app.handle(Event::Items {
        items: vec![item("a"), item("c"), item("d")],
        next: 1,
    });
    assert_eq!(app.table().selected(), [1]);
    type_text(ui, ">");
    assert!(
        sent(&app).ends_with("SetAction(1, Right)]"),
        "{}",
        sent(&app)
    );
    ui.invoke_sort("size".into());
    assert!(sent(&app).ends_with("Sort(Size)]"), "{}", sent(&app));
    assert!(app.busy());
}

/// The Expert menu, with `expert`: debugging a module, without the items
/// being scanned again.
#[test]
fn the_expert_menu_changes_what_is_debugged() {
    let work = support::Work::new("window-expert");
    let dir = &work.0;
    synced(dir, &["f"]);
    std::fs::write(dir.join("a/new"), "new in a").unwrap();
    let mut args = roots(dir);
    args.push("-expert".to_owned());
    let (app, out) = support::window(&dir.join("unison"), args);
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(
        wait_until(WAIT, || app.started() && !app.busy()),
        "{}",
        out.text()
    );
    assert!(ui.get_expert() && !ui.get_debug_update());
    ui.invoke_debug("update".into(), true);
    assert!(wait_until(WAIT, || ui.get_debug_update()));
    assert!(!app.busy() && !ui.get_debug_all());
    assert_eq!(app.table().len(), 1);
    ui.invoke_debug("update".into(), false);
    assert!(wait_until(WAIT, || !ui.get_debug_update()));
    assert_eq!(app.table().len(), 1);
    app.quit(0);
}

/// A profile's key loads it (`profileKeymap`), unless its file has gone.
#[test]
fn profile_keys_load_profiles() {
    let work = support::Work::new("window-keys");
    let dir = &work.0;
    synced(dir, &["f"]);
    let roots = format!(
        "root = {}\nroot = {}\nlog = false\n",
        dir.join("a").display(),
        dir.join("b").display()
    );
    std::fs::write(dir.join("unison/p.prf"), &roots).unwrap();
    std::fs::write(dir.join("unison/q.prf"), format!("{roots}key = 2\n")).unwrap();
    let (app, out) = support::window(&dir.join("unison"), vec!["p".to_owned()]);
    let ui = app.ui();
    let _ticks = app.start_ticks();
    let ready = || app.started() && !app.busy() && ui.get_dialog() == 0;
    assert!(wait_until(WAIT, ready), "{}", out.text());
    assert_eq!(ui.get_profile_text(), "Profile: p");
    let keys = ui.get_profile_keys();
    assert_eq!(keys.row_count(), 1);
    assert_eq!(keys.row_data(0).unwrap().name, "q");
    type_text(ui, "2");
    assert!(
        wait_until(WAIT, || ui.get_profile_text() == "Profile: q" && ready()),
        "{}",
        out.text()
    );
    std::fs::remove_file(dir.join("unison/q.prf")).unwrap();
    type_text(ui, "2");
    assert_eq!(ui.get_status_text().trim_end(), "Profile q not found");
    app.quit(0);
}

/// Ignoring the items selected drops them, and writes the patterns to
/// the profile.
#[test]
fn ignoring_drops_the_items() {
    let work = support::Work::new("window-ignore");
    let dir = &work.0;
    synced(dir, &["f"]);
    std::fs::write(dir.join("a/one"), "new in a").unwrap();
    std::fs::write(dir.join("a/two"), "new in a").unwrap();
    let (app, out) = support::window(&dir.join("unison"), roots(dir));
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(
        wait_until(WAIT, || app.started() && !app.busy()),
        "{}",
        out.text()
    );
    ui.invoke_select_all();
    type_text(ui, "I");
    assert!(wait_until(WAIT, || app.table().is_empty() && !app.busy()));
    let profile = std::fs::read_to_string(dir.join("unison/default.prf")).unwrap();
    assert!(
        profile.contains("ignore = Path {one}\nignore = Path {two}"),
        "{profile}"
    );
    app.quit(0);
}

/// Diffs open windows of their own (`diffCmd`'s `messageBox`).
#[cfg(unix)]
#[test]
fn diffs_open_windows_of_their_own() {
    let work = support::Work::new("window-diff");
    let dir = &work.0;
    synced(dir, &["both"]);
    std::fs::write(dir.join("a/both"), "a's").unwrap();
    std::fs::write(dir.join("b/both"), "b's").unwrap();
    let mut args = roots(dir);
    args.extend(["-diff", "echo"].map(String::from));
    let (app, out) = support::window(&dir.join("unison"), args);
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(
        wait_until(WAIT, || app.started() && !app.busy()),
        "{}",
        out.text()
    );
    assert!(ui.get_diff_enabled());
    type_text(ui, "d");
    assert!(
        wait_until(WAIT, || !app.diff_windows().is_empty() && !app.busy()),
        "{}",
        out.text()
    );
    let w = &app.diff_windows()[0];
    assert!(
        w.get_window_title().contains("both"),
        "{}",
        w.get_window_title()
    );
    assert!(w.get_lines().row_count() > 0);
    app.quit(0);
}

/// Replicas `a` and `b` in `dir`, `b` reached over ssh through a shim
/// that asks for a password at its terminal (twice at most) and, once
/// it is `secret`, runs `gemel-gui -server` there: the window's
/// arguments.
#[cfg(unix)]
fn over_ssh_with_a_password(dir: &Path) -> Vec<String> {
    for r in ["a", "b", "unison", "unison-server"] {
        std::fs::create_dir_all(dir.join(r)).unwrap();
    }
    std::fs::write(dir.join("a/f"), "f").unwrap();
    let shim = dir.join("ssh");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\n\
             n=0\n\
             while :; do\n\
               printf \"me@server's password: \" > /dev/tty\n\
               read -r pw < /dev/tty\n\
               [ \"$pw\" = secret ] && break\n\
               n=$((n+1)); [ $n -ge 2 ] && exit 255\n\
               echo 'Permission denied, please try again.' > /dev/tty\n\
             done\n\
             while [ \"$1\" != -e ]; do shift; done; shift 2\n\
             UNISONLOCALHOSTNAME=server UNISON={} HOME={} exec \"$@\"\n",
            dir.join("unison-server").display(),
            dir.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    vec![
        dir.join("a").display().to_string(),
        format!("ssh://server/{}", dir.join("b").display()),
        "-sshcmd".to_owned(),
        shim.display().to_string(),
        "-servercmd".to_owned(),
        support::gemel_gui().display().to_string(),
        "-log=false".to_owned(),
    ]
}

/// ssh's password prompt in a dialog of its own (`createPasswordDialog`):
/// a wrong answer, asked again, then the right one, and the connection.
#[cfg(unix)]
#[test]
fn ssh_asks_for_a_password_in_a_dialog() {
    let work = support::Work::new("window-password");
    let dir = &work.0;
    let args = over_ssh_with_a_password(dir);
    let root = args[1].clone();
    let (app, out) = support::window(&dir.join("unison"), args);
    let ui = app.ui();
    let _ticks = app.start_ticks();
    for answer in ["wrong", "secret"] {
        assert!(
            wait_until(WAIT, || ui.get_dialog() == 5
                && ui.get_dialog_message().ends_with("password: ")),
            "{}",
            out.text()
        );
        assert_eq!(ui.get_password_root(), root.as_str());
        assert!(ui.get_password_editable());
        if answer == "secret" {
            assert!(
                ui.get_dialog_message()
                    .contains("Permission denied, please try again."),
                "{}",
                ui.get_dialog_message()
            );
        }
        ui.set_password_text(answer.into());
        assert!(press(ui, "OK"));
        assert_eq!(ui.get_dialog(), 0);
    }
    // Connected: no archives yet, then the items.
    assert!(wait_until(WAIT, || ui.get_dialog() == 1), "{}", out.text());
    assert_eq!(ui.get_dialog_title(), "Warning");
    assert!(press(ui, "OK"));
    assert!(
        wait_until(WAIT, || app.started() && !app.busy()),
        "{}",
        out.text()
    );
    assert_eq!(app.table().len(), 1);
    app.quit(0);
}

/// Quit in the password dialog ends the program.
#[cfg(unix)]
#[test]
fn quit_in_the_password_dialog_ends_the_program() {
    let work = support::Work::new("window-password-quit");
    let dir = &work.0;
    let args = over_ssh_with_a_password(dir);
    let (app, out) = support::window(&dir.join("unison"), args);
    let ui = app.ui();
    let _ticks = app.start_ticks();
    assert!(wait_until(WAIT, || ui.get_dialog() == 5), "{}", out.text());
    assert!(press(ui, "Quit"));
    assert_eq!(app.exit_code(), Some(0));
}
