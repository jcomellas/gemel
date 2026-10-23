//! `-repeat`: the `gemel` program syncs again after each interval, in
//! batch mode, until it is asked to stop between syncs (`SIGUSR2`), or
//! stopped (`tests/watch.rs` has `-repeat watch`). With `GEMEL_REFERENCE_UNISON`, OCaml's `unison -ui text` is
//! driven the same way on a twin pair of replicas, and both runs must end
//! the same.

#![cfg(unix)]

mod common;
#[path = "../../../crates/gemel-engine/tests/support/mod.rs"]
mod support;
#[path = "../../../crates/gemel-ui-api/tests/ui_support/mod.rs"]
mod ui_support;

use common::{Driven, same_driven, same_keys, twins, twins_each};
use support::sync::T0;
use support::write;
use ui_support::Pair;

/// Replicas synced once, then a new file in each and a conflict.
fn changed(tag: &str) -> (Pair, Pair) {
    let (g, o) = twins(tag, &["times = true"], |r, _| write(r, "c", "c\n", T0));
    changed_in(g, o)
}

/// [`changed`], for each way of reaching root `b` (see
/// [`common::twins_each`]).
fn changed_each(tag: &str) -> Vec<(Pair, Pair)> {
    let each = twins_each(tag, &["times = true"], |r, _| write(r, "c", "c\n", T0));
    each.into_iter().map(|(g, o)| changed_in(g, o)).collect()
}

fn changed_in(g: Pair, o: Pair) -> (Pair, Pair) {
    same_keys(&g, &o, &["p", "-batch"], None);
    for p in [&g, &o] {
        write(&p.a(), "new-a", "a", T0 + 1.0);
        write(&p.b(), "new-b", "b", T0 + 2.0);
        write(&p.a(), "c", "a's c\n", T0 + 3.0);
        write(&p.b(), "c", "b's c\n", T0 + 4.0);
    }
    (g, o)
}

/// A sync, then another (the conflict left), each followed by a sleep;
/// asked to stop, it does so at once, with the last sync's exit code.
#[test]
fn repeats_until_asked_to_stop() {
    for (g, o) in changed_each("repeat-stop") {
        // Long enough a sleep for the stop to come during it.
        let r = same_driven(&g, &o, &["p", "-repeat", "3"], |p| {
            p.wait_for_err("Sleeping for 3 seconds...", 2);
            p.signal("USR2");
        });
        assert_eq!(r.code, Some(1), "{r:#?}");
    }
}

/// Ctrl-C while it sleeps stops it.
#[test]
fn ctrl_c_while_it_sleeps() {
    for (g, o) in changed_each("repeat-ctrl-c") {
        let r = common::same_signals(Driven::start, &g, &o, &["p", "-repeat", "30"], |p| {
            p.wait_for_err("Sleeping for 30 seconds...", 1);
            p.signal("INT");
        });
        assert_eq!(r.code, Some(3), "{r:#?}");
        assert_eq!(r.stderr.last().map(String::as_str), Some("Terminated!"));
    }
}

/// A setting that isn't an interval is an error once the roots are
/// connected. (OCaml leaves the exception uncaught, and prints it with a
/// backtrace: only gemel's run is checked.)
#[test]
fn a_setting_that_is_not_an_interval() {
    let (g, _) = changed("repeat-bad");
    let r = common::gemel(&g, &["p", "-repeat", "often"]);
    assert_eq!(r.code, Some(3), "{r:#?}");
    assert_eq!(
        r.stderr.last().map(String::as_str),
        Some(
            "Error: Value of 'repeat' preference (often) should be either a number, \
             'watch' or 'watch+<number>'"
        ),
        "{r:#?}"
    );
}
