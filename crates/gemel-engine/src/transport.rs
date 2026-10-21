//! Propagation of reconciled changes.
//!
//! Ports Unison `src/transport.ml`, and the pool that runs it
//! (`Uicommon.transportItems`), which every user interface shares.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Each item becomes one of the [`files`] operations, or several when a
//! move can't be propagated as a rename. The log entries are OCaml's, in
//! the client's [`Trace`] (a slot of the client's host, which the user
//! interface sets up).
//!
//! OCaml runs up to `maxthreads` items at once, but between local roots
//! its operations never yield, so each item runs to the end before the
//! next starts. Here the filesystem work runs on blocking tasks, which do
//! yield, so between local roots [`transport_items`] runs one item at a
//! time: that keeps OCaml's order (of the log and of archive updates), and
//! keeps each operation atomic on its host (see [`crate::exclusive`]).
//! Items run concurrently only with a remote root (milestone 3).

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

use gemel_base::path::{Global, Path};
use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
use gemel_base::trace;
use gemel_base::util::{Error, FileId, MY_NAME_AND_VERSION, capitalize_ascii};
use gemel_fs::fileinfo::Typ;
use gemel_fs::osx;
use gemel_fs::props::Filesize;
use gemel_fs::system;
use gemel_rpc::registry::Rpc;
use gemel_rpc::root::Root;

use crate::abort::Abort;
use crate::common::{
    ContentsChange, Direction, PrevState, ReconItem, ReplicaContent, Replicas, Status,
    UpdateContent, UpdateItem,
};
use crate::copy::Replacing;
use crate::files::{self, MoveSide};
use crate::globals::{self, Roots};
use crate::moves;
use crate::with_trace;

/// A boxed future of the engine thread.
pub type Task<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

fn internal(what: &str) -> Error {
    Error::Fatal(format!("Internal error: transport: {what}"))
}

fn path_str(p: &Path<Global>) -> String {
    String::from_utf8_lossy(p.as_bytes()).into_owned()
}

/// `maxthreads`.
pub static MAXTHREADS: PrefDef<i64> = PrefDef::new(
    "maxthreads",
    Category::Advanced(Topic::General),
    || 0,
    "maximum number of simultaneous file transfers",
    "This preference controls how much concurrency is allowed during \
     the transport phase.  Normally, it should be set reasonably high \
     to maximize performance, but when Unison is used over a \
     low-bandwidth link it may be helpful to set it lower (e.g. \
     to 1) so that Unison doesn't soak up all the available bandwidth. \
     The default is the special value 0, which mean 20 threads \
     when file content streaming is deactivated and 1000 threads \
     when it is activated.",
);

/// Registers this module's preferences.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&MAXTHREADS)
}

/// `Transport.maxThreads`: how many items may run at once (fewer without
/// streaming, `stream`).
pub fn max_threads(prefs: &Prefs) -> usize {
    match usize::try_from(*prefs.read(&MAXTHREADS)) {
        Ok(n) if n > 0 => n,
        _ if *prefs.read(&gemel_rpc::connection::STREAMING_ACTIVATED) => 1000,
        _ => 20,
    }
}

/// `Transport.run`: runs the tasks `dispense` hands out, up to `limit` at
/// once, until it hands out no more.
pub async fn run<'a>(limit: usize, mut dispense: impl FnMut() -> Option<Task<'a, ()>>) {
    let limit = limit.max(1);
    let mut running: Vec<Task<'a, ()>> = Vec::new();
    let mut dispensed_all = false;
    std::future::poll_fn(|cx| {
        loop {
            while !dispensed_all && running.len() < limit {
                match dispense() {
                    Some(t) => running.push(t),
                    None => dispensed_all = true,
                }
            }
            let before = running.len();
            running.retain_mut(|t| t.as_mut().poll(cx).is_pending());
            if running.is_empty() && dispensed_all {
                return Poll::Ready(());
            }
            if running.len() == before {
                return Poll::Pending;
            }
        }
    })
    .await;
}

/// `Uicommon.transportItems`: runs `make_action` on each item
/// `this_round` selects, in order. Unless everything is aborted: then it
/// starts nothing more. The first error stops the run: nothing more
/// starts, everything is aborted, and the error is returned once the items
/// already running have finished. (OCaml returns it at once, and gives the
/// others a few chances to run; they stop at their abort checks.)
pub async fn transport_items<'a, T>(
    rpc: &Rpc,
    roots: &Roots,
    items: &'a [T],
    this_round: impl Fn(&T) -> bool,
    make_action: impl Fn(usize, &'a T) -> Task<'a, Result<(), Error>>,
) -> Result<(), Error> {
    let abort = rpc.client_host().state::<Abort>();
    if abort.borrow().is_all() {
        return Ok(());
    }
    let limit = if roots.list().iter().all(Root::is_local) {
        1
    } else {
        max_threads(&rpc.client_host().prefs().borrow())
    };
    let failure = RefCell::new(None);
    let mut next = 0;
    run(limit, || {
        loop {
            if failure.borrow().is_some() || abort.borrow().is_all() || next >= items.len() {
                return None;
            }
            let i = next;
            next += 1;
            if !this_round(&items[i]) {
                continue;
            }
            let action = make_action(i, &items[i]);
            let (failure, abort) = (&failure, abort.clone());
            return Some(Box::pin(async move {
                if let Err(e) = action.await {
                    abort.borrow_mut().all();
                    failure.borrow_mut().get_or_insert(e);
                }
            }));
        }
    })
    .await;
    failure.into_inner().map_or(Ok(()), Err)
}

// ---- Logging ----

/// Runs `f` with the client's trace and preferences.
fn log(rpc: &Rpc, s: &str) -> Result<(), Error> {
    with_trace(rpc, |t, p| t.log(p, s))
}

/// `logLwtNumbered`: `[BGN]` and the description (on one line) before
/// `t`, and `[END]` and the short description after it, if it succeeds.
async fn log_numbered<T>(
    rpc: &Rpc,
    desc: &str,
    short: &str,
    t: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    log(rpc, &format!("[BGN] {}\n", desc.replace("\n ", "")))?;
    let v = t.await?;
    log(rpc, &format!("[END] {short}\n"))?;
    Ok(v)
}

// ---- Propagating an item ----

/// `fileSize`: the lengths of the file being replaced (data and resource
/// fork).
fn file_size(ui_from: &UpdateItem, ui_to: &UpdateItem) -> Result<(Filesize, Filesize), Error> {
    use UpdateItem::{NoUpdates, Updates};
    match (ui_from, ui_to) {
        (_, Updates(UpdateContent::File(props, ContentsChange::Updated(_, _, ress)), _)) => {
            Ok((props.length(), osx::ress_length(ress)))
        }
        (
            Updates(_, PrevState::File(props, _, _, ress)),
            NoUpdates | Updates(UpdateContent::File(_, ContentsChange::Same), _),
        ) => Ok((props.length(), osx::ress_length(ress))),
        _ => Err(internal("fileSize")),
    }
}

/// What stays the same while an item propagates.
#[derive(Clone, Copy)]
struct Item<'a> {
    rpc: &'a Rpc,
    roots: &'a Roots,
    from: &'a Root,
    to: &'a Root,
    not_default: bool,
    id: FileId,
}

/// One side of a propagation: a path and its contents.
type Side<'a> = (&'a Path<Global>, &'a ReplicaContent);

fn unchanged(rc: &ReplicaContent) -> bool {
    matches!(rc.status, Status::Unchanged | Status::PropsChanged)
}

/// `moveFallback`: propagates a move as separate copies and deletions.
fn move_fallback<'a>(it: Item<'a>, from: Side<'a>, to: Side<'a>) -> Task<'a, Result<(), Error>> {
    Box::pin(async move {
        let ((from_path, from_rc), (to_path, to_rc)) = (from, to);
        match (&from_rc.status, &to_rc.status) {
            (Status::MovedOut(..), Status::MovedOut(..))
            | (Status::MovedIn(..), Status::MovedIn(..)) => Err(internal("moveFallback")),
            (Status::MovedOut(from_path2, from2, to2), _) => {
                do_action(it, (from_path2, from2), (from_path2, to2)).await?;
                let original = moves::original_rc(from_rc);
                do_action(it, (from_path, &original), to).await
            }
            (_, Status::MovedOut(to_path2, to2, from2)) => {
                let original = moves::original_rc(to_rc);
                do_action(it, from, (to_path, &original)).await?;
                do_action(it, (to_path2, from2), (to_path2, to2)).await
            }
            (Status::MovedIn(from_path2, from2, to2), _) => {
                let original = moves::original_rc(from_rc);
                do_action(it, (from_path, &original), to).await?;
                do_action(it, (from_path2, from2), (from_path2, to2)).await
            }
            (_, Status::MovedIn(to_path2, to2, from2)) => {
                do_action(it, (to_path2, from2), (to_path2, to2)).await?;
                let original = moves::original_rc(to_rc);
                do_action(it, from, (to_path, &original)).await
            }
            _ => do_action(it, from, to).await,
        }
    })
}

/// `tryMove`, inside its log entries: the move, or the copies and
/// deletions if it would cross filesystems.
#[allow(clippy::too_many_arguments)]
async fn try_move(
    it: Item<'_>,
    (src, dst): (&Path<Global>, &Path<Global>),
    shown: &Path<Global>,
    from_side: MoveSide<'_>,
    new_desc: &gemel_fs::props::Props,
    to_side: MoveSide<'_>,
    old_desc: &gemel_fs::props::Props,
    size: Filesize,
    fallback: (Side<'_>, Side<'_>),
) -> Result<(), Error> {
    log_numbered(
        it.rpc,
        &format!(
            "Moving/renaming file {} --> {}\n  on {}",
            path_str(src),
            path_str(dst),
            it.to
        ),
        &format!("Moving/renaming file {}", path_str(shown)),
        async {
            match files::move_(
                it.rpc,
                it.from,
                from_side,
                new_desc,
                it.to,
                to_side,
                old_desc,
                it.not_default,
                size,
                it.id,
            )
            .await
            {
                Err(Error::Transient(m)) if m == "EXDEV" => {
                    with_trace(it.rpc, |t, p| {
                        t.debug(p, "transport", || {
                            "Attempt to rename across filesystems failed. \
                             Falling back to copy + delete.\n"
                                .to_owned()
                        })
                    });
                    move_fallback(it, fallback.0, fallback.1).await
                }
                r => r,
            }
        },
    )
    .await
}

/// `doMove`: propagates a move as a rename where the moved contents are
/// unchanged in the other replica, and as copies and deletions otherwise.
fn do_move<'a>(it: Item<'a>, from: Side<'a>, to: Side<'a>) -> Task<'a, Result<(), Error>> {
    Box::pin(async move {
        let ((from_path, from_rc), (to_path, to_rc)) = (from, to);
        let absent_new = UpdateItem::Updates(UpdateContent::Absent, PrevState::New);
        match (&from_rc.status, &to_rc.status) {
            (Status::MovedOut(..), Status::MovedOut(..))
            | (Status::MovedIn(..), Status::MovedIn(..)) => Err(internal("doMove")),
            (_, Status::MovedOut(..)) if from_rc.typ == Typ::Absent => Err(internal("doMove")),
            // Case 1: the move's source is unchanged in the other replica,
            // so the move is repeated there (over whatever is at its
            // target).
            (Status::MovedOut(from_path2, from2, to2), _) if unchanged(to_rc) => {
                try_move(
                    it,
                    (to_path, from_path2),
                    to_path,
                    MoveSide {
                        path: from_path,
                        ui: &from_rc.ui,
                        new_path: from_path2,
                        new_ui: &from2.ui,
                        new_props: &from2.props,
                    },
                    &from2.desc,
                    MoveSide {
                        path: to_path,
                        ui: &to_rc.ui,
                        new_path: from_path2,
                        new_ui: &to2.ui,
                        new_props: &to2.props,
                    },
                    &to_rc.desc,
                    from_rc.size.1,
                    (from, to),
                )
                .await
            }
            // Case 2: the move's target doesn't exist in the replica
            // propagated from, where its source is unchanged, so the move
            // is reverted.
            (_, Status::MovedOut(to_path2, to2, from2))
                if unchanged(from_rc) && from2.typ == Typ::Absent =>
            {
                try_move(
                    it,
                    (to_path2, to_path),
                    to_path2,
                    MoveSide {
                        path: to_path2,
                        ui: &absent_new,
                        new_path: from_path,
                        new_ui: &from_rc.ui,
                        new_props: &from_rc.props,
                    },
                    &from_rc.desc,
                    MoveSide {
                        path: to_path2,
                        ui: &to2.ui,
                        new_path: to_path,
                        new_ui: &to_rc.ui,
                        new_props: &to_rc.props,
                    },
                    &to2.desc,
                    to_rc.size.1,
                    (from, to),
                )
                .await
            }
            // Case 3: as case 1, from the move's target.
            (Status::MovedIn(from_path2, from2, to2), _) if unchanged(to2) => {
                try_move(
                    it,
                    (from_path2, to_path),
                    from_path2,
                    MoveSide {
                        path: from_path2,
                        ui: &from2.ui,
                        new_path: from_path,
                        new_ui: &from_rc.ui,
                        new_props: &from_rc.props,
                    },
                    &from_rc.desc,
                    MoveSide {
                        path: from_path2,
                        ui: &to2.ui,
                        new_path: to_path,
                        new_ui: &to_rc.ui,
                        new_props: &to_rc.props,
                    },
                    &to2.desc,
                    from_rc.size.1,
                    (from, to),
                )
                .await
            }
            // Case 4: as case 2, from the move's source.
            (_, Status::MovedIn(to_path2, to2, from2))
                if from_rc.typ == Typ::Absent && unchanged(from2) =>
            {
                try_move(
                    it,
                    (to_path, to_path2),
                    to_path,
                    MoveSide {
                        path: from_path,
                        ui: &absent_new,
                        new_path: to_path2,
                        new_ui: &from2.ui,
                        new_props: &from2.props,
                    },
                    &from2.desc,
                    MoveSide {
                        path: to_path,
                        ui: &to_rc.ui,
                        new_path: to_path2,
                        new_ui: &to2.ui,
                        new_props: &to2.props,
                    },
                    &to_rc.desc,
                    to_rc.size.1,
                    (from, to),
                )
                .await
            }
            (Status::MovedOut(..) | Status::MovedIn(..), _)
            | (_, Status::MovedOut(..) | Status::MovedIn(..)) => move_fallback(it, from, to).await,
            _ => Err(internal("doMove")),
        }
    })
}

/// `doAction`: propagates `from` over `to`. A failure is logged (but not
/// shown), and returned.
fn do_action<'a>(it: Item<'a>, from: Side<'a>, to: Side<'a>) -> Task<'a, Result<(), Error>> {
    Box::pin(async move {
        let to_path = to.0;
        with_trace(it.rpc, |t, p| {
            if t.send_log_msgs_to_stderr {
                Ok(())
            } else {
                t.status_detail(p, &path_str(to_path))
            }
        })?;
        let r = do_action_inner(it, from, to).await;
        if let Err(e) = &r {
            with_trace(it.rpc, |t, p| {
                t.log_only(p, &format!("Failed [{}]: {e}\n", path_str(to_path)))
            })?;
        }
        r
    })
}

async fn do_action_inner(it: Item<'_>, from: Side<'_>, to: Side<'_>) -> Result<(), Error> {
    let ((from_path, from_rc), (to_path, to_rc)) = (from, to);
    let moved =
        |rc: &ReplicaContent| matches!(rc.status, Status::MovedOut(..) | Status::MovedIn(..));
    let to_str = path_str(to_path);
    if moved(from_rc) || moved(to_rc) {
        do_move(it, from, to).await
    } else if from_rc.typ == Typ::Absent {
        log_numbered(
            it.rpc,
            &format!("Deleting {to_str}\n  from {}", it.to),
            &format!("Deleting {to_str}"),
            files::delete(
                it.rpc,
                it.from,
                from_path,
                it.to,
                to_path,
                &to_rc.ui,
                it.not_default,
            ),
        )
        .await
    } else if unchanged(from_rc) && unchanged(to_rc) {
        // Only props changed on one side: transferring the whole file or
        // directory would be wasteful (and wrong for a directory).
        log_numbered(
            it.rpc,
            &format!(
                "Copying properties for {to_str}\n  from {}\n  to {}",
                it.from, it.to
            ),
            &format!("Copying properties for {to_str}"),
            files::set_prop(
                it.rpc,
                it.from,
                from_path,
                it.to,
                to_path,
                &from_rc.desc,
                &to_rc.desc,
                &from_rc.ui,
                &to_rc.ui,
            ),
        )
        .await
    } else if from_rc.typ == Typ::File && to_rc.typ == Typ::File {
        let (len, ress_len) = file_size(&from_rc.ui, &to_rc.ui)?;
        log_numbered(
            it.rpc,
            &format!("Updating file {to_str}\n  from {}\n  to {}", it.from, it.to),
            &format!("Updating file {to_str}"),
            files::copy(
                it.rpc,
                it.roots,
                Replacing::Update(len, ress_len),
                it.from,
                from_path,
                &from_rc.ui,
                &[],
                it.to,
                to_path,
                &to_rc.ui,
                &[],
                it.not_default,
                it.id,
            ),
        )
        .await
    } else {
        log_numbered(
            it.rpc,
            &format!("Copying {to_str}\n  from {}\n  to {}", it.from, it.to),
            &format!("Copying {to_str}"),
            files::copy(
                it.rpc,
                it.roots,
                Replacing::Copy,
                it.from,
                from_path,
                &from_rc.ui,
                &from_rc.props,
                it.to,
                to_path,
                &to_rc.ui,
                &to_rc.props,
                it.not_default,
                it.id,
            ),
        )
        .await
    }
}

/// `Transport.transportItem`: propagates an item in its direction, or
/// logs why it is skipped. `show_merge` shows a merge's result and asks
/// whether to keep it.
pub async fn transport_item(
    rpc: &Rpc,
    roots: &Roots,
    ri: &ReconItem,
    id: FileId,
    show_merge: &mut files::ShowMerge<'_>,
) -> Result<(), Error> {
    let (root1, root2) = roots.pair();
    let path = path_str(&ri.path1);
    let d = match &ri.replicas {
        Replicas::Problem(p) => return log(rpc, &format!("[ERROR] Skipping {path}\n  {p}\n")),
        Replicas::Different(d) => d,
    };
    let item = |from, to| Item {
        rpc,
        roots,
        from,
        to,
        not_default: d.direction != d.default_direction,
        id,
    };
    match &d.direction {
        Direction::Conflict(c) => log(rpc, &format!("[CONFLICT] Skipping {path}\n  {c}\n")),
        Direction::Replica1ToReplica2 => {
            do_action(item(root1, root2), (&ri.path1, &d.rc1), (&ri.path2, &d.rc2)).await
        }
        Direction::Replica2ToReplica1 => {
            do_action(item(root2, root1), (&ri.path2, &d.rc2), (&ri.path1, &d.rc1)).await
        }
        Direction::Merge => {
            if d.rc1.typ != Typ::File || d.rc2.typ != Typ::File {
                return Err(Error::Transient(
                    "Can only merge two existing files".to_owned(),
                ));
            }
            files::merge(
                rpc, root1, &ri.path1, &d.rc1.ui, root2, &ri.path2, &d.rc2.ui, id, show_merge,
            )
            .await
        }
    }
}

// ---- The start and end of a run ----

/// When the last run started.
#[derive(Default)]
struct LastLogStart(f64);

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `HH:MM:SS.cc on DD Mon YYYY`.
fn timestamp(t: f64) -> Result<String, Error> {
    let tm = system::localtime(t).ok_or_else(|| internal("no local time"))?;
    let centis = ((t % 1.0) * 100.0) as u32;
    Ok(format!(
        "{:02}:{:02}:{:02}.{:02} on {:02} {} {:04}",
        tm.hour,
        tm.min,
        tm.sec,
        centis.min(99),
        tm.day,
        MONTHS[tm.month as usize - 1],
        tm.year
    ))
}

/// Blank lines around the run's entries, unless terse or batch.
fn spacing(prefs: &Prefs) -> &'static str {
    if *prefs.read(&trace::TERSE) || *prefs.read(&globals::BATCH) {
        ""
    } else {
        "\n\n"
    }
}

/// `Transport.logStart`: resets aborting, and logs the start of a run.
pub fn log_start(rpc: &Rpc) -> Result<(), Error> {
    let host = rpc.client_host();
    host.state::<Abort>().borrow_mut().reset();
    let t = system::gettimeofday();
    host.state::<LastLogStart>().borrow_mut().0 = t;
    let ts = timestamp(t)?;
    with_trace(rpc, |tr, p| {
        let m = format!(
            "{}{} started propagating changes at {ts}\n",
            spacing(p),
            capitalize_ascii(MY_NAME_AND_VERSION)
        );
        tr.log_verbose(p, &m)
    })
}

/// `Transport.logFinish`: logs the end of a run, and how long it took.
pub fn log_finish(rpc: &Rpc) -> Result<(), Error> {
    let t = system::gettimeofday();
    let start = rpc.client_host().state::<LastLogStart>().borrow().0;
    let ts = timestamp(t)?;
    with_trace(rpc, |tr, p| {
        let m = format!(
            "{} finished propagating changes at {ts}, {:.3} s\n{}",
            capitalize_ascii(MY_NAME_AND_VERSION),
            t - start,
            spacing(p)
        );
        tr.log_verbose(p, &m)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: Future>(f: F) -> F::Output {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&rt, f)
    }

    /// Tasks start in order, at most `limit` at a time, and each finished
    /// one makes room for the next.
    #[test]
    fn run_keeps_to_the_limit() {
        for limit in [1, 2, 5] {
            let events = RefCell::new(Vec::new());
            let running = RefCell::new(0usize);
            let mut n = 0;
            block_on(run(limit, || {
                if n == 6 {
                    return None;
                }
                n += 1;
                let i = n;
                let (events, running) = (&events, &running);
                Some(Box::pin(async move {
                    *running.borrow_mut() += 1;
                    assert!(*running.borrow() <= limit);
                    events.borrow_mut().push(format!("start {i}"));
                    for _ in 0..i % 3 {
                        tokio::task::yield_now().await;
                    }
                    events.borrow_mut().push(format!("end {i}"));
                    *running.borrow_mut() -= 1;
                }) as Task<'_, ()>)
            }));
            let events = events.into_inner();
            let starts: Vec<_> = events.iter().filter(|e| e.starts_with("start")).collect();
            assert_eq!(starts.len(), 6, "{events:?}");
            assert!(starts.windows(2).all(|w| w[0] < w[1]), "{events:?}");
            if limit == 1 {
                let paired = (1..=6).flat_map(|i| [format!("start {i}"), format!("end {i}")]);
                assert!(events.iter().cloned().eq(paired), "{events:?}");
            }
        }
        // Nothing to run.
        block_on(run(3, || None));
    }
}
