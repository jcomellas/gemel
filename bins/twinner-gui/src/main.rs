//! `twinner-gui`: the graphical frontend, built with Slint.
//!
//! Replaces Unison's `src/uigtk3.ml` and `src/uimac/`. Started from the GUI
//! spike in §6 of `docs/unison-port-analysis.md`. The window is described
//! in `ui/app.slint`; this file adapts the recon view model to a Slint
//! [`Model`], drains engine events on a timer and wires the callbacks (menu
//! bar, `rfd` directory picker, SSH-password prompt).
//!
//! There is no engine yet: [`fake_engine`] supplies the rows and progress.
//! Whatever replaces it, this crate talks to the engine only through
//! `twinner-ui-api`.
//!
//! Development flags: `--rows N` (fake items) and `--rate N` (fake progress
//! updates per second).

mod fake_engine;
mod recon_view;
mod task;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelNotify, ModelRc, ModelTracker, SharedString};
use twinner_ui_api::exit_code::ExitCode;

use crate::fake_engine::{Feed, generate_rows};
use crate::recon_view::{ClickMods, Direction, ReconTable, ReplicaStatus, Row, SortKey};

mod ui {
    #![allow(missing_docs, unsafe_code, clippy::all, clippy::todo)]
    slint::include_modules!();
}

use ui::{AppWindow, RowData};

/// Command-line options.
#[derive(Clone, Debug, PartialEq)]
struct Args {
    /// Number of fake recon items.
    rows: usize,
    /// Fake progress updates per second.
    rate: u32,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            rows: 50_000,
            rate: 500,
        }
    }
}

impl Args {
    fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut out = Args::default();
        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            let mut value = |name: &str| {
                it.next()
                    .ok_or_else(|| format!("{name} needs a value"))
                    .and_then(|v| v.parse::<u32>().map_err(|e| format!("{name}: {e}")))
            };
            match arg.as_str() {
                "--rows" => out.rows = value("--rows")? as usize,
                "--rate" => out.rate = value("--rate")?,
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        Ok(out)
    }
}

/// Exposes the [`ReconTable`] to Slint without copying it: rows are built
/// on demand for the visible part of the `ListView`, and changes are
/// reported row by row.
struct RowsModel {
    table: RefCell<ReconTable>,
    notify: ModelNotify,
}

impl RowsModel {
    fn changed(&self, views: impl IntoIterator<Item = usize>) {
        for view in views {
            self.notify.row_changed(view);
        }
    }
}

fn status_kind(status: ReplicaStatus) -> i32 {
    match status {
        ReplicaStatus::Unchanged => 0,
        ReplicaStatus::PropsChanged => 1,
        ReplicaStatus::Modified => 2,
        ReplicaStatus::Created => 3,
        ReplicaStatus::Deleted => 4,
    }
}

fn row_data(row: &Row, selected: bool) -> RowData {
    RowData {
        path: row.path.as_str().into(),
        left: row.left.label().into(),
        left_kind: status_kind(row.left),
        right: row.right.label().into(),
        right_kind: status_kind(row.right),
        arrow: row.direction.arrow().into(),
        direction_kind: match row.direction {
            Direction::Conflict => 0,
            Direction::Skip => 1,
            Direction::LeftToRight => 2,
            Direction::RightToLeft => 3,
        },
        size: row.size_label().into(),
        progress: row.progress,
        selected,
        spoken: row.spoken().into(),
    }
}

impl Model for RowsModel {
    type Data = RowData;

    fn row_count(&self) -> usize {
        self.table.borrow().len()
    }

    fn row_data(&self, view: usize) -> Option<RowData> {
        let table = self.table.borrow();
        (view < table.len()).then(|| row_data(table.row_at(view), table.is_selected(view)))
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

struct App {
    ui: AppWindow,
    model: Rc<RowsModel>,
    feed: Feed,
}

impl App {
    fn new(args: &Args) -> Result<Rc<Self>, slint::PlatformError> {
        let ui = AppWindow::new()?;
        let mut table = ReconTable::new(generate_rows(args.rows));
        table.sort_with(SortKey::Path, true);
        let model = Rc::new(RowsModel {
            table: RefCell::new(table),
            notify: ModelNotify::default(),
        });
        ui.set_rows(ModelRc::from(model.clone()));
        let feed = Feed::spawn(args.rows, args.rate);
        let app = Rc::new(App { ui, model, feed });
        app.wire();
        app.refresh_header();
        app.refresh_status("");
        Ok(app)
    }

    fn refresh_header(&self) {
        let (key, asc) = self
            .model
            .table
            .borrow()
            .sort()
            .unwrap_or((SortKey::Path, true));
        self.ui.set_sort_key(
            match key {
                SortKey::Path => "path",
                SortKey::Status => "status",
                SortKey::Size => "size",
            }
            .into(),
        );
        self.ui.set_sort_ascending(asc);
    }

    fn refresh_status(&self, extra: &str) {
        let table = self.model.table.borrow();
        let done = (0..table.len())
            .filter(|&v| table.row_at(v).progress >= 1.0)
            .count();
        self.ui.set_status_text(SharedString::from(format!(
            "{} items, {} selected, {} transferred. {extra}",
            table.len(),
            table.selected_count(),
            done
        )));
    }

    fn set_direction(&self, d: Direction) {
        let changed = self.model.table.borrow_mut().set_direction(d);
        self.model.changed(changed);
    }

    fn sort(&self, key: SortKey) {
        let cursor = {
            let mut table = self.model.table.borrow_mut();
            table.sort_by(key);
            table.cursor()
        };
        self.model.notify.reset();
        self.refresh_header();
        if let Some(c) = cursor {
            self.ui.invoke_bring_into_view(c as i32);
        }
    }

    fn wire(self: &Rc<Self>) {
        let ui = &self.ui;
        let weak = Rc::downgrade(self);
        let with = move |f: &dyn Fn(&App)| {
            if let Some(s) = weak.upgrade() {
                f(&s);
            }
        };

        let w = with.clone();
        ui.on_row_clicked(move |view, shift, command| {
            w(&|s| {
                let changed = s
                    .model
                    .table
                    .borrow_mut()
                    .click(view as usize, ClickMods { shift, command });
                s.model.changed(changed);
                s.refresh_status("");
            })
        });
        let w = with.clone();
        ui.on_move_cursor(move |delta, extend| {
            w(&|s| {
                let (cursor, changed) = s
                    .model
                    .table
                    .borrow_mut()
                    .move_cursor(delta as isize, extend);
                s.model.changed(changed);
                if let Some(c) = cursor {
                    s.ui.invoke_bring_into_view(c as i32);
                }
                s.refresh_status("");
            })
        });
        let w = with.clone();
        ui.on_direction_key(move |key| {
            w(&|s| {
                // Native menu shortcuts bypass Slint focus: ignore them
                // while the modal is up.
                if s.ui.get_ssh_open() {
                    return;
                }
                if let Some(d) = Direction::from_key(&key) {
                    s.set_direction(d);
                }
            })
        });
        let w = with.clone();
        ui.on_select_all(move || {
            w(&|s| {
                if s.ui.get_ssh_open() {
                    return;
                }
                let changed = s.model.table.borrow_mut().select_all();
                s.model.changed(changed);
                s.refresh_status("");
            })
        });
        let w = with.clone();
        ui.on_sort(move |key| {
            w(&|s| {
                if s.ui.get_ssh_open() {
                    return;
                }
                s.sort(match key.as_str() {
                    "status" => SortKey::Status,
                    "size" => SortKey::Size,
                    _ => SortKey::Path,
                })
            })
        });
        let w = with.clone();
        ui.on_choose_root(move || {
            w(&|s| {
                if s.ui.get_ssh_open() {
                    return;
                }
                // With a parent window this is a sheet on macOS, so the
                // event loop (and the progress feed) keeps running.
                let dialog = rfd::AsyncFileDialog::new()
                    .set_title("Choose the local root")
                    .set_parent(&s.ui.window().window_handle())
                    .pick_folder();
                let ui = s.ui.as_weak();
                task::spawn(dialog, move |picked| {
                    if let Some(path) = picked.map(|h| h.path().display().to_string()) {
                        let _ = ui.upgrade_in_event_loop(move |ui| ui.set_root_text(path.into()));
                    }
                });
            })
        });
        let w = with.clone();
        ui.on_connect_ssh(move || {
            w(&|s| {
                s.ui.set_ssh_password(SharedString::new());
                s.ui.set_ssh_open(true);
            })
        });
        let w = with;
        ui.on_ssh_done(move |ok| {
            w(&|s| {
                let msg = if ok {
                    format!(
                        "ssh: sent a {}-char password",
                        s.ui.get_ssh_password().chars().count()
                    )
                } else {
                    "ssh: cancelled".to_string()
                };
                s.ui.set_ssh_password(SharedString::new());
                s.ui.set_ssh_open(false);
                s.ui.invoke_focus_table();
                s.refresh_status(&msg);
            })
        });
        ui.set_root_text("(none)".into());
    }

    /// Applies whatever the engine sent since the last tick.
    fn pump(&self) {
        let updates = self.feed.drain();
        let changed = self.model.table.borrow_mut().apply_progress(&updates);
        self.model.changed(changed);
    }
}

fn run(args: &Args) -> Result<(), slint::PlatformError> {
    let app = App::new(args)?;
    app.ui.invoke_focus_table();

    // ~60 Hz: drain engine events and refresh the status line a few times a
    // second.
    let timer = slint::Timer::default();
    let weak = Rc::downgrade(&app);
    let mut tick = 0u64;
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(16),
        move || {
            let Some(app) = weak.upgrade() else { return };
            app.pump();
            tick += 1;
            if tick.is_multiple_of(15) {
                app.refresh_status("");
            }
        },
    );
    app.ui.run()
}

fn main() -> std::process::ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("twinner-gui: {e}");
            eprintln!("usage: twinner-gui [--rows N] [--rate UPDATES_PER_SEC]");
            return ExitCode::Fatal.into();
        }
    };
    match run(&args) {
        Ok(()) => ExitCode::Perfect.into(),
        Err(e) => {
            eprintln!("twinner-gui: {e}");
            ExitCode::Fatal.into()
        }
    }
}

#[cfg(test)]
mod tests {
    //! Drives the real UI on Slint's headless testing backend with
    //! synthetic input, and checks the accessibility properties that Slint's
    //! AccessKit bridge turns into what VoiceOver/Narrator/AT-SPI see.

    use super::*;
    use i_slint_backend_testing::{AccessibleRole, ElementQuery};
    use slint::platform::{Key, PointerEventButton, WindowEvent};
    use slint::{LogicalPosition, LogicalSize};

    struct Item {
        label: String,
        selected: bool,
        index: usize,
        centre: LogicalPosition,
    }

    fn items(ui: &AppWindow) -> Vec<Item> {
        let mut out: Vec<Item> = ElementQuery::from_root(ui)
            .match_descendants()
            .match_accessible_role(AccessibleRole::ListItem)
            .find_all()
            .into_iter()
            .map(|e| {
                let pos = e.absolute_position();
                let size = e.size();
                Item {
                    label: e.accessible_label().unwrap_or_default().to_string(),
                    selected: e.accessible_item_selected().unwrap_or(false),
                    index: e.accessible_item_index().expect("item index"),
                    centre: LogicalPosition::new(
                        pos.x + size.width / 2.0,
                        pos.y + size.height / 2.0,
                    ),
                }
            })
            .collect();
        // The query yields one handle per item-tree node that makes up the
        // row element, so the same row shows up several times.
        out.sort_by_key(|i| i.index);
        out.dedup_by_key(|i| i.index);
        out
    }

    fn click(ui: &AppWindow, at: LogicalPosition, modifier: Option<Key>) {
        let w = ui.window();
        if let Some(k) = modifier {
            w.dispatch_event(WindowEvent::KeyPressed { text: k.into() });
        }
        w.dispatch_event(WindowEvent::PointerMoved { position: at });
        w.dispatch_event(WindowEvent::PointerPressed {
            position: at,
            button: PointerEventButton::Left,
        });
        w.dispatch_event(WindowEvent::PointerReleased {
            position: at,
            button: PointerEventButton::Left,
        });
        if let Some(k) = modifier {
            w.dispatch_event(WindowEvent::KeyReleased { text: k.into() });
        }
    }

    fn type_text(ui: &AppWindow, text: &str) {
        let w = ui.window();
        w.dispatch_event(WindowEvent::KeyPressed { text: text.into() });
        w.dispatch_event(WindowEvent::KeyReleased { text: text.into() });
    }

    #[test]
    fn parses_flags() {
        let args = Args::parse(["--rows", "10", "--rate", "5000"].map(String::from)).unwrap();
        assert_eq!(
            args,
            Args {
                rows: 10,
                rate: 5000
            }
        );
        assert!(Args::parse(["--nope".to_string()]).is_err());
        assert!(Args::parse(["--rows".to_string()]).is_err());
    }

    #[test]
    fn table_is_virtualised_selectable_and_accessible() {
        i_slint_backend_testing::init_no_event_loop();
        let args = Args {
            rows: 50_000,
            rate: 0,
        };
        let app = App::new(&args).unwrap();
        let ui = &app.ui;
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
        let first = app.model.table.borrow().row_at(0).path.clone();
        assert!(rows[0].label.starts_with(&first), "{}", rows[0].label);
        assert_eq!(rows[0].index, 0);

        // Click row 0, shift-click row 3, cmd-click row 1: rows 0, 2, 3.
        // (Slint reports Cmd as `control` on macOS.)
        click(ui, rows[0].centre, None);
        click(ui, rows[3].centre, Some(Key::Shift));
        click(ui, rows[1].centre, Some(Key::Control));
        let selected: Vec<usize> = {
            let t = app.model.table.borrow();
            (0..t.len()).filter(|&v| t.is_selected(v)).collect()
        };
        assert_eq!(selected, [0, 2, 3]);

        // `>` applies to the whole selection; the tree reports selection.
        type_text(ui, ">");
        {
            let t = app.model.table.borrow();
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
        assert_eq!(
            app.model.table.borrow().row_at(0).direction,
            Direction::LeftToRight
        );
        assert_eq!(ui.get_ssh_password(), "<");
    }
}
