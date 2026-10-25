//! Checks the parser of control sequences and the patterns for OpenSSH's
//! messages against OCaml Unison's own (gemel-compat's
//! `tests/fixtures/terminal.txt`, see `scripts/gen-ocaml-fixtures.sh` and
//! `tools/ocaml-fixtures/gen_terminal.ml`).

use gemel_rpc::terminal::{authenticity, passphrase, password, process_escapes};

const FIXTURE: &str = include_str!("../../gemel-compat/tests/fixtures/terminal.txt");

fn unhex(s: &str) -> Vec<u8> {
    if s == "-" {
        return Vec::new();
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn lines(kind: &str) -> Vec<Vec<&'static str>> {
    FIXTURE
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .filter(|f| f[0] == kind)
        .collect()
}

#[test]
fn escapes_are_processed_as_ocaml_processes_them() {
    let lines = lines("escapes");
    assert!(lines.len() > 50);
    for f in lines {
        let input = unhex(f[1]);
        assert_eq!(process_escapes(&input), unhex(f[2]), "{}", f[1]);
    }
}

#[test]
fn sshs_messages_are_recognised_as_ocaml_recognises_them() {
    let lines = lines("ssh");
    assert!(lines.len() > 10);
    for f in lines {
        let s = unhex(f[1]);
        let got =
            [password(&s), passphrase(&s), authenticity(&s)].map(|b| if b { "1" } else { "0" });
        assert_eq!(got, [f[2], f[3], f[4]], "{}", String::from_utf8_lossy(&s));
    }
}
