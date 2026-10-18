//! `twinner-gui`: the graphical frontend.
//!
//! Will replace Unison's `src/uigtk3.ml` and `src/uimac/`. The toolkit is
//! deliberately not chosen yet: see §6 of `docs/unison-port-analysis.md`
//! for the spike that decides between Slint and egui (or Tauri). Whatever
//! wins, this crate talks to the engine only through `twinner-ui-api`.

use twinner_ui_api::exit_code::ExitCode;

fn main() -> std::process::ExitCode {
    eprintln!(
        "twinner-gui {}: not implemented yet",
        env!("CARGO_PKG_VERSION")
    );
    ExitCode::Fatal.into()
}
