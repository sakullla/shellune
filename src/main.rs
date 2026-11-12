//! Shellune application entry point.
//!
//! The binary only wires the process up: the GPUI shell lives in [`app`] and the
//! render-independent domain model lives in the `shellune-core` crate.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;

fn main() {
    // The capture probe is a second entry of the same binary, and only when the
    // feature is asked for: a normal launch never sees it (R1).
    #[cfg(feature = "ui-capture")]
    {
        let arguments: Vec<String> = std::env::args().skip(1).collect();
        if app::capture::invoked(&arguments) {
            std::process::exit(app::capture::main(arguments));
        }
    }
    app::run();
}
