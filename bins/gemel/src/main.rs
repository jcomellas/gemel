//! `gemel`: the command-line and text UI, and the `-server` mode run on
//! remote hosts.
//!
//! Ports Unison `src/main.ml`, `src/uitext.ml` and `src/linktext.ml`.
//!
//! This binary must never depend on a GUI toolkit: it is what runs on
//! headless servers. Command-line flags follow Unison's single-dash style
//! (`-server`, `-batch`, `-ui text`), parsed by `gemel_base::uarg`, because
//! OCaml Unison clients invoke the server side as `<servercmd> -server`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.

use gemel_ui_api::exit_code::ExitCode;

fn main() -> std::process::ExitCode {
    eprintln!("gemel {}: not implemented yet", env!("CARGO_PKG_VERSION"));
    ExitCode::Fatal.into()
}
