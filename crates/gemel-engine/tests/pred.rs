//! Predicates against OCaml's `pred.ml` (`pred.txt`, from `gen_pred.ml`):
//! matching and associated strings in every case mode, and the errors for
//! malformed patterns.

use gemel_base::case::Mode;
use gemel_engine::pred::{self, Compiled};

fn unhex(s: &str) -> Vec<u8> {
    if s == "-" {
        return Vec::new();
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn text(s: &str) -> String {
    String::from_utf8(unhex(s)).unwrap()
}

fn list(s: &str) -> Vec<String> {
    if s.is_empty() {
        Vec::new()
    } else {
        s.split(',').map(text).collect()
    }
}

fn mode(s: &str) -> Mode {
    match s {
        "sensitive" => Mode::Sensitive,
        "insensitive" => Mode::Insensitive,
        "unicode-sensitive" => Mode::UnicodeSensitive,
        "unicode-insensitive" => Mode::UnicodeInsensitive,
        _ => panic!("{s}"),
    }
}

const FIXTURES: &str = include_str!("../../gemel-compat/tests/fixtures/pred.txt");

#[test]
fn matching_matches_ocaml() {
    let mut n = 0;
    for line in FIXTURES.lines().filter(|l| l.starts_with("test\t")) {
        let f: Vec<&str> = line.split('\t').collect();
        let (m, patterns, s) = (mode(f[1]), list(f[2]), unhex(f[3]));
        let c = Compiled::new(patterns.clone(), m).unwrap();
        let what = format!("{m:?} {patterns:?} {:?}", String::from_utf8_lossy(&s));
        assert_eq!(c.test(&s).to_string(), f[4], "test {what}");
        assert_eq!(c.assoc_all(&s), list(f[5]), "assoc_all {what}");
        assert_eq!(
            c.assoc(&s),
            list(f[5]).first().map(String::as_str),
            "assoc {what}"
        );
        n += 1;
    }
    assert!(n > 100, "{n}");
}

#[test]
fn pattern_errors_match_ocaml() {
    let mut n = 0;
    for line in FIXTURES.lines().filter(|l| l.starts_with("compile\t")) {
        let f: Vec<&str> = line.split('\t').collect();
        let clause = text(f[1]);
        let got = pred::compile_pattern(&clause, Mode::Sensitive).map(|_| ());
        let expected = match f[2] {
            "ok" => Ok(()),
            _ => Err(text(f[3])),
        };
        assert_eq!(got, expected, "{clause:?}");
        n += 1;
    }
    assert!(n > 15, "{n}");
}
