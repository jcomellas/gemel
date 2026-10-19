//! Throwaway GUI spike: the recon-table slice in Slint 1.18.
//!
//! See §6 of `docs/unison-port-analysis.md`. The UI is in `ui/app.slint`;
//! this file adapts the shared model from `twinner-gui-spike-model` to a
//! Slint [`Model`], drains the fake engine feed on a timer and wires the
//! callbacks (menu bar, `rfd` directory picker, SSH-password modal).
//!
//! Flags: `--rows N`, `--rate N`, `--bench SECS`, `--exit-after-first-frame`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelNotify, ModelRc, ModelTracker, SharedString};
use twinner_gui_spike_model::args::SpikeArgs;
use twinner_gui_spike_model::engine::{Feed, generate_rows};
use twinner_gui_spike_model::stats::{FrameStats, Op, headless_bench};
use twinner_gui_spike_model::table::{
    ClickMods, Direction, ReconTable, ReplicaStatus, Row, SortKey,
};

mod ui {
    #![allow(missing_docs, unsafe_code, clippy::all, clippy::todo)]
    slint::include_modules!();
}

use ui::{AppWindow, RowData};

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

struct Spike {
    ui: AppWindow,
    model: Rc<RowsModel>,
    feed: Feed,
}

impl Spike {
    fn new(args: &SpikeArgs) -> Result<Rc<Self>, slint::PlatformError> {
        let ui = AppWindow::new()?;
        let mut table = ReconTable::new(generate_rows(args.rows));
        table.sort_with(SortKey::Path, true);
        let model = Rc::new(RowsModel {
            table: RefCell::new(table),
            notify: ModelNotify::default(),
        });
        ui.set_rows(ModelRc::from(model.clone()));
        // The UI drains the feed on a timer, so no wake-up is needed.
        let feed = Feed::spawn(args.rows, args.rate, || {});
        let spike = Rc::new(Spike { ui, model, feed });
        spike.wire();
        spike.refresh_header();
        spike.refresh_status("");
        Ok(spike)
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
        let with = move |f: &dyn Fn(&Spike)| {
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
                twinner_gui_spike_model::task::spawn(dialog, move |picked| {
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
    fn pump(&self) -> usize {
        let updates = self.feed.drain();
        let changed = self.model.table.borrow_mut().apply_progress(&updates);
        self.model.changed(changed);
        updates.len()
    }
}

/// `--bench SECS --headless`: no window or GPU. Installs a platform with
/// Slint's software renderer and times the main-thread work per frame
/// (engine updates, model notifications, layout and rendering into a
/// 1000×700 buffer). Rendering in software makes this an upper bound: the
/// windowed app renders with femtovg on the GPU.
fn run_headless(args: &SpikeArgs, limit: Duration) -> Result<(), slint::PlatformError> {
    use slint::platform::software_renderer::{
        MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
    };

    struct Headless {
        window: Rc<MinimalSoftwareWindow>,
        start: Instant,
    }

    impl slint::platform::Platform for Headless {
        fn create_window_adapter(
            &self,
        ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(self.window.clone())
        }

        fn duration_since_start(&self) -> Duration {
            self.start.elapsed()
        }
    }

    const W: u32 = 1000;
    const H: u32 = 700;
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(Headless {
        window: window.clone(),
        start: Instant::now(),
    }))
    .expect("no platform set yet");
    let spike = Spike::new(args)?;
    window.set_size(slint::PhysicalSize::new(W, H));
    spike.ui.show()?;
    let mut buffer = vec![PremultipliedRgbaColor::default(); (W * H) as usize];
    let mut draw = || {
        slint::platform::update_timers_and_animations();
        window.draw_if_needed(|renderer| {
            renderer.render(&mut buffer, W as usize);
        });
    };
    draw();
    draw();
    let rows = spike.model.row_count().max(1);
    let mut row = 0;
    let report = headless_bench("slint", limit, |op| {
        match op {
            Some(Op::SelectAll) => spike.ui.invoke_select_all(),
            Some(Op::SortByStatus) => spike.ui.invoke_sort("status".into()),
            Some(Op::DirectionAll) => spike.ui.invoke_direction_key(">".into()),
            None => {}
        }
        let applied = spike.pump();
        row = (row + 7) % rows;
        spike.ui.invoke_scroll_to(row as i32);
        draw();
        applied
    });
    eprintln!("{report}");
    Ok(())
}

fn main() -> Result<(), slint::PlatformError> {
    let process_start = Instant::now();
    let args = SpikeArgs::from_env();
    if let (true, Some(limit)) = (args.headless, args.bench) {
        return run_headless(&args, limit);
    }
    let spike = Spike::new(&args)?;
    spike.ui.invoke_focus_table();
    spike.ui.set_on_top(args.bench.is_some());

    let frames = Rc::new(RefCell::new(FrameStats::default()));
    let bench_start: Rc<Cell<Option<Instant>>> = Rc::default();
    let first_frame = Rc::new(Cell::new(true));
    {
        let frames = frames.clone();
        let bench_start = bench_start.clone();
        let exit_early = args.exit_after_first_frame;
        let bench = args.bench.is_some();
        let spike_weak = Rc::downgrade(&spike);
        let notifier = spike.ui.window().set_rendering_notifier(move |state, _| {
            if !matches!(state, slint::RenderingState::AfterRendering) {
                return;
            }
            if first_frame.replace(false) {
                eprintln!(
                    "[slint] first frame after {:.1} ms",
                    process_start.elapsed().as_secs_f64() * 1000.0
                );
                if exit_early {
                    let _ = slint::quit_event_loop();
                }
                if bench && let Some(s) = spike_weak.upgrade() {
                    let mut table = s.model.table.borrow_mut();
                    let t = Instant::now();
                    table.sort_with(SortKey::Status, true);
                    let status = t.elapsed();
                    let t = Instant::now();
                    table.sort_with(SortKey::Path, true);
                    eprintln!(
                        "[slint] sort {} rows: by status {:.1} ms, by path {:.1} ms",
                        table.len(),
                        status.as_secs_f64() * 1000.0,
                        t.elapsed().as_secs_f64() * 1000.0
                    );
                    bench_start.set(Some(Instant::now()));
                }
            }
            if bench_start.get().is_some() {
                frames.borrow_mut().frame();
            }
        });
        if let Err(e) = notifier {
            eprintln!("[slint] no rendering notifier ({e}); frame stats unavailable");
        }
    }

    // ~60 Hz: drain the feed, refresh the status line a few times a second,
    // and auto-scroll during --bench.
    let timer = slint::Timer::default();
    {
        let spike_weak = Rc::downgrade(&spike);
        let frames = frames.clone();
        let limit = args.bench;
        let mut tick = 0u64;
        let mut bench_row = 0usize;
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(16),
            move || {
                let Some(s) = spike_weak.upgrade() else {
                    return;
                };
                frames.borrow_mut().updates_applied += s.pump() as u64;
                tick += 1;
                if tick.is_multiple_of(15) {
                    s.refresh_status("");
                }
                if let (Some(start), Some(limit)) = (bench_start.get(), limit) {
                    let rows = s.model.row_count().max(1);
                    bench_row = (bench_row + 7) % rows;
                    s.ui.invoke_scroll_to(bench_row as i32);
                    if start.elapsed() >= limit {
                        eprintln!("{}", frames.borrow().report("slint", start.elapsed()));
                        bench_start.set(None);
                        let _ = slint::quit_event_loop();
                    }
                }
            },
        );
    }

    spike.ui.run()
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
    fn table_is_virtualised_selectable_and_accessible() {
        i_slint_backend_testing::init_no_event_loop();
        let args = SpikeArgs {
            rows: 50_000,
            rate: 0,
            ..SpikeArgs::default()
        };
        let spike = Spike::new(&args).unwrap();
        let ui = &spike.ui;
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
        let first = spike.model.table.borrow().row_at(0).path.clone();
        assert!(rows[0].label.starts_with(&first), "{}", rows[0].label);
        assert_eq!(rows[0].index, 0);

        // Click row 0, shift-click row 3, cmd-click row 1: rows 0, 2, 3.
        // (Slint reports Cmd as `control` on macOS.)
        click(ui, rows[0].centre, None);
        click(ui, rows[3].centre, Some(Key::Shift));
        click(ui, rows[1].centre, Some(Key::Control));
        let selected: Vec<usize> = {
            let t = spike.model.table.borrow();
            (0..t.len()).filter(|&v| t.is_selected(v)).collect()
        };
        assert_eq!(selected, [0, 2, 3]);

        // `>` applies to the whole selection; the tree reports selection.
        type_text(ui, ">");
        {
            let t = spike.model.table.borrow();
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
            spike.model.table.borrow().row_at(0).direction,
            Direction::LeftToRight
        );
        assert_eq!(ui.get_ssh_password(), "<");
    }
}
