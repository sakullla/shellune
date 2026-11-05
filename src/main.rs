//! Shellune application entry point.
//!
//! The binary only wires the process up: the GPUI shell lives in [`app`] and the
//! render-independent domain model lives in the `shellune-core` crate.

mod app;

fn main() {
    app::run();
}
