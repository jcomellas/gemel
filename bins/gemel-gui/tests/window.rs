//! Drives the real window on Slint's headless testing backend with
//! synthetic input, and checks the accessibility properties that Slint's
//! AccessKit bridge turns into what VoiceOver/Narrator/AT-SPI see.

mod support;

use gemel_gui::recon_view::Direction;
use gemel_gui::{App, Args};
use i_slint_backend_testing::{AccessibleRole, ElementQuery};
use slint::platform::Key;
use slint::{ComponentHandle, LogicalSize};
use std::time::Duration;

use support::{click, init, items, type_text, wait_until};

#[test]
fn table_is_virtualised_selectable_and_accessible() {
    init();
    let args = Args {
        rows: 50_000,
        rate: 0,
    };
    let app = App::new(&args).unwrap();
    let ui = app.ui();
    ui.window().set_size(LogicalSize::new(1000.0, 700.0));
    ui.show().unwrap();

    let list = ElementQuery::from_root(ui)
        .match_descendants()
        .match_accessible_role(AccessibleRole::List)
        .find_first()
        .expect("list");
    assert_eq!(list.accessible_item_count(), Some(50_000));

    // Only the visible rows are instantiated.
    let rows = items(ui);
    assert!((10..100).contains(&rows.len()), "{} row items", rows.len());
    let first = app.table().row_at(0).path.clone();
    assert!(rows[0].label.starts_with(&first), "{}", rows[0].label);
    assert_eq!(rows[0].index, 0);

    // Click row 0, shift-click row 3, cmd-click row 1: rows 0, 2, 3.
    // (Slint reports Cmd as `control` on macOS.)
    click(ui, rows[0].centre, None);
    click(ui, rows[3].centre, Some(Key::Shift));
    click(ui, rows[1].centre, Some(Key::Control));
    let selected: Vec<usize> = {
        let t = app.table();
        (0..t.len()).filter(|&v| t.is_selected(v)).collect()
    };
    assert_eq!(selected, [0, 2, 3]);

    // `>` applies to the whole selection; the tree reports selection.
    type_text(ui, ">");
    {
        let t = app.table();
        for v in [0, 2, 3] {
            assert_eq!(t.row_at(v).direction, Direction::LeftToRight);
        }
    }
    let rows = items(ui);
    let flags: Vec<bool> = rows.iter().take(4).map(|r| r.selected).collect();
    assert_eq!(flags, [true, false, true, true]);
    assert!(rows[0].label.contains("left to right"), "{}", rows[0].label);

    // The direction keys go to the password field, not the table,
    // while the SSH modal is open.
    ui.invoke_connect_ssh();
    type_text(ui, "<");
    assert_eq!(app.table().row_at(0).direction, Direction::LeftToRight);
    assert_eq!(ui.get_ssh_password(), "<");
}

/// The window's timer applies what another thread sends as the harness
/// moves the mock clock on.
#[test]
fn progress_from_another_thread_arrives_as_time_passes() {
    init();
    let args = Args {
        rows: 20,
        rate: 2_000,
    };
    let app = App::new(&args).unwrap();
    app.ui().show().unwrap();
    let progressed = |app: &App| {
        let t = app.table();
        (0..t.len()).any(|v| t.row_at(v).progress > 0.0)
    };
    assert!(!progressed(&app));
    let _ticks = app.start_ticks();
    assert!(wait_until(Duration::from_secs(10), || progressed(&app)));
}
