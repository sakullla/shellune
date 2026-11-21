# Repository Guidelines

## Project Structure

Shellune is a Rust 2024 workspace. The desktop application starts in `src/main.rs`, and its GPUI views live in `src/app/`. Put shared domain types in `crates/core/` and terminal emulation or local shell handling in `crates/terminal/`. SSH and SFTP share `crates/ssh/`. RDP and VNC share `crates/desktop/`. Serial sessions, storage, sync, and updates each have their own crate under `crates/`.

Application icons live in `assets/icons/`. Platform installers and release tooling live in `packaging/`. See `vendor/README.md` for the patches carried by vendored dependencies.

## Build and Development Commands

Run commands from the repository root using the toolchain pinned in `rust-toolchain.toml`. On Linux, consult `.github/workflows/ci.yml` for graphics headers and integration test dependencies.

- `cargo build --workspace` builds the application and workspace crates.
- `cargo run` launches the desktop application.
- `cargo fmt --all -- --check` checks formatting. Use `cargo fmt --all` to apply it.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` runs the CI lint check.

## Coding Style

Use four-space indentation and rustfmt. Use `snake_case` for modules and functions, `UpperCamelCase` for types, and `SCREAMING_SNAKE_CASE` for constants. Keep transport details in the crate that owns the protocol so shared models can be tested without opening a connection or window.

## Testing

Keep unit tests beside the implementation in `#[cfg(test)]` modules. Use the GPUI test harness for window interactions. Place integration tests in `tests/` or the owning crate’s `tests/` directory. Name files by feature, such as `tests/ssh_forwarding.rs`, and test functions by expected behavior, such as `a_blank_name_falls_back_to_the_program`.

Run `cargo test -p shellune-core` for focused model changes. CI runs `cargo test --workspace --all-features -- --include-ignored` with `SHELLUNE_REQUIRE_NATIVE=1` on Windows, macOS, and Linux.

## Commits and Pull Requests

Follow the history’s `type(scope): summary` pattern, for example `fix(desktop): stabilize idle VNC reads on Windows`. Omit the scope for cross-cutting changes. In a pull request, describe the observable change and list the checks and platforms tested. Link a related issue when available and include a screenshot for visible UI changes.
