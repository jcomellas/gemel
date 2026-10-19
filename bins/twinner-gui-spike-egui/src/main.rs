//! Throwaway GUI spike: the recon-table slice in egui/eframe 0.36.
//!
//! See §6 of `docs/unison-port-analysis.md`. Renders the shared model from
//! `twinner-gui-spike-model`: a 50k-row recon table with multi-select,
//! sorting, status icons and `>` `<` `/`, a live progress feed, a native
//! macOS menu bar (via `muda`, which eframe lacks), an `rfd` directory
//! picker and a modal SSH-password prompt.
//!
//! Flags: `--rows N`, `--rate N`, `--bench SECS`, `--exit-after-first-frame`.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use eframe::egui::{
    self, Align, Color32, Event, Key, Modal, Sense, Ui, WidgetInfo, WidgetType, accesskit,
};
use egui_extras::{Column, TableBuilder};
use twinner_gui_spike_model::args::SpikeArgs;
use twinner_gui_spike_model::engine::{Feed, generate_rows};
use twinner_gui_spike_model::stats::{FrameStats, Op, headless_bench};
use twinner_gui_spike_model::table::{
    ClickMods, Direction, ReconTable, ReplicaStatus, Row, SortKey,
};

const ROW_HEIGHT: f32 = 20.0;

#[cfg(target_os = "macos")]
type NativeMenu = muda::Menu;
#[cfg(not(target_os = "macos"))]
type NativeMenu = ();

/// Things the menu bar, the dialog thread and the UI ask the app to do.
// Some variants are only sent by the macOS menu bar.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug)]
enum Command {
    ChooseRoot,
    RootChosen(Option<PathBuf>),
    ConnectSsh,
    SelectAll,
    Direction(Direction),
    Sort(SortKey),
}

struct SshPrompt {
    password: String,
    focus_requested: bool,
}

struct SpikeApp {
    args: SpikeArgs,
    process_start: Instant,
    table: ReconTable,
    feed: Feed,
    commands: Receiver<Command>,
    command_tx: Sender<Command>,
    /// Keeps the native menu alive.
    _menu: Option<NativeMenu>,
    root: Option<PathBuf>,
    ssh: Option<SshPrompt>,
    ssh_result: String,
    scroll_to: Option<usize>,
    frames: FrameStats,
    first_frame_done: bool,
    bench_start: Option<Instant>,
    bench_row: usize,
}

impl SpikeApp {
    fn new(cc: &eframe::CreationContext<'_>, args: SpikeArgs, process_start: Instant) -> Self {
        let mut app = Self::headless(&cc.egui_ctx, args, process_start);
        app._menu = build_menu(&cc.egui_ctx, app.command_tx.clone());
        app
    }

    /// Everything but the native menu, so tests can drive a bare
    /// `egui::Context`.
    fn headless(ctx: &egui::Context, args: SpikeArgs, process_start: Instant) -> Self {
        let (command_tx, commands) = channel();
        let repaint = ctx.clone();
        let feed = Feed::spawn(args.rows, args.rate, move || repaint.request_repaint());
        let mut table = ReconTable::new(generate_rows(args.rows));
        table.sort_with(SortKey::Path, true);
        SpikeApp {
            args,
            process_start,
            table,
            feed,
            commands,
            command_tx,
            _menu: None,
            root: None,
            ssh: None,
            ssh_result: String::new(),
            scroll_to: None,
            frames: FrameStats::default(),
            first_frame_done: false,
            bench_start: None,
            bench_row: 0,
        }
    }

    fn run_command(&mut self, cmd: Command, frame: Option<&eframe::Frame>, ctx: &egui::Context) {
        match cmd {
            Command::ChooseRoot => {
                let mut dialog = rfd::AsyncFileDialog::new().set_title("Choose the local root");
                // With a parent window this is a sheet on macOS, so the event
                // loop (and the progress feed) keeps running.
                if let Some(frame) = frame {
                    dialog = dialog.set_parent(frame);
                }
                let dialog = dialog.pick_folder();
                let tx = self.command_tx.clone();
                let ctx = ctx.clone();
                twinner_gui_spike_model::task::spawn(dialog, move |picked| {
                    let _ = tx.send(Command::RootChosen(picked.map(|h| h.path().to_owned())));
                    ctx.request_repaint();
                });
            }
            Command::RootChosen(path) => {
                if path.is_some() {
                    self.root = path;
                }
            }
            Command::ConnectSsh => {
                self.ssh = Some(SshPrompt {
                    password: String::new(),
                    focus_requested: false,
                });
            }
            Command::SelectAll => {
                self.table.select_all();
            }
            Command::Direction(d) => {
                self.table.set_direction(d);
            }
            Command::Sort(key) => {
                self.table.sort_by(key);
                self.scroll_to = self.table.cursor();
            }
        }
    }

    /// Table keyboard handling: `>` `<` `/`, arrows (with shift), cmd-A.
    fn handle_keys(&mut self, ui: &Ui) {
        let events = ui.input(|i| i.events.clone());
        for event in events {
            match event {
                Event::Text(text) => {
                    if let Some(d) = Direction::from_key(&text) {
                        self.table.set_direction(d);
                    }
                }
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let delta = match key {
                        Key::ArrowDown => 1,
                        Key::ArrowUp => -1,
                        Key::PageDown => 25,
                        Key::PageUp => -25,
                        Key::A if modifiers.command => {
                            self.table.select_all();
                            continue;
                        }
                        _ => continue,
                    };
                    let (cursor, _) = self.table.move_cursor(delta, modifiers.shift);
                    self.scroll_to = cursor;
                }
                _ => {}
            }
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, frame: Option<&eframe::Frame>) {
        ui.horizontal(|ui| {
            if ui.button("Choose root…").clicked() {
                self.run_command(Command::ChooseRoot, frame, ui.ctx());
            }
            if ui.button("Connect over SSH…").clicked() {
                self.run_command(Command::ConnectSsh, frame, ui.ctx());
            }
            ui.separator();
            for (label, d) in [
                ("Left to right  >", Direction::LeftToRight),
                ("Right to left  <", Direction::RightToLeft),
                ("Skip  /", Direction::Skip),
            ] {
                if ui.button(label).clicked() {
                    self.table.set_direction(d);
                }
            }
            ui.separator();
            ui.label(format!(
                "root: {}",
                self.root
                    .as_deref()
                    .map_or("(none)".into(), |p| p.display().to_string())
            ));
        });
    }

    fn status_bar(&self, ui: &mut Ui) {
        let done = (0..self.table.len())
            .filter(|&v| self.table.row_at(v).progress >= 1.0)
            .count();
        ui.label(format!(
            "{} items, {} selected, {} transferred. {}",
            self.table.len(),
            self.table.selected_count(),
            done,
            self.ssh_result
        ));
    }

    fn recon_table(&mut self, ui: &mut Ui) {
        let sort = self.table.sort();
        let mut sort_clicked = None;
        let mut clicked = None;
        let mods = ui.input(|i| ClickMods {
            shift: i.modifiers.shift,
            command: i.modifiers.command,
        });
        let mut builder = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .animate_scrolling(false)
            .column(Column::remainder().at_least(240.0).clip(true))
            .column(Column::exact(80.0))
            .column(Column::exact(56.0))
            .column(Column::exact(80.0))
            .column(Column::exact(80.0))
            .column(Column::exact(110.0));
        if let Some(row) = self.scroll_to.take() {
            builder = builder.scroll_to_row(row, None);
        }
        if self.bench_start.is_some() {
            // Auto-scroll through the whole table during --bench.
            self.bench_row = (self.bench_row + 7) % self.table.len().max(1);
            builder = builder.scroll_to_row(self.bench_row, Some(Align::TOP));
        }
        let header = |ui: &mut Ui, label: &str, key: SortKey, out: &mut Option<SortKey>| {
            let arrow = match sort {
                // egui's bundled fonts have no ▲/▼ or ↑/↓.
                Some((k, true)) if k == key => " ⬆",
                Some((k, false)) if k == key => " ⬇",
                _ => "",
            };
            if ui
                .add(
                    egui::Button::new(egui::RichText::new(format!("{label}{arrow}")).strong())
                        .frame(false),
                )
                .clicked()
            {
                *out = Some(key);
            }
        };
        let table = &self.table;
        builder
            .header(ROW_HEIGHT + 2.0, |mut h| {
                h.col(|ui| header(ui, "Path", SortKey::Path, &mut sort_clicked));
                h.col(|ui| header(ui, "Left", SortKey::Status, &mut sort_clicked));
                h.col(|ui| header(ui, "Action", SortKey::Status, &mut sort_clicked));
                h.col(|ui| header(ui, "Right", SortKey::Status, &mut sort_clicked));
                h.col(|ui| header(ui, "Size", SortKey::Size, &mut sort_clicked));
                h.col(|ui| {
                    ui.strong("Progress");
                });
            })
            .body(|mut body| {
                let ctx = body.ui_mut().ctx().clone();
                let len = table.len();
                let mut list_parent = None;
                body.rows(ROW_HEIGHT, len, |mut row| {
                    let view = row.index();
                    let item = table.row_at(view);
                    let selected = table.is_selected(view);
                    row.set_selected(selected);
                    let (_, path_cell) = row.col(|ui| {
                        if list_parent.is_none() {
                            list_parent = ui.stack().parent.as_ref().map(|p| p.id);
                        }
                        ui.add(egui::Label::new(&item.path).truncate().selectable(false));
                    });
                    let other_cells = [
                        row.col(|ui| status_cell(ui, item.left)).1,
                        row.col(|ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(item.direction.arrow())
                                        .monospace()
                                        .color(direction_color(item.direction)),
                                )
                                .selectable(false),
                            );
                        })
                        .1,
                        row.col(|ui| status_cell(ui, item.right)).1,
                        row.col(|ui| {
                            ui.add(egui::Label::new(item.size_label()).selectable(false));
                        })
                        .1,
                        row.col(|ui| progress_cell(ui, item)).1,
                    ];
                    // The row item's label already says all of this: keep
                    // screen readers from stepping through every cell.
                    for cell in other_cells {
                        cell.ctx
                            .accesskit_node_builder(cell.id, |node| node.set_hidden());
                    }
                    // Expose the row to screen readers as one selectable item
                    // with a spoken summary.
                    path_cell.widget_info(|| {
                        WidgetInfo::selected(
                            WidgetType::SelectableLabel,
                            true,
                            selected,
                            item.spoken(),
                        )
                    });
                    // egui maps `selected` to "toggled" (a button state); a
                    // list item needs the ARIA-style selected flag and its
                    // position in the full, virtualised list.
                    path_cell.ctx.accesskit_node_builder(path_cell.id, |node| {
                        node.set_role(accesskit::Role::ListBoxOption);
                        node.clear_toggled();
                        node.set_selected(selected);
                        node.set_position_in_set(view + 1);
                        node.set_size_of_set(len);
                    });
                    if row.response().clicked() {
                        clicked = Some(view);
                    }
                });
                // Make the rows' container a multi-select list box so that
                // VoiceOver treats them as selectable items: AccessKit's
                // macOS adapter only reports selection for item-like roles
                // inside such a container. TableBuilder doesn't expose that
                // container, so find it through the cell's Ui stack.
                if let Some(id) = list_parent {
                    ctx.accesskit_node_builder(id, |node| {
                        node.set_role(accesskit::Role::ListBox);
                        node.set_label("Reconciliation items");
                        node.set_multiselectable();
                    });
                }
            });
        if let Some(view) = clicked {
            self.table.click(view, mods);
        }
        if let Some(key) = sort_clicked {
            self.table.sort_by(key);
            self.scroll_to = self.table.cursor();
        }
    }

    fn ssh_modal(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &mut self.ssh else { return };
        let mut outcome = None;
        let response = Modal::new(egui::Id::new("ssh-password")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading("SSH password");
            ui.label("jcomellas@example.org's password:");
            let edit = ui.add(
                egui::TextEdit::singleline(&mut prompt.password)
                    .password(true)
                    .desired_width(f32::INFINITY),
            );
            if !prompt.focus_requested {
                edit.request_focus();
                prompt.focus_requested = true;
            }
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            ui.horizontal(|ui| {
                if ui.button("Connect").clicked() || enter {
                    outcome = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    outcome = Some(false);
                }
            });
        });
        if response.should_close() && outcome.is_none() {
            outcome = Some(false);
        }
        match outcome {
            Some(true) => {
                self.ssh_result = format!("ssh: sent a {}-char password", prompt.password.len());
                self.ssh = None;
            }
            Some(false) => {
                self.ssh_result = "ssh: cancelled".into();
                self.ssh = None;
            }
            None => {}
        }
    }
}

impl eframe::App for SpikeApp {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.first_frame_done {
            self.first_frame_done = true;
            eprintln!(
                "[egui] first frame after {:.1} ms",
                self.process_start.elapsed().as_secs_f64() * 1000.0
            );
            if self.args.exit_after_first_frame {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            if self.args.bench.is_some() {
                let t = Instant::now();
                self.table.sort_with(SortKey::Status, true);
                let status = t.elapsed();
                let t = Instant::now();
                self.table.sort_with(SortKey::Path, true);
                eprintln!(
                    "[egui] sort {} rows: by status {:.1} ms, by path {:.1} ms",
                    self.table.len(),
                    status.as_secs_f64() * 1000.0,
                    t.elapsed().as_secs_f64() * 1000.0
                );
                self.bench_start = Some(Instant::now());
            }
        }
        if let (Some(start), Some(limit)) = (self.bench_start, self.args.bench) {
            self.frames.frame();
            if start.elapsed() >= limit {
                eprintln!("{}", self.frames.report("egui", start.elapsed()));
                self.bench_start = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint();
        }

        self.update_ui(ui, Some(frame));
    }
}

impl SpikeApp {
    /// One frame: apply engine updates and commands, then lay out.
    fn update_ui(&mut self, ui: &mut Ui, frame: Option<&eframe::Frame>) {
        let ctx = ui.ctx().clone();
        let updates = self.feed.drain();
        self.frames.updates_applied += updates.len() as u64;
        self.table.apply_progress(&updates);

        while let Ok(cmd) = self.commands.try_recv() {
            // The menu bar has no notion of egui focus: ignore table
            // commands while the modal is up.
            if self.ssh.is_some() && !matches!(cmd, Command::RootChosen(_)) {
                continue;
            }
            self.run_command(cmd, frame, &ctx);
        }
        if self.ssh.is_none() {
            self.handle_keys(ui);
        }

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui, frame));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default().show(ui, |ui| self.recon_table(ui));
        self.ssh_modal(&ctx);
    }
}

fn status_color(status: ReplicaStatus) -> Color32 {
    match status {
        ReplicaStatus::Unchanged => Color32::TRANSPARENT,
        ReplicaStatus::PropsChanged => Color32::from_rgb(140, 140, 200),
        ReplicaStatus::Modified => Color32::from_rgb(230, 170, 30),
        ReplicaStatus::Created => Color32::from_rgb(60, 170, 80),
        ReplicaStatus::Deleted => Color32::from_rgb(210, 60, 60),
    }
}

fn direction_color(direction: Direction) -> Color32 {
    match direction {
        Direction::Conflict => Color32::from_rgb(210, 60, 60),
        Direction::Skip => Color32::GRAY,
        Direction::LeftToRight | Direction::RightToLeft => Color32::from_rgb(60, 130, 210),
    }
}

/// A status icon (a painted badge) followed by its label.
fn status_cell(ui: &mut Ui, status: ReplicaStatus) {
    if status == ReplicaStatus::Unchanged {
        return;
    }
    // Table cells lay out top-down; keep the icon and its label on one line.
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 4.5, status_color(status));
        ui.add(egui::Label::new(status.label()).selectable(false));
    });
}

fn progress_cell(ui: &mut Ui, row: &Row) {
    if row.progress > 0.0 {
        ui.add(
            egui::ProgressBar::new(row.progress)
                .desired_height(10.0)
                .desired_width(100.0),
        );
    }
}

/// Builds the native menu bar. `muda` is a separate crate because eframe
/// has no menu-bar support; on macOS it must be installed after NSApp
/// exists, which is why this runs in the app-creation callback.
#[cfg(target_os = "macos")]
fn build_menu(ctx: &egui::Context, tx: Sender<Command>) -> Option<NativeMenu> {
    use muda::{Menu, MenuItem, PredefinedMenuItem, Submenu};
    let item = |id: &str, text: &str, accel: Option<&str>| {
        MenuItem::with_id(id, text, true, accel.and_then(|a| a.parse().ok()))
    };
    let app_menu = Submenu::with_items(
        "twinner-gui-spike-egui",
        true,
        &[
            &PredefinedMenuItem::about(None, None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::quit(None),
        ],
    )
    .ok()?;
    let file = Submenu::with_items(
        "File",
        true,
        &[
            &item("choose-root", "Choose Root…", Some("CmdOrCtrl+O")),
            &item("ssh", "Connect over SSH…", Some("CmdOrCtrl+K")),
        ],
    )
    .ok()?;
    let edit = Submenu::with_items(
        "Edit",
        true,
        &[&item("select-all", "Select All", Some("CmdOrCtrl+A"))],
    )
    .ok()?;
    // No accelerators for > < /: a menu key equivalent without a
    // modifier would also swallow those characters in text fields.
    let actions = Submenu::with_items(
        "Actions",
        true,
        &[
            &item("ltr", "Propagate Left to Right  (>)", None),
            &item("rtl", "Propagate Right to Left  (<)", None),
            &item("skip", "Skip  (/)", None),
        ],
    )
    .ok()?;
    let view = Submenu::with_items(
        "View",
        true,
        &[
            &item("sort-path", "Sort by Path", Some("CmdOrCtrl+1")),
            &item("sort-status", "Sort by Status", Some("CmdOrCtrl+2")),
            &item("sort-size", "Sort by Size", Some("CmdOrCtrl+3")),
        ],
    )
    .ok()?;
    let menu = Menu::with_items(&[&app_menu, &file, &edit, &actions, &view]).ok()?;
    menu.init_for_nsapp();
    let ctx = ctx.clone();
    muda::MenuEvent::set_event_handler(Some(move |event: muda::MenuEvent| {
        let cmd = match event.id().as_ref() {
            "choose-root" => Command::ChooseRoot,
            "ssh" => Command::ConnectSsh,
            "select-all" => Command::SelectAll,
            "ltr" => Command::Direction(Direction::LeftToRight),
            "rtl" => Command::Direction(Direction::RightToLeft),
            "skip" => Command::Direction(Direction::Skip),
            "sort-path" => Command::Sort(SortKey::Path),
            "sort-status" => Command::Sort(SortKey::Status),
            "sort-size" => Command::Sort(SortKey::Size),
            _ => return,
        };
        let _ = tx.send(cmd);
        ctx.request_repaint();
    }));
    Some(menu)
}

/// Windows needs `init_for_hwnd` plus accelerator translation in the
/// message loop, and Linux needs a GTK window: out of scope for the spike.
#[cfg(not(target_os = "macos"))]
fn build_menu(_: &egui::Context, _: Sender<Command>) -> Option<NativeMenu> {
    None
}

/// `--bench SECS --headless`: no window or GPU. Times egui's main-thread
/// work per frame (engine updates, layout and tessellation into triangles),
/// which is what would delay a frame; the GPU upload/draw is excluded.
fn run_headless(args: SpikeArgs, limit: std::time::Duration) {
    let ctx = egui::Context::default();
    let mut app = SpikeApp::headless(&ctx, args, Instant::now());
    // Turns on auto-scrolling in `recon_table`.
    app.bench_start = Some(Instant::now());
    let run = |app: &mut SpikeApp| {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 700.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| app.update_ui(ui, None));
        std::hint::black_box(ctx.tessellate(out.shapes, out.pixels_per_point));
        out.textures_delta.clear();
    };
    run(&mut app);
    run(&mut app);
    let report = headless_bench("egui", limit, |op| {
        match op {
            Some(Op::SelectAll) => {
                app.table.select_all();
            }
            Some(Op::SortByStatus) => app.table.sort_with(SortKey::Status, true),
            Some(Op::DirectionAll) => {
                app.table.set_direction(Direction::LeftToRight);
            }
            None => {}
        }
        let before = app.frames.updates_applied;
        run(&mut app);
        (app.frames.updates_applied - before) as usize
    });
    eprintln!("{report}");
}

fn main() -> eframe::Result {
    let process_start = Instant::now();
    let args = SpikeArgs::from_env();
    if let (true, Some(limit)) = (args.headless, args.bench) {
        run_headless(args, limit);
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("twinner GUI spike — egui")
            .with_inner_size([1000.0, 700.0])
            // Keep the --bench window in front (a locked screen still occludes it).
            .with_window_level(if args.bench.is_some() {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            }),
        ..Default::default()
    };
    eframe::run_native(
        "twinner-gui-spike-egui",
        options,
        Box::new(move |cc| Ok(Box::new(SpikeApp::new(cc, args, process_start)))),
    )
}

#[cfg(test)]
mod tests {
    //! Drives the real table code with synthetic input on a bare
    //! `egui::Context`, and checks what AccessKit (and so VoiceOver,
    //! Narrator and AT-SPI) would be told about the rows.

    use super::*;
    use eframe::egui::{Modifiers, PointerButton, Pos2, RawInput, Rect, accesskit::Role};

    fn frame(
        ctx: &egui::Context,
        app: &mut SpikeApp,
        events: Vec<Event>,
        mods: Modifiers,
    ) -> Vec<(String, bool, Rect)> {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 700.0))),
            events: [vec![Event::ModifiersChanged(mods)], events].concat(),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| app.update_ui(ui, None));
        out.textures_delta.clear();
        let update = out
            .platform_output
            .accesskit_update
            .expect("accesskit enabled");
        // The rows must be children of the multi-select list box, or
        // VoiceOver won't report their selection.
        let list_boxes: Vec<_> = update
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == Role::ListBox)
            .collect();
        let items: Vec<_> = update
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == Role::ListBoxOption)
            .collect();
        if !items.is_empty() {
            assert_eq!(list_boxes.len(), 1);
            let list_box = &list_boxes[0].1;
            assert!(list_box.is_multiselectable());
            // Only the row items are visible to screen readers; the other
            // cells are hidden.
            for child in list_box.children() {
                let (_, node) = update.nodes.iter().find(|(i, _)| i == child).unwrap();
                assert!(
                    node.role() == Role::ListBoxOption || node.is_hidden(),
                    "visible {:?} in the list box",
                    node.role()
                );
            }
            for (id, item) in &items {
                let parent_of = |c: &accesskit::NodeId| {
                    update
                        .nodes
                        .iter()
                        .find(|(_, n)| n.children().contains(c))
                        .map(|(i, n)| (*i, n.role()))
                };
                let mut chain = vec![];
                let mut cur = *id;
                while let Some((p, r)) = parent_of(&cur) {
                    chain.push(r);
                    cur = p;
                }
                assert!(
                    list_box.children().contains(id),
                    "row is not a list-box child: {chain:?}"
                );
                assert_eq!(item.size_of_set(), Some(50_000));
            }
        }
        let mut rows: Vec<_> = items
            .into_iter()
            .map(|(_, n)| {
                let b = n.bounds().expect("row bounds");
                (
                    n.label().unwrap_or_default().to_string(),
                    n.is_selected().unwrap_or(false),
                    Rect::from_min_max(
                        Pos2::new(b.x0 as f32, b.y0 as f32),
                        Pos2::new(b.x1 as f32, b.y1 as f32),
                    ),
                )
            })
            .collect();
        // Tree order is not screen order.
        rows.sort_by(|a, b| a.2.min.y.total_cmp(&b.2.min.y));
        rows
    }

    fn click(at: Pos2, mods: Modifiers) -> Vec<Event> {
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: mods,
        };
        vec![Event::PointerMoved(at), button(true), button(false)]
    }

    #[test]
    fn table_is_virtualised_selectable_and_accessible() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let args = SpikeArgs {
            rows: 50_000,
            rate: 0,
            ..SpikeArgs::default()
        };
        let mut app = SpikeApp::headless(&ctx, args, Instant::now());
        let none = Modifiers::default();
        // The first frame is a sizing pass with no real geometry.
        frame(&ctx, &mut app, vec![], none);
        let rows = frame(&ctx, &mut app, vec![], none);
        assert!((10..100).contains(&rows.len()), "{} row nodes", rows.len());
        assert!(
            rows[0].0.starts_with(&app.table.row_at(0).path),
            "{}",
            rows[0].0
        );
        assert!(rows[0].0.contains("% done"), "{}", rows[0].0);

        // Click row 0, shift-click row 3, cmd-click row 1: rows 0, 2, 3.
        let shift = Modifiers {
            shift: true,
            ..none
        };
        let cmd = Modifiers {
            command: true,
            mac_cmd: cfg!(target_os = "macos"),
            ctrl: !cfg!(target_os = "macos"),
            ..none
        };
        frame(&ctx, &mut app, click(rows[0].2.center(), none), none);
        frame(&ctx, &mut app, click(rows[3].2.center(), shift), shift);
        frame(&ctx, &mut app, click(rows[1].2.center(), cmd), cmd);
        let selected: Vec<usize> = (0..app.table.len())
            .filter(|&v| app.table.is_selected(v))
            .collect();
        assert_eq!(selected, [0, 2, 3]);

        // `>` applies to the whole selection; the tree reports selection.
        let rows = frame(&ctx, &mut app, vec![Event::Text(">".into())], none);
        for v in [0, 2, 3] {
            assert_eq!(app.table.row_at(v).direction, Direction::LeftToRight);
        }
        let flags: Vec<bool> = rows.iter().take(4).map(|r| r.1).collect();
        assert_eq!(flags, [true, false, true, true]);
        assert!(rows[0].0.contains("left to right"), "{}", rows[0].0);

        // The direction keys are ignored while the SSH modal is open.
        app.ssh = Some(SshPrompt {
            password: String::new(),
            focus_requested: false,
        });
        frame(&ctx, &mut app, vec![Event::Text("<".into())], none);
        assert_eq!(app.table.row_at(0).direction, Direction::LeftToRight);
    }
}
