//! ssh under a terminal of its own: a shim standing in for it asks for a
//! password at its terminal (a pseudo-terminal, or on Windows a
//! pseudo-console), and what it prints there reaches the user; once the
//! connection is up, no more is asked.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use gemel_rpc::runtime;
use gemel_rpc::terminal::{self, Child, Interact};
use tokio::io::AsyncReadExt;

/// A shim that asks for a password at its terminal twice at most, says it
/// is connected on its standard output once the answer is `secret`, then
/// prints a banner at its terminal.
#[cfg(unix)]
const SHIM: &str = "\
n=0
while :; do
  printf \"me@server's password: \" > /dev/tty
  read -r pw < /dev/tty
  [ \"$pw\" = secret ] && break
  n=$((n+1))
  [ $n -ge 2 ] && { echo 'Permission denied (publickey,password).' > /dev/tty; exit 255; }
  echo 'Permission denied, please try again.' > /dev/tty
done
echo UP
echo 'Welcome, this is a banner' > /dev/tty
sleep 2
";

/// What the user was shown, and how many times input ended.
#[derive(Default)]
struct Seen {
    shown: Vec<String>,
    ended: usize,
}

/// An interaction answering with `answers`, in turn, what it records in
/// the returned `Seen`.
fn interact(answers: &[&str]) -> (Interact, Rc<RefCell<Seen>>) {
    let seen = Rc::new(RefCell::new(Seen::default()));
    let answers: Rc<RefCell<VecDeque<String>>> = Rc::new(RefCell::new(
        answers.iter().map(|s| s.to_string()).collect(),
    ));
    let (s1, s2) = (seen.clone(), seen.clone());
    let i = Interact {
        user_input: Box::new(move |text, respond| {
            s1.borrow_mut().shown.push(text);
            if let Some(a) = answers.borrow_mut().pop_front() {
                respond(a);
            }
        }),
        end_input: Box::new(move || s2.borrow_mut().ended += 1),
    };
    (i, seen)
}

/// The same, in Python, reading the answer at the console a key at a
/// time until a return or a newline, as Windows OpenSSH reads a password
/// (`_getch`); a line-at-a-time read (`set /p`) wants a return.
#[cfg(windows)]
const SHIM: &str = r#"import msvcrt, sys, time
def say(s):
    with open("CONOUT$", "w") as c:
        c.write(s)
def ask():
    say("me@server's password: ")
    pw = ""
    while True:
        ch = msvcrt.getwch()
        if ch in "\r\n":
            return pw
        pw += ch
n = 0
while ask() != "secret":
    n += 1
    if n >= 2:
        say("Permission denied (publickey,password).\r\n")
        sys.exit(255)
    say("Permission denied, please try again.\r\n")
sys.stdout.write("UP")
sys.stdout.flush()
say("Welcome, this is a banner\r\n")
time.sleep(2)
"#;

#[cfg(unix)]
fn shim() -> std::process::Command {
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", SHIM])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    cmd
}

#[cfg(windows)]
fn shim() -> std::process::Command {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let script =
        std::env::temp_dir().join(format!("gemel-terminal-shim-{}-{n}.py", std::process::id()));
    std::fs::write(&script, SHIM).unwrap();
    // GitHub's Windows runners have Python on PATH.
    let mut cmd = std::process::Command::new("python");
    cmd.arg(script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    cmd
}

/// Waits for the shim to end: its exit code, where it can be had.
async fn ended(child: Child) -> Option<i32> {
    match child {
        Child::Spawned(mut c) => c.wait().await.ok().and_then(|s| s.code()),
        #[cfg(windows)]
        Child::OnConsole(p) => {
            runtime::blocking(move || p.wait(u32::MAX)).await;
            None
        }
    }
}

#[test]
fn a_password_is_asked_for_until_the_connection_is_up() {
    runtime::run_here(async {
        let (i, seen) = interact(&["wrong", "secret"]);
        let s = terminal::create_session(shim()).unwrap();
        let term = s.term.expect("a terminal");
        let requests = terminal::handle_password_requests(term.clone(), i);
        // The server's first bytes: the connection is up.
        let mut stdout = s.stdout;
        let mut b = [0; 2];
        tokio::time::timeout(Duration::from_secs(30), stdout.read_exact(&mut b))
            .await
            .expect("connected")
            .unwrap();
        assert_eq!(&b, b"UP");
        requests.ready();
        // The banner comes after: not shown to the user.
        tokio::time::sleep(Duration::from_millis(300)).await;
        // What ssh printed, in as many pieces as it came in (pieces more
        // than 50 ms apart are shown apart, as in OCaml).
        // (A pseudo-console may draw the screen again: there, the prompt
        // shows at least twice.)
        let seen = seen.borrow();
        let all = seen.shown.concat();
        assert!(
            seen.shown[0]
                .trim_start()
                .starts_with("me@server's password:"),
            "{all:?}"
        );
        let prompts = all.matches("password:").count();
        assert!(prompts == 2 || cfg!(windows) && prompts > 2, "{all:?}");
        assert!(
            all.contains("Permission denied, please try again."),
            "{all:?}"
        );
        assert!(!all.contains("banner"), "{all:?}");
        assert_eq!(seen.ended, 1);
        term.close();
        terminal::end_session(s.child);
    })
    .unwrap();
}

#[test]
fn what_ssh_said_last_is_kept_when_it_fails() {
    runtime::run_here(async {
        let (i, seen) = interact(&["wrong", "wrong"]);
        let s = terminal::create_session(shim()).unwrap();
        let term = s.term.expect("a terminal");
        let requests = terminal::handle_password_requests(term.clone(), i);
        let code = tokio::time::timeout(Duration::from_secs(30), ended(s.child))
            .await
            .expect("ended");
        if cfg!(unix) {
            assert_eq!(code, Some(255));
        }
        // ssh's last words: shown, if they came in time, or the warning.
        let last = requests.remaining().await;
        let seen = seen.borrow();
        let all = format!("{}{last}", seen.shown.join(""));
        assert!(
            all.contains("Permission denied (publickey,password)."),
            "{all:?}"
        );
        assert_eq!(seen.ended, 1);
        term.close();
    })
    .unwrap();
}

#[test]
fn without_answers_nothing_is_written() {
    runtime::run_here(async {
        let (i, seen) = interact(&[]);
        let s = terminal::create_session(shim()).unwrap();
        let term = s.term.expect("a terminal");
        let requests = terminal::handle_password_requests(term.clone(), i);
        // Until the prompt shows (Python may take its time to start).
        for _ in 0..300 {
            if !seen.borrow().shown.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let shown = seen.borrow().shown.concat();
        assert!(
            shown.trim_start().starts_with("me@server's password:"),
            "{shown:?}"
        );
        if cfg!(unix) {
            assert_eq!(seen.borrow().shown, ["me@server's password: "]);
        }
        // Once input has ended, an answer goes nowhere.
        assert_eq!(requests.ready(), "");
        assert_eq!(seen.borrow().ended, 1);
        term.close();
        terminal::end_session(s.child);
        tokio::time::sleep(Duration::from_millis(100)).await;
    })
    .unwrap();
}
