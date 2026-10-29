//! Compiles `ui/app.slint` into Rust.

fn main() {
    // Element debug info lets the tests query elements through
    // `i-slint-backend-testing`; keep it out of release builds so the size
    // measurement is honest.
    let debug = std::env::var("PROFILE").is_ok_and(|p| p == "debug");
    let mut config = slint_build::CompilerConfiguration::new().with_debug_info(debug);
    // The widgets in the system's look: Slint's macOS style there, Fluent
    // elsewhere (which Slint picks without Qt). `SLINT_STYLE` still wins.
    println!("cargo:rerun-if-env-changed=SLINT_STYLE");
    if std::env::var_os("SLINT_STYLE").is_none()
        && std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "macos")
    {
        config = config.with_style("cupertino".to_owned());
    }
    slint_build::compile_with_config("ui/app.slint", config).expect("compile ui/app.slint");
}
