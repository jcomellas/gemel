//! `gemel-gui` as a whole gemel, as `unison-gui` is a whole unison: the
//! options `main.ml` handles before any interface, the text interface
//! with `-ui text` or without a display, and the server modes.
//!
//! Every run but the one without a display passes `-ui text` or serves:
//! on macOS and Windows, `gemel-gui` would otherwise open its window.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::Duration;

const GEMEL_GUI: &str = env!("CARGO_BIN_EXE_gemel-gui");

/// A directory of its own for a test, removed when it ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("gemel-gui-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    /// Replica `a` with one file, and an empty replica `b`.
    fn replicas(tag: &str) -> TempDir {
        let d = TempDir::new(tag);
        std::fs::create_dir(d.0.join("a")).unwrap();
        std::fs::create_dir(d.0.join("b")).unwrap();
        std::fs::write(d.0.join("a").join("f.txt"), "contents\n").unwrap();
        d
    }

    /// Whether the file reached replica `b`.
    fn synced(&self) -> bool {
        std::fs::read_to_string(self.0.join("b").join("f.txt")).is_ok_and(|s| s == "contents\n")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `gemel-gui` with `args`, in `dir`, with its Unison directory there.
fn command(dir: &Path, unison: &str, args: &[&str]) -> Command {
    let mut c = Command::new(GEMEL_GUI);
    c.args(args)
        .current_dir(dir)
        .env("UNISON", dir.join(unison))
        .env("HOME", dir)
        .stdin(Stdio::null());
    c
}

fn run(dir: &Path, args: &[&str]) -> Output {
    command(dir, "u", args).output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn the_version_is_gemels() {
    let d = TempDir::new("version");
    let out = run(&d.0, &["-version"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        text(&out.stdout).starts_with("gemel version "),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn ui_text_runs_the_text_interface() {
    let d = TempDir::replicas("text");
    let out = run(&d.0, &["-ui", "text", "a", "b", "-batch"]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{stdout}{stderr}");
    assert!(d.synced(), "{stdout}{stderr}");
    assert!(!stderr.contains("only provides the text GUI"), "{stderr}");
    assert!(!stderr.contains("Warning"), "{stderr}");
}

/// As `uigtk3`'s `Body.start`, on systems that show windows through X11
/// or Wayland.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn without_a_display_the_text_interface_runs() {
    let d = TempDir::replicas("no-display");
    let out = command(&d.0, "u", &["a", "b", "-batch"])
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{stdout}{stderr}");
    assert!(d.synced(), "{stdout}{stderr}");
    assert!(
        stderr.starts_with(
            "Warning: DISPLAY and WAYLAND_DISPLAY not set or empty; starting the Text UI\n"
        ),
        "{stderr}"
    );
}

#[test]
fn server_mode_answers_as_gemels() {
    let d = TempDir::new("server");
    let mut child = command(&d.0, "u", &["-server", "__new-rpc-mode"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The end of its input ends it.
    child.stdin.take().unwrap().flush().unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(text(&out.stdout), "Unison RPC\nVERSIONS 1\n");
    assert!(d.0.join("u").is_dir());
}

/// A socket server, killed when dropped.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn socket_mode_serves_a_text_client() {
    let d = TempDir::replicas("socket");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
        .to_string();
    let mut child = command(&d.0, "su", &["-socket", &port, "-listen", "127.0.0.1"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut r = BufReader::new(child.stderr.take().unwrap());
    let server = Server(child);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = r.read_line(&mut line);
        let _ = tx.send(line);
    });
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(20)).as_deref(),
        Ok("server started\n")
    );
    let root = format!("socket://127.0.0.1:{port}/b");
    let out = run(&d.0, &["-ui", "text", "a", &root, "-batch"]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{stdout}{stderr}");
    assert!(d.synced(), "{stdout}{stderr}");
    drop(server);
}
