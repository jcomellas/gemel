//! The `gemel` program in interactive mode in a terminal (a
//! pseudo-terminal), so in raw mode: keys are read as they are typed,
//! without a return, and not echoed by the terminal; each is typed once
//! the program asks. With `GEMEL_REFERENCE_UNISON`, OCaml's
//! `unison -ui text` is driven the same way on a twin pair of replicas,
//! and both runs must end the same.

#![cfg(unix)]
// `openpty`.
#![allow(unsafe_code)]

mod common;
#[path = "../../../crates/gemel-engine/tests/support/mod.rs"]
mod support;
#[path = "../../../crates/gemel-ui-api/tests/ui_support/mod.rs"]
mod ui_support;

use std::ffi::OsStr;
use std::fs::File;
use std::os::fd::{FromRawFd, OwnedFd};
use std::process::Stdio;

use common::{Driven, same_driven_by, same_keys, twins};
use support::sync::T0;
use support::write;
use ui_support::Pair;

/// Starts `prog` with stdin and stdout on a pseudo-terminal (`TERM=dumb`,
/// so that what shows the path being scanned is plain), and stderr on a
/// pipe.
fn in_terminal(prog: &OsStr, pair: &Pair, args: &[&str]) -> Driven {
    let (mut master, mut slave) = (0, 0);
    // SAFETY: the out-pointers are valid; no name, settings or size are
    // given.
    let r = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(r, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: both were just opened, and nothing else owns them.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let mut c = common::command(prog, pair, args);
    c.env("TERM", "dumb")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave))
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    // Only the child has the terminal's other end now.
    drop(c);
    let stderr = child.stderr.take().unwrap();
    let reader = File::from(master.try_clone().unwrap());
    Driven::new(
        child,
        Box::new(File::from(master)),
        Box::new(reader),
        Box::new(stderr),
    )
}

/// Replicas synced once, then with a conflict and new files.
fn items(tag: &str) -> (Pair, Pair) {
    let (g, o) = twins(tag, &["times = true"], |r, _| write(r, "c", "c\n", T0));
    same_keys(&g, &o, &["p", "-batch"], None);
    for p in [&g, &o] {
        write(&p.a(), "c", "a's c\n", T0 + 1.0);
        write(&p.b(), "c", "b's c\n", T0 + 2.0);
        write(&p.a(), "new-a", "a", T0 + 3.0);
        write(&p.b(), "new-b", "b", T0 + 4.0);
    }
    (g, o)
}

/// Each key as the program asks: the details, a key that is an escape
/// sequence, back, the help, a key that doesn't exist, the proceed menu,
/// and quitting.
#[test]
fn keys_in_raw_mode() {
    let (g, o) = items("term-keys");
    let r = same_driven_by(in_terminal, &g, &o, &["p"], |p| {
        for (n, key) in ["x", "\x1b[B", "p", "?", "z", "f", ">", "", "s", "R", "q"]
            .iter()
            .enumerate()
        {
            p.wait_for("] ", n + 1);
            p.type_keys(if key.is_empty() {
                b"\r"
            } else {
                key.as_bytes()
            });
        }
    });
    assert_eq!(r.code, Some(3), "{r:#?}");
}

/// In raw mode a signal interrupts the read, so a sync's question is
/// asked again after each Ctrl-C, and the fourth stops the program.
#[test]
fn interrupts_in_raw_mode() {
    let lines = [
        "times = true",
        "confirmmerge = true",
        "merge = Name f -> cat CURRENT1 CURRENT2 > NEW",
    ];
    let (g, o) = twins("term-intr", &lines, |r, _| write(r, "f", "same\n", T0));
    same_keys(&g, &o, &["p", "-batch"], None);
    for p in [&g, &o] {
        write(&p.a(), "f", "a's\n", T0 + 10.0);
        write(&p.b(), "f", "b's\n", T0 + 11.0);
    }
    let r = common::same_signals(in_terminal, &g, &o, &["p"], |p| {
        p.wait_for("[f] ", 1);
        p.type_keys(b"\r");
        p.wait_for("Proceed with propagating updates? [] ", 1);
        p.type_keys(b"y");
        p.wait_for("Commit results of merge? [] ", 1);
        for n in 0..4 {
            p.signal("INT");
            if n < 3 {
                p.wait_for("Commit results of merge? [] ", n + 2);
            }
        }
    });
    assert_eq!(r.code, Some(3), "{r:#?}");
}
