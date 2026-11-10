//! `gemel-gui`: gemel with its graphical interface. See the library's
//! documentation.

// On Windows, a program for the window, as OCaml builds `unison-gui`
// (`-subsystem windows`): started from the Start menu, it opens no console
// beside the window. Its other runs get a console when they need one
// (`gemel_fs::terminal::init_console`).
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    gemel_gui::main()
}
