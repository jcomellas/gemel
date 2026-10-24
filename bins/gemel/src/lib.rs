//! The command-line and text UI, and the server modes run on remote hosts
//! (`-server` and `-socket`): what the `gemel` program runs.
//!
//! Ports Unison `src/main.ml`, `src/uitext.ml` and `src/linktext.ml`.
//!
//! This crate must never depend on a GUI toolkit: it is what runs on
//! headless servers. Command-line flags follow Unison's single-dash style
//! (`-server`, `-batch`, `-ui text`), parsed by `gemel_base::uarg`, because
//! OCaml Unison clients invoke the server side as `<servercmd> -server`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! `main.ml`'s options handled before any profile is loaded: `-version`,
//! `-prefsman short`, `-server`, `-socket` (with `-listen`), and (not
//! supported yet) `-doc`, `-prefsdocs` and `-prefsman full`; then the
//! interface `-ui` asks for. Without a graphical interface ([`main`]'s
//! `gui`), that is the text interface, whatever `-ui` asks for, as with
//! OCaml's text-only build; with one, as `gemel-gui` runs it, the
//! graphical interface by default, as `uigtk3`'s `Body.start` chooses.

mod infos;
mod input;
mod server;
mod text;

use gemel_base::uarg::UsageError;
use gemel_base::util::{MY_NAME, MY_VERSION};
use gemel_ui_api::cli::{self, Interface};
use gemel_ui_api::exit_code::ExitCode;

/// A graphical interface: runs on the command line's arguments, and
/// returns the exit code.
pub type Gui<'a> = &'a dyn Fn(Vec<String>) -> i32;

/// Runs the program on `args` (the command line without the program's
/// name), with the graphical interface `gui` if there is one, and ends
/// the shells it started (`end_ssh`) as it exits.
pub fn main(args: Vec<String>, gui: Option<Gui>) -> std::process::ExitCode {
    let code = run(args, gui);
    gemel_rpc::shell::end_shells();
    std::process::ExitCode::from(code as u8)
}

/// `Main.init`, then the interface: the exit code.
fn run(args: Vec<String>, gui: Option<Gui>) -> i32 {
    let prefs = match gemel_ui_api::new_prefs() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::Fatal as i32;
        }
    };
    let argv = match prefs.scan_cmd_line(&args) {
        Ok(a) => a,
        Err(e) => return usage_error(&prefs, &e),
    };
    if argv.contains_key(cli::VERSION.name()) {
        println!("{MY_NAME} version {MY_VERSION}");
        return 0;
    }
    if argv.contains_key(cli::PREFS_DOCS.name()) {
        eprintln!("{MY_NAME}: -prefsdocs is not supported yet");
        return 1;
    }
    if let Some(v) = argv.get(cli::PREFS_MAN.name()) {
        match v.first().map(String::as_str) {
            Some("short") => print!("{}", prefs.usage_for_man()),
            Some("full") => {
                eprintln!("{MY_NAME}: -prefsman full is not supported yet");
                return 1;
            }
            _ => {}
        }
        return 0;
    }
    let unsupported = |what: &str| {
        eprintln!("{MY_NAME}: {what} is not supported yet");
        1
    };
    if argv.contains_key(cli::DOC.name()) {
        return unsupported("-doc");
    }
    if argv.contains_key(cli::SERVER.name()) {
        return server::run();
    }
    if let Some(port) = argv.get(cli::SOCKET.name()).and_then(|v| v.first()) {
        let hosts: Vec<String> = [cli::LISTEN.name(), "host"]
            .iter()
            .flat_map(|k| argv.get(*k).into_iter().flatten().cloned())
            .collect();
        return server::run_socket(&hosts, port);
    }
    let interface = match argv.get(cli::UI.name()).and_then(|v| v.first()) {
        None if gui.is_some() => Interface::Graphic,
        None => Interface::Text,
        Some(s) if s == "text" => Interface::Text,
        Some(s) if s == "graphic" => Interface::Graphic,
        Some(_) => {
            print_usage(&prefs);
            return 1;
        }
    };
    match (interface, gui) {
        (Interface::Graphic, Some(gui)) if display_available() => gui(args),
        (Interface::Graphic, Some(_)) => {
            eprintln!(
                "Warning: DISPLAY and WAYLAND_DISPLAY not set or empty; starting the Text UI"
            );
            text::start(args, Interface::Text)
        }
        (interface, _) => text::start(args, interface),
    }
}

/// Whether a graphical interface can show a window, as `uigtk3`'s
/// `Body.start` decides: on X11 and Wayland systems, `DISPLAY` or
/// `WAYLAND_DISPLAY` must be set and not empty. `uigtk3` checks them
/// everywhere but on Windows; macOS, which has neither without XQuartz,
/// always has a display here.
fn display_available() -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        return true;
    }
    ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty()))
}

/// `Uarg.parse`'s `stop`: the error, then the usage; exits with 2. For
/// `-help`, only the usage, on stdout.
fn usage_error(prefs: &gemel_base::prefs::Prefs, e: &UsageError) -> i32 {
    let progname = std::env::args().next().unwrap_or_else(|| "(?)".to_owned());
    match e {
        UsageError::Unknown(s) if s == "-help" => {}
        e => eprintln!("{progname}: {e}."),
    }
    print_usage(prefs);
    2
}

/// `Prefs.printUsage` (`Uarg.usage`): the usage and the options.
fn print_usage(prefs: &gemel_base::prefs::Prefs) {
    println!(
        "{}",
        prefs.one_line_docs(&gemel_ui_api::uicommon::usage_msg(), "", "")
    );
}
