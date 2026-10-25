//! The GUI harness: driving the window headless on Slint's testing
//! backend, and running generated scenarios (the oracle's) through the
//! window, to compare what it leaves with what `gemel-gui -ui text`
//! leaves.

#![allow(dead_code)]

use std::cell::Cell;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gemel_gui::ui::AppWindow;
use gemel_gui::{App, Startup};
use gemel_oracle::compare::{check_recheck, compare_state};
use gemel_oracle::run::{self, How, Pairing, Step};
use gemel_oracle::scenario::{Flavour, Scenario};
use gemel_ui_api::session::{ItemView, RootNames, Updates};
use gemel_ui_api::uicommon::Action;
use gemel_ui_api::{Global, Trace};
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
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

/// The buttons labelled `label`.
fn buttons(ui: &AppWindow, label: &str) -> Vec<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label)
        .filter(|e| e.accessible_role() == Some(AccessibleRole::Button))
        .collect()
}

/// Whether a button labelled `label` is shown and enabled.
pub fn can_press(ui: &AppWindow, label: &str) -> bool {
    buttons(ui, label)
        .iter()
        .any(|b| b.accessible_enabled() != Some(false))
}

/// Presses the button labelled `label`, as a screen reader would: whether
/// there was one, enabled.
pub fn press(ui: &AppWindow, label: &str) -> bool {
    match buttons(ui, label)
        .into_iter()
        .find(|b| b.accessible_enabled() != Some(false))
    {
        Some(b) => {
            b.invoke_accessible_default_action();
            true
        }
        None => false,
    }
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

// ---- Items without a session ----

/// An item as the session would show a new file at `path` in the first
/// replica, with `action`.
pub fn fake_item(path: &str, action: Action) -> ItemView {
    ItemView {
        path: gemel_ui_api::Path::<Global>::from_bytes(path.as_bytes()).unwrap(),
        path_string: path.to_owned(),
        left: "Created",
        right: "",
        action,
        conflict: matches!(action, Action::Skip(_)),
        changed_from_default: false,
        can_diff: false,
        both_files: true,
        length: 100,
        file_type: "file",
        line: String::new(),
        details: format!("new file  {path}\n"),
        problematic: matches!(action, Action::Skip(_)),
        partially_problematic: false,
        deletion: false,
        transferred: 0,
        progress: String::new(),
        outcome: None,
        status1: "new file",
        status2: "        ",
        shown_path: path.to_owned(),
        problem: None,
        skip_reason: None,
        errors1: Vec::new(),
        errors2: Vec::new(),
        props_only: false,
        favor_changed: None,
    }
}

/// Update detection's result, with `items`.
pub fn fake_updates(items: Vec<ItemView>) -> Updates {
    let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
    Updates {
        items,
        any_equal_updates: false,
        dangerous: Vec::new(),
        dangerous_message: None,
        found_archives: true,
        roots: RootNames {
            short: pair("a", "b"),
            narrow: pair("a", "b"),
            full: pair("a", "b"),
            headings: String::new(),
            hosts: pair("local", "local"),
        },
    }
}

// ---- Syncs through the window ----

/// What the session's trace printed.
#[derive(Clone, Default)]
pub struct Output(Arc<Mutex<Vec<u8>>>);

impl Output {
    /// What was printed.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Output {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The window on `args`, with the Unison directory `unison`, showing,
/// and its trace's output.
pub fn window(unison: &Path, args: Vec<String>) -> (std::rc::Rc<App>, Output) {
    init();
    let out = Output::default();
    let app = App::new(Startup {
        unison_dir: unison.to_owned(),
        args,
        trace: Trace::with_sinks(Box::new(out.clone()), Box::new(out.clone())),
    })
    .unwrap();
    app.ui().show().unwrap();
    (app, out)
}

/// How long a sync through the window may take.
const SYNC: Duration = Duration::from_secs(120);

/// A sync of `work`'s replicas `a` and `b` through the window, with the
/// default actions, as a user goes through it: OK at each warning, then
/// Go if there is anything to propagate, and OK at the summary. The oracle's flags but batch
/// mode and the text interface. Its exit code is the text interface's
/// for the same summary; it prints nothing.
pub fn window_sync(work: &Path) -> io::Result<Step> {
    let mut args: Vec<String> = ["a", "b"]
        .iter()
        .map(|r| work.join(r).display().to_string())
        .collect();
    let mut flags = run::client_flags(&How::default()).into_iter();
    while let Some(f) = flags.next() {
        match f.as_str() {
            "-batch" => {}
            "-ui" => {
                flags.next();
            }
            _ => args.push(f),
        }
    }
    let (app, out) = window(&work.join("unison"), args);
    let ui = app.ui();
    let _ticks = app.start_ticks();
    let mut went = false;
    let done = wait_until(SYNC, || {
        if app.exit_code().is_some() {
            return true;
        }
        match ui.get_dialog() {
            0 => {}
            1 if ui.get_dialog_title() == "Warning" => {
                press(ui, "OK");
                return false;
            }
            // The summary, when the replicas aren't fully synchronized.
            4 => {
                press(ui, "OK");
                return false;
            }
            _ => return true,
        }
        if !app.started() || app.busy() {
            return false;
        }
        if app.summary().is_some() {
            return true;
        }
        if !went && ui.get_go_enabled() {
            went = press(ui, "Go");
            return false;
        }
        !went
    });
    let problem = if !done {
        Some(format!("timed out; status {:?}", ui.get_status_text()))
    } else if ui.get_dialog() != 0 {
        Some(format!(
            "{}: {}",
            ui.get_dialog_title(),
            ui.get_dialog_message()
        ))
    } else {
        app.exit_code()
            .map(|c| format!("the window ended with {c}"))
    };
    let exit = app.summary().map_or(0, |s| s.exit_code() as i32);
    app.quit(0);
    match problem {
        Some(p) => Err(io::Error::other(format!("{p}\n{}", out.text()))),
        None => Ok(Step {
            exit: Some(exit),
            stdout: String::new(),
            stderr: String::new(),
            server: String::new(),
        }),
    }
}

// ---- Scenarios ----

/// The program, whose text interface is the side the window is compared
/// with.
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

/// Runs scenario `seed` with local roots through the window, then through
/// `gemel-gui -ui text -batch`, one after the other in `work` (archive
/// names hash the roots' absolute paths), each rechecked by the text
/// interface, and compares what they left: what differs, a line each.
pub fn disagreements(seed: u64, work: &Path) -> Vec<String> {
    let scenario = Scenario::generate(seed, Flavour::host());
    let text = Pairing {
        client: gemel_gui(),
        server: gemel_gui(),
    };
    let how = How::default();
    let dir = work.join("work");
    let reference = run::outcome(&text, &text, None, &how, &scenario, &dir).unwrap();
    let window = run::outcome_with(&mut window_sync, &text, None, &how, &scenario, &dir);
    let window = match window {
        Ok(o) => o,
        Err(e) => return vec![scenario.to_string(), format!("through the window: {e}")],
    };
    let mut problems = compare_state(&reference, &window);
    problems.extend(check_recheck(&reference).map(|p| format!("(reference) {p}")));
    if !problems.is_empty() {
        problems.insert(0, scenario.to_string());
    }
    problems
}
