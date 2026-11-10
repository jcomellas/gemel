//! The GUI harness: driving the window headless on Slint's testing
//! backend, and running generated scenarios (the oracle's) through the
//! window, to compare what it leaves with what `gemel-gui -ui text`
//! leaves: with local roots, and with replica `b` over ssh (through the
//! oracle's shim) or on a socket.

#![allow(dead_code)]

use std::cell::Cell;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gemel_gui::ui::AppWindow;
use gemel_gui::{App, Startup};
use gemel_oracle::compare::{check_recheck, compare_state};
use gemel_oracle::run::{self, How, Pairing, Remote, Step};
use gemel_oracle::scenario::{Flavour, Scenario};
use gemel_ui_api::Trace;
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, ElementQuery, TestingBackend, TestingBackendOptions,
};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Model};

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

/// [`init`], with Slint's software renderer in each window, so that it
/// can be drawn (`Window::take_snapshot`). Text is laid out in the
/// system's fonts, not the testing backend's fixed ones.
pub fn init_drawn() {
    if !STARTED.replace(true) {
        slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
            mock_time: true,
            threading: false,
            renderer_name: Some("software".into()),
        })))
        .expect("platform already initialized");
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

/// The buttons labelled `label`, in any window.
fn buttons(ui: &impl ComponentHandle, label: &str) -> Vec<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label)
        .filter(|e| e.accessible_role() == Some(AccessibleRole::Button))
        .collect()
}

/// Whether a button labelled `label` is shown and enabled.
pub fn can_press(ui: &impl ComponentHandle, label: &str) -> bool {
    buttons(ui, label)
        .iter()
        .any(|b| b.accessible_enabled() != Some(false))
}

/// Presses the button labelled `label`, as a screen reader would: whether
/// there was one, enabled.
pub fn press(ui: &impl ComponentHandle, label: &str) -> bool {
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

/// What a screen reader is told has the focus, by its label: the
/// element with the keys or, if it isn't accessible, its nearest
/// accessible ancestor, or the descendant that one delegates the focus
/// to, as Slint's AccessKit bridge works it out (`focus_node` in
/// i-slint-backend-winit's `accesskit.rs`), which the testing backend
/// hasn't. `None` while nothing has the keys.
pub fn spoken_focus(ui: &impl ComponentHandle) -> Option<String> {
    use i_slint_core::accessibility::{
        AccessibleStringProperty, accessible_descendents, nearest_accessible_item,
    };
    // A `ListView` makes the rows shown as it is laid out, which a frame
    // does before the bridge looks, and a query here.
    ElementQuery::from_root(ui).match_descendants().find_all();
    let inner = i_slint_core::window::WindowInner::from_pub(ui.window());
    let focus = inner.focus_item.borrow().upgrade()?;
    let parent = nearest_accessible_item(focus);
    let item = parent
        .accessible_string_property(AccessibleStringProperty::DelegateFocus)
        .and_then(|s| s.parse::<usize>().ok())
        .and_then(|i| accessible_descendents(&parent).nth(i))
        .unwrap_or(parent);
    Some(
        item.accessible_string_property(AccessibleStringProperty::Label)
            .unwrap_or_default()
            .to_string(),
    )
}

/// Types `text`, as one key, in any window.
pub fn type_text(ui: &impl ComponentHandle, text: &str) {
    let w = ui.window();
    w.dispatch_event(WindowEvent::KeyPressed { text: text.into() });
    w.dispatch_event(WindowEvent::KeyReleased { text: text.into() });
}

// ---- Items without a session ----

// Not every test uses them.
#[allow(unused_imports)]
pub use gemel_gui::fake::{item as fake_item, updates as fake_updates};

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
    window_named(unison, args, None)
}

/// [`window`], with the client's host named `host_name`.
fn window_named(
    unison: &Path,
    args: Vec<String>,
    host_name: Option<&str>,
) -> (std::rc::Rc<App>, Output) {
    init();
    let out = Output::default();
    let app = App::new(Startup {
        unison_dir: unison.to_owned(),
        args,
        trace: Trace::with_sinks(Box::new(out.clone()), Box::new(out.clone())),
        host_name: host_name.map(str::to_owned),
    })
    .unwrap();
    app.ui().show().unwrap();
    (app, out)
}

// ---- Replicas and profiles ----

/// Replicas `a` and `b` and a Unison directory in `dir`, with `files` in
/// `a` and `b`, synced once through the window.
pub fn synced(dir: &Path, files: &[&str]) {
    for r in ["a", "b", "unison"] {
        std::fs::create_dir_all(dir.join(r)).unwrap();
    }
    for f in files {
        std::fs::write(dir.join("a").join(f), "same").unwrap();
        std::fs::write(dir.join("b").join(f), "same").unwrap();
    }
    window_sync(dir, None).unwrap();
}

/// The window's arguments for replicas `a` and `b` in `dir`.
pub fn roots(dir: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["a", "b"]
        .iter()
        .map(|r| dir.join(r).display().to_string())
        .collect();
    args.extend(["-log=false", "-times"].map(String::from));
    args
}

/// Profiles `names` in `dir/unison`, each with replicas of its own (and
/// a file in the first), and the label `Label of NAME`.
pub fn profiles(dir: &Path, names: &[&str]) {
    std::fs::create_dir_all(dir.join("unison")).unwrap();
    for n in names {
        let (a, b) = (dir.join(format!("{n}-a")), dir.join(format!("{n}-b")));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join(format!("{n}-file")), n).unwrap();
        std::fs::write(
            dir.join("unison").join(format!("{n}.prf")),
            format!(
                "label = Label of {n}\nroot = {}\nroot = {}\nlog = false\n",
                a.display(),
                b.display()
            ),
        )
        .unwrap();
    }
}

/// The profile manager's rows, as a screen reader names them.
pub fn manager_rows(ui: &AppWindow) -> Vec<String> {
    ui.get_profiles()
        .iter()
        .map(|r| r.name.to_string())
        .collect()
}

/// How long a sync through the window may take.
const SYNC: Duration = Duration::from_secs(120);

/// The oracle's program, copied as the ssh shim: built next to the tests'
/// programs when the workspace's tests are (`cargo test --workspace`).
pub fn oracle() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let p = dir.join(format!("gemel-oracle{}", std::env::consts::EXE_SUFFIX));
    assert!(
        p.exists(),
        "{} is missing: build it with `cargo build -p gemel-oracle`",
        p.display()
    );
    p
}

/// How the scenarios run with replica `b` reached as `remote` says: over
/// ssh, the oracle's program is the shim.
fn how(remote: Option<Remote>) -> How {
    match remote {
        Some(Remote::Ssh) => How {
            harness: oracle(),
            ..How::default()
        },
        _ => How::default(),
    }
}

/// The text interface's program, which serves replica `b` and is the
/// reference.
fn text() -> Pairing {
    Pairing {
        client: gemel_gui(),
        server: gemel_gui(),
    }
}

/// A sync of `work`'s replicas `a` and `b` through the window, with the
/// default actions, as a user goes through it: OK at each warning, then
/// Go if there is anything to propagate, and OK at the summary. Replica
/// `b` is reached as `remote` says, `gemel-gui` serving it. The oracle's
/// flags but batch mode and the text interface. Its exit code is the text
/// interface's for the same summary; it prints nothing (but the server's
/// standard error).
pub fn window_sync(work: &Path, remote: Option<Remote>) -> io::Result<Step> {
    let how = how(remote);
    let client = run::client_run(&text(), remote, &how, work)?;
    let mut args = client.args.clone();
    let mut flags = run::client_flags(&how).into_iter();
    while let Some(f) = flags.next() {
        match f.as_str() {
            "-batch" => {}
            "-ui" => {
                flags.next();
            }
            _ => args.push(f),
        }
    }
    let (app, out) = window_named(&work.join("unison"), args, client.host_name);
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
    let (server, _) = client.finish(work)?;
    match problem {
        Some(p) => Err(io::Error::other(format!("{p}\n{}\n{server}", out.text()))),
        None => Ok(Step {
            exit: Some(exit),
            stdout: String::new(),
            stderr: String::new(),
            server,
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

/// Runs scenario `seed` through the window, then through
/// `gemel-gui -ui text -batch`, one after the other in `work` (archive
/// names hash the roots' absolute paths), with replica `b` reached as
/// `remote` says, each rechecked by the text interface, and compares what
/// they left: what differs, a line each.
pub fn disagreements(seed: u64, work: &Path, remote: Option<Remote>) -> Vec<String> {
    let scenario = Scenario::generate(seed, Flavour::host());
    let (text, how) = (text(), how(remote));
    let dir = work.join("work");
    let reference = run::outcome(&text, &text, remote, &how, &scenario, &dir).unwrap();
    let window = run::outcome_with(
        &mut |work| window_sync(work, remote),
        &text,
        remote,
        &how,
        &scenario,
        &dir,
    );
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
