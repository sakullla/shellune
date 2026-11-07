//! Shellune application entry point.
//!
//! The binary only wires the process up: the GPUI shell lives in [`app`] and the
//! render-independent domain model lives in the `shellune-core` crate.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;

fn main() {
    app::run();
}
