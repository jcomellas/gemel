//! The GUI harness: driving the window headless on Slint's testing
//! backend, and running generated scenarios (the oracle's) to compare what
//! one side leaves with what `gemel-gui -ui text` leaves.
//!
//! Milestone 4's phase 0. The window has no session yet, so the
//! scenarios' other side is the text interface too; phase 1 runs the
//! window there.

#![allow(dead_code)]

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gemel_gui::ui::AppWindow;
use gemel_oracle::compare::{check_recheck, compare};
use gemel_oracle::run::{self, How, Pairing};
use gemel_oracle::scenario::{Flavour, Scenario};
use i_slint_backend_testing::{AccessibleRole, ElementQuery};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};

// ---- The window ----

thread_local! {
    static STARTED: Cell<bool> = const { Cell::new(false) };
}

/// Starts Slint's testing backend on this thread, once: each test thread
/// has a platform of its own, and runs without an event loop, with a mock
/// clock that only [`pump`] moves.
pub fn init() {
    if !STARTED.replace(true) {
        i_slint_backend_testing::init_no_event_loop();
    }
}

/// Moves the mock clock on by `d`, and runs the timers due by then.
pub fn pump(d: Duration) {
    i_slint_backend_testing::mock_elapsed_time(d);
    slint::platform::update_timers_and_animations();
}

/// Pumps a frame (16 ms) at a time until `done`, for at most `limit` of
/// real time, since what the timers wait for (another thread, a process)
/// takes real time: whether it came.
pub fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if done() {
            return true;
        }
        if start.elapsed() > limit {
            return false;
        }
        pump(Duration::from_millis(16));
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A list item as a screen reader sees it.
pub struct Item {
    /// What it says.
    pub label: String,
    /// Whether it is selected.
    pub selected: bool,
    /// Its place in the list.
    pub index: usize,
    /// Where to click it.
    pub centre: LogicalPosition,
}

/// The list items shown, in order.
pub fn items(ui: &AppWindow) -> Vec<Item> {
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
                centre: LogicalPosition::new(pos.x + size.width / 2.0, pos.y + size.height / 2.0),
            }
        })
        .collect();
    // The query yields one handle per item-tree node that makes up the
    // row element, so the same row shows up several times.
    out.sort_by_key(|i| i.index);
    out.dedup_by_key(|i| i.index);
    out
}

/// Clicks at `at`, with `modifier` held.
pub fn click(ui: &AppWindow, at: LogicalPosition, modifier: Option<Key>) {
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

/// Types `text`, as one key.
pub fn type_text(ui: &AppWindow, text: &str) {
    let w = ui.window();
    w.dispatch_event(WindowEvent::KeyPressed { text: text.into() });
    w.dispatch_event(WindowEvent::KeyReleased { text: text.into() });
}

// ---- Scenarios ----

/// The program, whose text interface is the side the GUI is compared with.
pub fn gemel_gui() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_gemel-gui"))
}

/// A work directory of its own for a test, removed when it ends if the
/// test passed.
pub struct Work(pub PathBuf);

impl Work {
    /// The directory for `tag`, empty.
    pub fn new(tag: &str) -> Work {
        let dir = std::env::temp_dir().join(format!("gemel-gui-{tag}-{}", std::process::id()));
        if dir.exists() {
            run::remove_all(&dir).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        Work(dir)
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = run::remove_all(&self.0);
        }
    }
}

/// Runs scenario `seed` with local roots through `candidate`, then through
/// `gemel-gui -ui text`, one after the other in `work` (archive names hash
/// the roots' absolute paths), and compares them as the oracle does, each
/// rechecked by the text interface: what differs, a line each.
pub fn disagreements(seed: u64, candidate: &Pairing, work: &Path) -> Vec<String> {
    let scenario = Scenario::generate(seed, Flavour::host());
    let text = Pairing {
        client: gemel_gui(),
        server: gemel_gui(),
    };
    let how = How::default();
    let dir = work.join("work");
    let run = |p: &Pairing| run::outcome(p, &text, None, &how, &scenario, &dir).unwrap();
    let reference = run(&text);
    let theirs = run(candidate);
    let mut problems = compare(&reference, &theirs, (false, false));
    problems.extend(check_recheck(&reference).map(|p| format!("(reference) {p}")));
    problems.extend(check_recheck(&theirs).map(|p| format!("(candidate) {p}")));
    if !problems.is_empty() {
        problems.insert(0, scenario.to_string());
    }
    problems
}
