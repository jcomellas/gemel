//! Generated scenarios through the window, with the default actions:
//! it must leave what `gemel -batch` leaves (milestone 4's definition of
//! done, item 2), with local roots, and with replica `b` over ssh and on
//! a socket.

mod support;

use gemel_oracle::run::Remote;

fn agree(tag: &str, remote: Option<Remote>, seeds: std::ops::RangeInclusive<u64>) {
    let work = support::Work::new(tag);
    for seed in seeds {
        let problems = support::disagreements(seed, &work.0, remote);
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }
}

#[test]
fn the_window_syncs_as_the_text_interface_does() {
    agree("scenarios", None, 1..=8);
}

#[test]
fn the_window_syncs_as_the_text_interface_does_over_ssh() {
    agree("scenarios-ssh", Some(Remote::Ssh), 1..=4);
}

#[test]
fn the_window_syncs_as_the_text_interface_does_on_a_socket() {
    agree("scenarios-socket", Some(Remote::Socket), 1..=4);
}
