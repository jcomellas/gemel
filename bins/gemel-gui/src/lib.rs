//! `gemel-gui`: the graphical frontend, built with Slint.
//!
//! Replaces Unison's `src/uigtk3.ml` and `src/uimac/`. Started from the GUI
//! spike in §6 of `docs/unison-port-analysis.md`. The window is described
//! in `ui/app.slint`; this file adapts the recon view model to a Slint
//! [`Model`], drains engine events on a timer and wires the callbacks (menu
//! bar, `rfd` directory picker, SSH-password prompt).
//!
//! It is a whole gemel, as `unison-gui` is a whole unison: the command
//! line is Unison's, and `gemel`'s library runs it. `-ui text` runs the
//! text interface, and so does a system without a display; `-server` and
//! `-socket` serve.
//!
//! There is no engine yet: [`fake_engine`] supplies the rows and progress,
//! and the window takes no roots or profile from the command line.
//! Whatever replaces it, this crate talks to the engine only through
//! `gemel-ui-api`.
//!
//! Development flags, taken out of the command line before Unison reads
//! it: `--rows N` (fake items) and `--rate N` (fake progress updates per
//! second).

mod fake_engine;
pub mod recon_view;
mod task;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gemel_ui_api::exit_code::ExitCode;
use slint::{ComponentHandle, Model, ModelNotify, ModelRc, ModelTracker, SharedString};

use crate::fake_engine::{Feed, generate_rows};
use crate::recon_view::{ClickMods, Direction, ReconTable, ReplicaStatus, Row, SortKey};

/// The window, compiled from `ui/app.slint`.
pub mod ui {
    #![allow(missing_docs, unsafe_code, clippy::all, clippy::todo)]
    slint::include_modules!();
}

use ui::{AppWindow, RowData};

/// The development flags.
#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    /// Number of fake recon items.
    pub rows: usize,
    /// Fake progress updates per second.
    pub rate: u32,
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
    /// Takes the development flags out of `args`: them, and the rest.
    fn split<I: IntoIterator<Item = String>>(args: I) -> Result<(Self, Vec<String>), String> {
        let mut out = Args::default();
        let mut rest = Vec::new();
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
                _ => rest.push(arg),
            }
        }
        Ok((out, rest))
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

/// The graphical interface: the window and what it shows.
pub struct App {
    ui: AppWindow,
    model: Rc<RowsModel>,
    feed: Feed,
}

impl App {
    /// The window, with the fake items and progress `args` asks for.
    pub fn new(args: &Args) -> Result<Rc<Self>, slint::PlatformError> {
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

    /// The window.
    pub fn ui(&self) -> &AppWindow {
        &self.ui
    }

    /// The items, as the window shows them.
    pub fn table(&self) -> std::cell::Ref<'_, ReconTable> {
        self.model.table.borrow()
    }

    /// Starts the timer that applies what the engine sends (~60 Hz) and
    /// refreshes the status line a few times a second; it runs while the
    /// timer is kept.
    pub fn start_ticks(self: &Rc<Self>) -> slint::Timer {
        let timer = slint::Timer::default();
        let weak = Rc::downgrade(self);
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
        timer
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
    let _ticks = app.start_ticks();
    app.ui.run()
}

/// Runs the program on its command line, with the window as its
/// graphical interface: the exit code.
pub fn main() -> std::process::ExitCode {
    let (dev, args) = match Args::split(std::env::args().skip(1)) {
        Ok(split) => split,
        Err(e) => {
            eprintln!("gemel-gui: {e}");
            return ExitCode::Fatal.into();
        }
    };
    gemel::main(args, Some(&|_| graphical(&dev)))
}

/// The graphical interface: the exit code.
fn graphical(dev: &Args) -> i32 {
    match run(dev) {
        Ok(()) => ExitCode::Perfect as i32,
        Err(e) => {
            eprintln!("gemel-gui: {e}");
            ExitCode::Fatal as i32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_flags_are_taken_out() {
        let (args, rest) =
            Args::split(["-batch", "--rows", "10", "a", "--rate", "5000"].map(String::from))
                .unwrap();
        assert_eq!(
            args,
            Args {
                rows: 10,
                rate: 5000
            }
        );
        assert_eq!(rest, ["-batch", "a"]);
        assert!(Args::split(["--rows".to_string()]).is_err());
        assert!(Args::split(["--rate".to_string(), "x".to_string()]).is_err());
    }
}
