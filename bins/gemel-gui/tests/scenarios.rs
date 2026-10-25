//! Generated scenarios through the window, with the default actions:
//! it must leave what `gemel -batch` leaves (milestone 4's definition of
//! done, item 2), for now with local roots.

mod support;

#[test]
fn the_window_syncs_as_the_text_interface_does() {
    let work = support::Work::new("scenarios");
    for seed in 1..=8 {
        let problems = support::disagreements(seed, &work.0);
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }
}
