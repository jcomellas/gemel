//! The GUI spike's checks (§6 of `docs/unison-port-analysis.md`), on the
//! window as the session drives it: headless, with Slint's software
//! renderer at 1000×700 and 60 frames a second, scrolling 7 rows a frame
//! through the items while progress comes at `--rate` updates a second.
//! It reports the main thread's work per frame, and the frames that carry
//! a one-off operation, and the replies to it, one by one: select all,
//! sort, `>` on every item.
//!
//! The events come from `gemel_gui::fake`, each frame those due since the
//! last, as the window's ticks take them from the session. Rendering in
//! software makes the numbers an upper bound: the window renders on the
//! GPU.
//!
//! With `--session`, the window runs on a session over `--rows` new files
//! in a temporary directory, and the report is of the frames that take the
//! first items, the replies to `<` on every item, and a sort's, with the
//! time each took through the engine.
//!
//! ```text
//! cargo run --release -p gemel-gui --example bench -- [--rows N] [--rate N] [--secs S] [--session]
//! ```

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gemel_gui::App;
use gemel_gui::fake;
use gemel_ui_api::session::{Command, Event, SetAction};
use slint::ComponentHandle;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter};

const W: u32 = 1000;
const H: u32 = 700;
const TICK: Duration = Duration::from_micros(16_667);

/// Every window the program makes is a software one; the first is the
/// main window, which is drawn.
struct Headless {
    windows: Rc<RefCell<Vec<Rc<MinimalSoftwareWindow>>>>,
    start: Instant,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        let w = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        self.windows.borrow_mut().push(w.clone());
        Ok(w)
    }

    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }
}

struct Args {
    rows: usize,
    rate: f64,
    secs: f64,
    session: bool,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        rows: 50_000,
        rate: 500.0,
        secs: 10.0,
        session: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        if flag == "--session" {
            a.session = true;
            continue;
        }
        let value = it.next().ok_or(format!("{flag} needs a value"))?;
        let number = |v: &str| v.parse::<f64>().map_err(|e| format!("{flag}: {e}"));
        match flag.as_str() {
            "--rows" => a.rows = number(&value)? as usize,
            "--rate" => a.rate = number(&value)?,
            "--secs" => a.secs = number(&value)?,
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(a)
}

/// What a frame does besides the feed and the scrolling.
#[derive(Clone, Copy, Debug)]
enum Op {
    SelectAll,
    Sort,
    SortReply,
    Right,
    RightReplies,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = args()?;
    let made = Rc::new(RefCell::new(Vec::new()));
    slint::platform::set_platform(Box::new(Headless {
        windows: made.clone(),
        start: Instant::now(),
    }))
    .map_err(|e| format!("{e:?}"))?;
    if args.session {
        return session(&args, &made);
    }
    let app = App::detached()?;
    let ui = app.ui();
    // The main window was made first.
    let window = made.borrow()[0].clone();
    window.set_size(slint::PhysicalSize::new(W, H));
    ui.show()?;
    let mut buffer = vec![PremultipliedRgbaColor::default(); (W * H) as usize];
    let mut draw = || {
        slint::platform::update_timers_and_animations();
        window.draw_if_needed(|renderer| {
            renderer.render(&mut buffer, W as usize);
        });
    };

    let t = Instant::now();
    app.handle(Event::Updates(fake::updates(fake::items(args.rows))));
    draw();
    let first = t.elapsed();
    draw();

    let schedule = [
        (120, Op::SelectAll),
        (240, Op::Sort),
        (241, Op::SortReply),
        (360, Op::Right),
        (361, Op::RightReplies),
    ];
    let mut work = Vec::new();
    let mut ops = Vec::new();
    let mut owed = 0.0;
    let mut fed = 0usize;
    let mut row = 0usize;
    let start = Instant::now();
    let mut next = start;
    let mut n = 0;
    while start.elapsed().as_secs_f64() < args.secs {
        next += TICK;
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let op = schedule.iter().find(|(at, _)| *at == n).map(|(_, op)| *op);
        let t = Instant::now();
        match op {
            Some(Op::SelectAll) => ui.invoke_select_all(),
            Some(Op::Sort) => ui.invoke_sort("name".into()),
            // The session's reply: the items in another order.
            Some(Op::SortReply) => {
                let mut items = fake::items(args.rows);
                items.reverse();
                app.handle(Event::Items { items, next: 0 });
            }
            Some(Op::Right) => ui.invoke_action("right".into()),
            // The session's replies, each item's view as it is now.
            Some(Op::RightReplies) => {
                let sent = app.sent();
                let mut items = fake::items(args.rows);
                items.reverse();
                for c in sent {
                    if let Command::SetAction(i, SetAction::Right) = c {
                        let mut item = items[i].clone();
                        item.changed_from_default = true;
                        app.handle(Event::Item(i, item));
                    }
                }
            }
            None => {}
        }
        // The progress due by now.
        owed += args.rate / 60.0;
        while owed >= 1.0 {
            let item = fed % args.rows;
            let percent = ((fed / args.rows) * 7 % 101) as u8;
            app.handle(Event::Progress(fake::progress(item, percent, args.rows)));
            fed += 1;
            owed -= 1.0;
        }
        row = (row + 7) % args.rows.max(1);
        ui.invoke_bring_into_view(row as i32);
        draw();
        let took = t.elapsed();
        match op {
            Some(op) => ops.push(format!("{op:?} {:.2} ms", ms(took))),
            None => work.push(ms(took)),
        }
        n += 1;
    }
    work.sort_by(f64::total_cmp);
    let pct = |p: f64| work[((work.len() - 1) as f64 * p).round() as usize];
    println!(
        "{} items: first shown in {:.2} ms; {:.1}s, {} frames, work per frame p50 {:.2} ms, \
         p99 {:.2} ms, max {:.2} ms; one-off frames: {}; progress updates {} ({:.0}/s)",
        args.rows,
        ms(first),
        start.elapsed().as_secs_f64(),
        n,
        pct(0.5),
        pct(0.99),
        work.last().copied().unwrap_or(0.0),
        ops.join(", "),
        fed,
        fed as f64 / start.elapsed().as_secs_f64(),
    );
    app.quit(0);
    Ok(())
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

type Windows = Rc<RefCell<Vec<Rc<MinimalSoftwareWindow>>>>;

/// `--session`: the window on a session over new files.
fn session(args: &Args, made: &Windows) -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("gemel-bench-{}", std::process::id()));
    let (a, b, unison) = (dir.join("a"), dir.join("b"), dir.join("unison"));
    for d in [&a, &b, &unison] {
        std::fs::create_dir_all(d)?;
    }
    // At the top: a new directory is one item.
    let t = Instant::now();
    for i in 0..args.rows {
        std::fs::write(a.join(format!("file-{i:05}")), i.to_string())?;
    }
    eprintln!(
        "{} files made in {:.1}s",
        args.rows,
        t.elapsed().as_secs_f64()
    );
    let sink = || Box::new(std::io::sink());
    let app = App::new(gemel_gui::Startup {
        unison_dir: unison,
        args: vec![a.display().to_string(), b.display().to_string()],
        trace: gemel_ui_api::Trace::with_sinks(sink(), sink()),
        host_name: None,
    })?;
    let ui = app.ui();
    let window = made.borrow()[0].clone();
    window.set_size(slint::PhysicalSize::new(W, H));
    ui.show()?;
    let _ticks = app.start_ticks();
    let mut buffer = vec![PremultipliedRgbaColor::default(); (W * H) as usize];
    let mut row = 0usize;
    // Runs frames until `done`: the wall time, the longest frame, and the
    // frames over 16.7 ms.
    let mut frames = |done: &dyn Fn(&App) -> bool| -> (Duration, f64, usize) {
        let start = Instant::now();
        let mut next = start;
        let (mut longest, mut slow) = (0.0f64, 0);
        while !done(&app) {
            next += TICK;
            if let Some(wait) = next.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            // OK at the warnings (no archives yet), as a user would.
            if ui.get_dialog() == 1 {
                ui.invoke_dialog_done(true);
            }
            let t = Instant::now();
            slint::platform::update_timers_and_animations();
            if app.started() {
                row = (row + 7) % args.rows.max(1);
                ui.invoke_bring_into_view(row as i32);
            }
            window.draw_if_needed(|renderer| {
                renderer.render(&mut buffer, W as usize);
            });
            let took = ms(t.elapsed());
            longest = longest.max(took);
            slow += usize::from(took > 16.7);
        }
        (start.elapsed(), longest, slow)
    };
    let report = |what: &str, (wall, longest, slow): (Duration, f64, usize)| {
        println!(
            "{what}: {:.2} s, longest frame {longest:.2} ms, {slow} frames over 16.7 ms",
            wall.as_secs_f64()
        )
    };
    let shown = frames(&|app| app.started() && !app.busy());
    report(
        &format!("{} items found and shown", app.table().len()),
        shown,
    );
    ui.invoke_select_all();
    ui.invoke_action("left".into());
    let all_left = |app: &App| {
        app.table()
            .rows()
            .iter()
            .all(|r| matches!(r.item.action, gemel_ui_api::uicommon::Action::RtoL(_)))
    };
    report("< on every item, and the replies", frames(&all_left));
    ui.invoke_sort("size".into());
    report("a sort, and its reply", frames(&|app| !app.busy()));
    app.quit(0);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
