//! Compiles `ui/app.slint` into Rust.

fn main() {
    // Element debug info lets the tests query elements through
    // `i-slint-backend-testing`; keep it out of release builds so the size
    // measurement is honest.
    let debug = std::env::var("PROFILE").is_ok_and(|p| p == "debug");
    let config = slint_build::CompilerConfiguration::new().with_debug_info(debug);
    slint_build::compile_with_config("ui/app.slint", config).expect("compile ui/app.slint");
}
