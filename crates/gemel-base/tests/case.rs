//! Checks `gemel_base::case` against golden vectors produced by OCaml Unison
//! (the `c` and `h` lines of gemel-compat's `unicode_strings.txt`).

use std::cmp::Ordering;

use gemel_base::case::Mode;

const MODES: [Mode; 4] = [
    Mode::Sensitive,
    Mode::Insensitive,
    Mode::UnicodeSensitive,
    Mode::UnicodeInsensitive,
];

fn unhex(s: &str) -> Vec<u8> {
    if s == "-" {
        return vec![];
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn sign(o: Ordering) -> String {
    (o as i8).to_string()
}

#[test]
fn compare_and_hash_match_ocaml_in_every_mode() {
    let text = include_str!("../../gemel-compat/tests/fixtures/unicode_strings.txt");
    let (mut compares, mut hashes) = (0, 0);
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let fields: Vec<&str> = line.split(' ').collect();
        match fields[0] {
            "c" => {
                let (a, b) = (unhex(fields[1]), unhex(fields[2]));
                let actual: Vec<String> = MODES.iter().map(|m| sign(m.compare(&a, &b))).collect();
                assert_eq!(actual, fields[3..], "{} vs {}", fields[1], fields[2]);
                compares += 1;
            }
            "h" => {
                let s = unhex(fields[1]);
                let actual: Vec<String> = MODES.iter().map(|m| m.hash(&s).to_string()).collect();
                assert_eq!(actual, fields[2..], "{}", fields[1]);
                hashes += 1;
            }
            _ => {}
        }
    }
    assert!(compares >= 2000 && hashes >= 600, "{compares} {hashes}");
}

/// case.ml's invariant: names that compare equal hash equally.
#[test]
fn equal_names_hash_equally() {
    let text = include_str!("../../gemel-compat/tests/fixtures/unicode_strings.txt");
    for line in text.lines().filter(|l| l.starts_with("c ")) {
        let fields: Vec<&str> = line.split(' ').collect();
        let (a, b) = (unhex(fields[1]), unhex(fields[2]));
        for m in MODES {
            if m.compare(&a, &b) == Ordering::Equal {
                assert_eq!(
                    m.hash(&a),
                    m.hash(&b),
                    "{m:?}: {} vs {}",
                    fields[1],
                    fields[2]
                );
            }
        }
    }
}
