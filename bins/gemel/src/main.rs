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
//!
//! `main.ml`'s options handled before any profile is loaded: `-version`,
//! and (not supported yet) `-doc`, `-prefsdocs`, `-prefsman`, `-server`
//! and `-socket`; then the text interface, whatever `-ui` asks for.

mod infos;
mod input;
mod text;

use gemel_base::uarg::UsageError;
use gemel_base::util::{MY_NAME, MY_VERSION};
use gemel_ui_api::cli::{self, Interface};
use gemel_ui_api::exit_code::ExitCode;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::ExitCode::from(run(args) as u8)
}

/// `Main.init`, then the interface: the exit code.
fn run(args: Vec<String>) -> i32 {
    let prefs = match gemel_ui_api::new_prefs() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::Fatal as i32;
        }
    };
    let argv = match prefs.scan_cmd_line(&args) {
        Ok(a) => a,
        Err(e) => return usage_error(&e),
    };
    if argv.contains_key(cli::VERSION.name()) {
        println!("{MY_NAME} version {MY_VERSION}");
        return 0;
    }
    for (pref, what) in [
        (cli::PREFS_DOCS.name(), "-prefsdocs"),
        (cli::PREFS_MAN.name(), "-prefsman"),
        (cli::DOC.name(), "-doc"),
        (cli::SERVER.name(), "-server"),
        (cli::SOCKET.name(), "-socket"),
    ] {
        if argv.contains_key(pref) {
            eprintln!("{MY_NAME}: {what} is not supported yet");
            return 1;
        }
    }
    let interface = match argv.get(cli::UI.name()).and_then(|v| v.first()) {
        None => Interface::Text,
        Some(s) if s == "text" => Interface::Text,
        Some(s) if s == "graphic" => Interface::Graphic,
        Some(_) => {
            // `Prefs.printUsage`, without the list of options (phase 8b).
            println!("{}", gemel_ui_api::uicommon::usage_msg());
            return 1;
        }
    };
    text::start(args, interface)
}

/// `Uarg.parse`'s `stop`: the error, then the usage; exits with 2. For
/// `-help`, only the usage, on stdout. (The list of options is phase 8b.)
fn usage_error(e: &UsageError) -> i32 {
    let progname = std::env::args().next().unwrap_or_else(|| "(?)".to_owned());
    match e {
        UsageError::Unknown(s) if s == "-help" => {}
        e => eprintln!("{progname}: {e}."),
    }
    println!("{}", gemel_ui_api::uicommon::usage_msg());
    2
}
