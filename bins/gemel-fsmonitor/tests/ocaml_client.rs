//! OCaml Unison's `-repeat watch` with this helper as its watcher, when
//! `GEMEL_REFERENCE_UNISON` names OCaml Unison.
//!
//! OCaml looks for `unison-fsmonitor` next to its binary, then on `PATH`,
//! so the test puts a copy of this helper under that name in a directory
//! of its own, first on `PATH`. Without a watcher, OCaml's watch mode
//! stops at once ("No file monitoring helper program found"), so a change
//! that reaches the other replica shows OCaml used this one.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "gemel-fsmonitor-ocaml-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::canonicalize(&d).unwrap()
}

fn until(what: &str, ok: impl Fn() -> bool) {
    let start = Instant::now();
    while !ok() {
        assert!(start.elapsed() < TIMEOUT, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn ocaml_unison_watches_with_this_helper() {
    let Some(unison) = std::env::var_os("GEMEL_REFERENCE_UNISON") else {
        return;
    };
    let base = dir("watch");
    let (a, b, bin, home) = (
        base.join("a"),
        base.join("b"),
        base.join("bin"),
        base.join("u"),
    );
    for d in [&a, &b, &bin, &home] {
        std::fs::create_dir(d).unwrap();
    }
    std::fs::copy(
        env!("CARGO_BIN_EXE_gemel-fsmonitor"),
        bin.join("unison-fsmonitor"),
    )
    .unwrap();
    std::fs::write(a.join("first"), b"1").unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let log = base.join("log");
    let mut child = Command::new(&unison)
        .args([a.as_os_str(), b.as_os_str()])
        .args(["-ui", "text", "-batch", "-repeat", "watch", "-log=false"])
        .env("UNISON", &home)
        .env("HOME", &base)
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&log).unwrap())
        .stderr(std::fs::File::create(base.join("err")).unwrap())
        .spawn()
        .unwrap();
    let exists = |p: &Path| p.exists();
    let result = std::panic::catch_unwind(|| {
        until("the first sync", || exists(&b.join("first")));
        // Changes from now on reach OCaml only through the watcher.
        std::thread::sleep(Duration::from_secs(1));
        std::fs::write(a.join("second"), b"2").unwrap();
        until("the change to propagate", || exists(&b.join("second")));
        std::fs::remove_file(b.join("first")).unwrap();
        until("the deletion to propagate", || !exists(&a.join("first")));
    });
    let _ = child.kill();
    let _ = child.wait();
    if result.is_err() {
        let read = |n: &str| std::fs::read_to_string(base.join(n)).unwrap_or_default();
        panic!("OCaml's output:\n{}\n{}", read("log"), read("err"));
    }
}
