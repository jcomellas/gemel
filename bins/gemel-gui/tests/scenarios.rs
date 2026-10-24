//! The harness's scenarios, run with the text interface on both sides: the
//! harness itself must find them the same. Phase 1 runs the window as one
//! side.

mod support;

use gemel_oracle::run::Pairing;

#[test]
fn the_text_interface_agrees_with_itself() {
    let work = support::Work::new("scenarios");
    let text = Pairing {
        client: support::gemel_gui(),
        server: support::gemel_gui(),
    };
    for seed in 1..=5 {
        let problems = support::disagreements(seed, &text, &work.0);
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }
}
