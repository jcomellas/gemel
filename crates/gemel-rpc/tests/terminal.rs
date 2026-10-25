//! ssh under a terminal of its own: a shim standing in for it asks for a
//! password at its terminal, and what it prints there reaches the user;
//! once the connection is up, no more is asked.

#![cfg(unix)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use gemel_rpc::runtime;
use gemel_rpc::terminal::{self, Interact};
use tokio::io::AsyncReadExt;

/// A shim that asks for a password at its terminal twice at most, says it
/// is connected on its standard output once the answer is `secret`, then
/// prints a banner at its terminal.
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

fn shim() -> std::process::Command {
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", SHIM])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    cmd
}

#[test]
fn a_password_is_asked_for_until_the_connection_is_up() {
    runtime::run_here(async {
        let (i, seen) = interact(&["wrong", "secret"]);
        let (term, mut child) = terminal::create_session(shim()).unwrap();
        let term = term.expect("a terminal");
        let requests = terminal::handle_password_requests(term.clone(), i);
        // The server's first bytes: the connection is up.
        let mut stdout = child.stdout.take().unwrap();
        let mut b = [0; 2];
        tokio::time::timeout(Duration::from_secs(10), stdout.read_exact(&mut b))
            .await
            .expect("connected")
            .unwrap();
        assert_eq!(&b, b"UP");
        requests.ready();
        // The banner comes after: not shown to the user.
        tokio::time::sleep(Duration::from_millis(300)).await;
        // What ssh printed, in as many pieces as it came in (pieces more
        // than 50 ms apart are shown apart, as in OCaml).
        let seen = seen.borrow();
        let all = seen.shown.concat();
        assert!(
            seen.shown[0].starts_with("me@server's password: "),
            "{all:?}"
        );
        assert_eq!(all.matches("password: ").count(), 2, "{all:?}");
        assert!(
            all.contains("Permission denied, please try again."),
            "{all:?}"
        );
        assert!(!all.contains("banner"), "{all:?}");
        assert_eq!(seen.ended, 1);
        term.close();
        terminal::end_session(child);
    })
    .unwrap();
}

#[test]
fn what_ssh_said_last_is_kept_when_it_fails() {
    runtime::run_here(async {
        let (i, seen) = interact(&["wrong", "wrong"]);
        let (term, mut child) = terminal::create_session(shim()).unwrap();
        let term = term.expect("a terminal");
        let requests = terminal::handle_password_requests(term.clone(), i);
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .expect("ended")
            .unwrap();
        assert_eq!(status.code(), Some(255));
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
        let (term, child) = terminal::create_session(shim()).unwrap();
        let term = term.expect("a terminal");
        let requests = terminal::handle_password_requests(term.clone(), i);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(seen.borrow().shown, ["me@server's password: "]);
        // Once input has ended, an answer goes nowhere.
        assert_eq!(requests.ready(), "");
        assert_eq!(seen.borrow().ended, 1);
        term.close();
        terminal::end_session(child);
        tokio::time::sleep(Duration::from_millis(100)).await;
    })
    .unwrap();
}
