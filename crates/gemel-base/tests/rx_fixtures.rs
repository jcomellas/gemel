//! Checks `gemel_base::rx` against golden vectors produced by Unison's own
//! Rx engine (gemel-compat's `tests/fixtures/rx.txt`, see
//! `scripts/gen-ocaml-fixtures.sh` and `tools/ocaml-fixtures/gen_rx.ml`).

use std::collections::HashMap;

use gemel_base::rx::{Rx, RxError};

const FIXTURE: &str = include_str!("../../gemel-compat/tests/fixtures/rx.txt");

fn unhex(s: &str) -> Vec<u8> {
    if s == "-" {
        return vec![];
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn b(x: bool) -> &'static str {
    if x { "1" } else { "0" }
}

fn prefix(r: &Rx, s: &[u8], p: usize) -> String {
    if p > s.len() {
        return "~".into();
    }
    r.match_prefix(s, p).map_or("~".into(), |n| n.to_string())
}

fn err(e: RxError) -> &'static str {
    match e {
        RxError::ParseError => "E:Parse_error",
        RxError::NotSupported => "E:Not_supported",
        RxError::InvalidArgument => "E:Invalid_argument",
    }
}

#[test]
fn rx_matches_ocaml() {
    let mut compiled: HashMap<(String, String), Result<Rx, RxError>> = HashMap::new();
    let (mut checked, mut divergent, mut mismatches) = (0, 0, Vec::new());
    for line in FIXTURE.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split('\t').collect();
        let (key, pattern, rest): ((String, String), Vec<u8>, &[&str]) = match f[0] {
            "rx" | "ci" => ((f[0].into(), f[1].into()), unhex(f[1]), &f[2..]),
            "glob" => (
                (format!("glob{}{}", f[1], f[2]), f[3].into()),
                unhex(f[3]),
                &f[4..],
            ),
            other => panic!("unknown line kind {other}"),
        };
        let r = compiled.entry(key).or_insert_with(|| match f[0] {
            "rx" => Rx::regex(&pattern),
            "ci" => Rx::regex(&pattern).map(|r| r.case_insensitive()),
            _ => {
                let nodot = f[1] == "1";
                if f[2] == "1" {
                    Rx::globx_with(nodot, &pattern)
                } else {
                    Rx::glob_with(nodot, &pattern)
                }
            }
        });
        let actual: Vec<String> = match (r, rest[0].starts_with("E:")) {
            (Err(e), _) => vec![err(*e).into()],
            (Ok(_), true) => vec!["compiled".into()],
            (Ok(r), false) => {
                let s = unhex(rest[0]);
                let mut v = vec![rest[0].to_owned(), b(r.match_string(&s)).into()];
                match f[0] {
                    "rx" => {
                        v.push(b(r.match_substring(&s)).into());
                        v.push(prefix(r, &s, 0));
                        v.push(prefix(r, &s, 1));
                    }
                    "glob" => {
                        v.push(b(r.match_substring(&s)).into());
                        v.push(prefix(r, &s, 0));
                    }
                    _ => {}
                }
                v
            }
        };
        // The one known divergence (see the rx module's documentation):
        // match_substring with a leading `^`, in column 2.
        let known = actual != rest
            && f[0] == "rx"
            && pattern.starts_with(b"^")
            && actual.len() == rest.len()
            && (0..rest.len()).all(|i| i == 2 || actual[i] == rest[i]);
        if known {
            divergent += 1;
        } else if actual != rest {
            mismatches.push(format!(
                "{} {:?}: expected {rest:?}, got {actual:?}",
                f[0],
                String::from_utf8_lossy(&pattern)
            ));
        }
        checked += 1;
    }
    assert!(checked > 11_000, "{checked}");
    assert!(
        divergent < 100,
        "{divergent} anchored match_substring results differ"
    );
    assert!(
        mismatches.is_empty(),
        "{} of {checked} differ:\n{}",
        mismatches.len(),
        mismatches[..mismatches.len().min(25)].join("\n")
    );
}
