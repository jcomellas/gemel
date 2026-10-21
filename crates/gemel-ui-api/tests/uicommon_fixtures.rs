//! Checks `truncateString`, `quote` and `Stats` against OCaml Unison's own
//! (gemel-compat's `tests/fixtures/uicommon.txt`, see
//! `scripts/gen-ocaml-fixtures.sh` and `tools/ocaml-fixtures/gen_uicommon.ml`).

use gemel_base::util::truncate_string;
use gemel_ui_api::uicommon::quote;
use gemel_ui_api::uicommon::stats::Stats;

const FIXTURE: &str = include_str!("../../gemel-compat/tests/fixtures/uicommon.txt");

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

fn float(s: &str) -> f64 {
    if s == "nan" {
        f64::NAN
    } else {
        f64::from_bits(u64::from_str_radix(s, 16).unwrap())
    }
}

/// Equal, or both NaN, up to rounding: `exp` and `pow` may round the last
/// bit differently from the C library OCaml ran on.
fn close(a: f64, b: f64) -> bool {
    (a.is_nan() && b.is_nan()) || a == b || (a - b).abs() <= 1e-12 * a.abs().max(b.abs())
}

fn lines(kind: &str) -> impl Iterator<Item = Vec<&'static str>> {
    FIXTURE
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .filter(move |f| f[0] == kind)
}

#[test]
fn truncate_string_matches_ocaml() {
    let mut n = 0;
    for f in lines("truncate") {
        let (s, count) = (unhex(f[1]), f[2].parse().unwrap());
        assert_eq!(truncate_string(&s, count), text(f[3]), "{:?} {count}", f[1]);
        n += 1;
    }
    assert!(n > 100);
}

#[test]
fn quote_matches_ocaml() {
    let mut n = 0;
    for f in lines("quote") {
        assert_eq!(quote(&text(f[1])), text(f[2]), "{:?}", text(f[1]));
        n += 1;
    }
    assert!(n > 10);
}

#[test]
fn eta_matches_ocaml() {
    let mut n = 0;
    for f in lines("eta") {
        let sta = Stats::new(f[1].parse().unwrap());
        assert_eq!(
            sta.eta(Some(float(f[2])), "-"),
            f[3],
            "{} at {}",
            f[1],
            f[2]
        );
        n += 1;
    }
    assert!(n > 100);
}

#[test]
fn rates_match_ocaml() {
    let mut scenario: Option<(&str, Stats)> = None;
    let mut n = 0;
    let totals = [
        ("steady", 10_000_000),
        ("faster", 50_000_000),
        ("stall", 1_000_000),
        ("zero-start", 1000),
        ("jumps", 1_000_000_000),
    ];
    // The progress of each scenario, as gen_uicommon.ml makes it.
    let progress = |name: &str, i: usize| -> (f64, i64) {
        let i64_ = i as i64;
        match name {
            "steady" => (100. + 0.3 * i as f64, 1 + 100_000 * i64_),
            "faster" => (5. + 0.26 * i as f64, 1 + 1000 * i64_ * i64_),
            "stall" => [
                (1., 1),
                (1.3, 1000),
                (1.6, 2000),
                (1.9, 2000),
                (2.2, 2000),
                (2.3, 2000),
                (2.6, 2000),
                (5., 2000),
                (9., 500_000),
                (9.1, 500_000),
                (9.4, 999_999),
                (9.8, 1_000_000),
            ][i],
            "zero-start" => [(0., 0), (1., 0), (2., 10), (2.1, 20), (2.5, 40), (3., 400)][i],
            "jumps" => (
                i as f64 * 0.5,
                if i % 7 == 3 {
                    1 + i64_ * 10
                } else {
                    1 + i64_ * 1_000_000
                },
            ),
            other => panic!("unknown scenario {other}"),
        }
    };
    for f in lines("stats") {
        let (name, i): (&str, usize) = (f[1], f[2].parse().unwrap());
        if scenario.as_ref().is_none_or(|(s, _)| *s != name) {
            assert_eq!(i, 0);
            let total = totals.iter().find(|(s, _)| *s == name).unwrap().1;
            scenario = Some((name, Stats::new(total)));
        }
        let sta = &mut scenario.as_mut().unwrap().1;
        let (t, done) = progress(name, i);
        sta.update(t, done);
        let rates = [sta.cur_rate(), sta.avg_rate1(), sta.avg_rate2()];
        for (got, want) in rates.into_iter().zip(&f[3..6]) {
            assert!(close(got, float(want)), "{name} step {i}: {got} {want}");
        }
        assert_eq!(sta.eta(None, "-"), f[6], "{name} step {i}");
        n += 1;
    }
    assert!(n > 100);
}
