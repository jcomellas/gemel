//! The program itself. (Other packages' tests copy it as their ssh shim,
//! so it must be built when the workspace's tests are; a test of its own
//! sees to that.)

use std::process::Command;

#[test]
fn without_a_reference_it_gives_its_usage() {
    let out = Command::new(env!("CARGO_BIN_EXE_gemel-oracle"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage: gemel-oracle"), "{stderr}");
}
