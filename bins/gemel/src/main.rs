//! `gemel`: the command-line and text UI, and the server modes run on
//! remote hosts (`-server` and `-socket`). See the library's documentation.

fn main() -> std::process::ExitCode {
    gemel::main(std::env::args().skip(1).collect(), None)
}
