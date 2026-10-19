//! Checks the fsmonitor protocol's quoting against golden vectors from
//! Unison's own `fswatch.ml` code (the `quote`, `unquote` and `split` lines of
//! `tests/fixtures/base.txt`).

use gemel_compat::fsmonitor_protocol::{quote, split_on_space, unquote};

fn unhex(s: &str) -> Vec<u8> {
    if s == "-" {
        return vec![];
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn hex(b: &[u8]) -> String {
    if b.is_empty() {
        "-".into()
    } else {
        b.iter().map(|c| format!("{c:02x}")).collect()
    }
}

#[test]
fn quoting_matches_ocaml() {
    let mut checked = 0;
    for line in include_str!("fixtures/base.txt").lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let input = || unhex(f[1]);
        match f[0] {
            "quote" => assert_eq!(hex(&quote(&input())), f[2], "quote {}", f[1]),
            "unquote" => {
                let actual = unquote(&input()).map_or("!".into(), |u| hex(&u));
                assert_eq!(actual, f[2], "unquote {}", f[1]);
            }
            "split" => {
                let s = input();
                let (a, b) = split_on_space(&s);
                assert_eq!([hex(a), hex(b)], [f[2], f[3]], "split {}", f[1]);
            }
            _ => continue,
        }
        checked += 1;
    }
    assert!(checked >= 35, "{checked}");
}
