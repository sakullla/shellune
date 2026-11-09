//! Canonical product identity.
//!
//! The window title, identifiers and product copy are defined once here so the
//! application shell, packaging and documentation cannot drift apart (R18).

/// Product name shown to users.
pub const NAME: &str = "Shellune";

/// English product tagline.
pub const TAGLINE_EN: &str = "A modern terminal for local shells, SSH, and SFTP.";

/// Chinese product tagline.
pub const TAGLINE_ZH: &str = "连接远程，也专注本地。";

/// Application identifier used for window grouping and desktop integration.
pub const APP_ID: &str = "shellune";

/// Cargo package (and therefore binary) name of the main application.
///
/// Packaging, release notes and the identity checks in the delivery step read it
/// from here instead of hard-coding the name again (R18).
#[allow(dead_code)]
pub const PACKAGE_NAME: &str = env!("CARGO_PKG_NAME");
