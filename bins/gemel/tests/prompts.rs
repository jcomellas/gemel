//! ssh's prompts as a graphical interface gets them: a session with
//! `prompts` runs ssh under a terminal of its own, and what ssh prints
//! there comes as questions, until the connection is up. A shim standing
//! in for ssh asks for a password at its terminal, then runs the server
//! (`gemel`) once the answer is right.

#![cfg(unix)]

mod common;
#[path = "../../../crates/gemel-engine/tests/support/mod.rs"]
mod support;
#[path = "../../../crates/gemel-ui-api/tests/ui_support/mod.rs"]
mod ui_support;

use std::sync::mpsc;
use std::time::Duration;

use gemel_base::trace::Trace;
use gemel_ui_api::session::{self, Command, Config, Event, Question};
use support::sync::T0;
use support::write;
use ui_support::Pair;

/// A session on `pair`'s profile `p`, with `prompts`, and its events.
fn start(pair: &Pair) -> (session::Handle, mpsc::Receiver<Event>) {
    let mut s = session::spawn(Config {
        unison_dir: pair.u(),
        args: vec!["p".to_owned(), "-batch".to_owned()],
        trace: Trace::with_sinks(Box::new(std::io::sink()), Box::new(std::io::sink())),
        loopback: false,
        status_events: true,
        scan_status: false,
        log_warnings: false,
        print_wait_message: false,
        complete_progress: true,
        traffic: None,
        prompts: true,
    })
    .unwrap();
    let (tx, rx) = mpsc::channel();
    s.forward_events(move |e| e.is_none_or(|e| tx.send(e).is_ok()))
        .unwrap();
    (s, rx)
}

/// The next event that isn't a status.
fn next(rx: &mpsc::Receiver<Event>) -> Event {
    loop {
        match rx.recv_timeout(Duration::from_secs(30)).expect("an event") {
            Event::Status(_) | Event::Contacting(_) => {}
            e => return e,
        }
    }
}

/// Against `gemel` as the server, and OCaml's when
/// `GEMEL_REFERENCE_UNISON` names it.
#[test]
fn ssh_prompts_come_as_questions() {
    prompts_with(common::gemel_path());
    if let Some(ocaml) = common::ocaml_path() {
        prompts_with(&ocaml);
    }
}

fn prompts_with(server: &std::ffi::OsStr) {
    let pair = Pair::remote("prompts", server);
    write(&pair.a(), "f", "f\n", T0);
    std::fs::write(
        pair.base().join("ssh"),
        format!(
            "#!/bin/sh\n\
             n=0\n\
             while :; do\n\
               printf \"me@server's password: \" > /dev/tty\n\
               read -r pw < /dev/tty\n\
               [ \"$pw\" = secret ] && break\n\
               n=$((n+1)); [ $n -ge 3 ] && exit 255\n\
               echo 'Permission denied, please try again.' > /dev/tty\n\
             done\n\
             while [ \"$1\" != -e ]; do shift; done; shift 2\n\
             UNISONLOCALHOSTNAME={} UNISON={} HOME={} exec \"$@\"\n",
            ui_support::SERVER,
            pair.u().display(),
            pair.base().display()
        ),
    )
    .unwrap();
    pair.profile("p", &[]);
    let (s, rx) = start(&pair);
    s.send(Command::Start);
    let Event::Started(Some(p)) = next(&rx) else {
        panic!()
    };
    s.send(Command::LoadProfile(p));
    assert!(matches!(next(&rx), Event::ProfileLoaded(_)));
    s.send(Command::Connect);
    let root = pair.root("b");
    let mut shown = String::new();
    // A wrong answer, then the right one.
    for answer in ["wrong", "secret"] {
        match next(&rx) {
            Event::Ask(Question::Password {
                root: r,
                text,
                reply,
            }) => {
                assert_eq!(r, root);
                shown += &text;
                if !text.ends_with("password: ") {
                    // The rest of the prompt, apart.
                    let Event::Ask(Question::Password { text, reply, .. }) = next(&rx) else {
                        panic!()
                    };
                    shown += &text;
                    reply.send(Some(answer.to_owned()));
                } else {
                    reply.send(Some(answer.to_owned()));
                }
            }
            e => panic!("{e:?}"),
        }
    }
    assert!(shown.starts_with("me@server's password: "), "{shown:?}");
    assert!(
        shown.contains("Permission denied, please try again."),
        "{shown:?}"
    );
    match next(&rx) {
        Event::InputEnded(r) => assert_eq!(r, root),
        e => panic!("{e:?}"),
    }
    let Event::Connected(names) = next(&rx) else {
        panic!()
    };
    assert_eq!(names.hosts.1, ui_support::SERVER);
    s.send(Command::Update {
        allow_partial: true,
        want_watcher: false,
        paths: None,
        only: None,
    });
    loop {
        match next(&rx) {
            Event::Ask(Question::Warning(_, r)) => r.send(true),
            Event::Warning(_) => {}
            Event::Updates(u) => {
                assert_eq!(u.items.len(), 1);
                break;
            }
            e => panic!("{e:?}"),
        }
    }
    s.join().unwrap();
}
