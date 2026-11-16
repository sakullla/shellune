//! Self-update for Shellune (R2, R3).
//!
//! The release pipeline publishes `latest.json` next to the release files. It names
//! the newest version and, per platform, the file to download and its minisign
//! signature. An update is:
//!
//! 1. [`Updater::check`] — read the manifest and compare its version with the one
//!    running ([`CheckOutcome`]);
//! 2. [`Updater::download`] — fetch the file for this copy's [`InstallRoute`],
//!    reporting progress, and keep it only when its signature verifies against
//!    the built-in [`PUBLIC_KEY`] *and* the signature's trusted comment names the
//!    manifest's version ([`DownloadedUpdate`]);
//! 3. [`Updater::install`] — put it in place for the route: start the Windows
//!    installer silently, swap the portable `shellune.exe`, replace the macOS
//!    `.app`, or point at the release page ([`InstallOutcome`]).
//!
//! Every call blocks; the application runs them on a background thread. Nothing
//! here starts a thread, a timer or a user interface. A refused or interrupted
//! download never touches the running copy, and an install that fails half way
//! puts the old files back.
//!
//! Requests use the same client setup as WebDAV sync: `ureq` with rustls and the
//! system certificate verifier, and the proxy named by `HTTPS_PROXY` /
//! `HTTP_PROXY` / `ALL_PROXY` unless `NO_PROXY` lists the host.
//!
//! Debug builds read two overrides: `SHELLUNE_DEBUG_UPDATE_ENDPOINT` (another
//! manifest address) and `SHELLUNE_DEBUG_UPDATE_INSTALL_PATH` (act as if this were
//! the running executable). Without the latter a debug build only offers the
//! release page. Release builds ignore both.

mod error;
mod http;
mod install;
mod manifest;
mod signature;
mod updater;

#[cfg(any(test, feature = "release-tools", feature = "test-support"))]
pub mod release;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use error::UpdateError;
pub use install::{
    InstallOutcome, InstallRoute, ManualReason, RestartTarget, cleanup_portable_leftovers,
    current_route, installer_arguments, relaunch, route_for, route_for_executable,
};
pub use manifest::{Asset, Manifest, PlatformKey, is_newer, parse_version};
pub use signature::{verify_bytes, verify_file};
pub use updater::{
    CheckOutcome, DownloadProgress, DownloadedUpdate, Release, Updater, UpdaterConfig,
};

/// Where the newest release's manifest is published.
pub const ENDPOINT: &str =
    "https://github.com/sakullla/shellune/releases/latest/download/latest.json";

/// The releases of the project, for the manual route.
pub const RELEASES_URL: &str = "https://github.com/sakullla/shellune/releases";

/// The minisign public key every update must be signed with (the base64 key line
/// of the public key file). The secret half never enters the repository.
pub const PUBLIC_KEY: &str = "RWSVMHqzPzrTBqbrVIEdI+JIRcUIOtmj1/RYpAMLMQkG4E8T2PqL1WKP";

/// The version of this build.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Debug builds: another manifest address.
pub const DEBUG_ENDPOINT_VARIABLE: &str = "SHELLUNE_DEBUG_UPDATE_ENDPOINT";

/// Debug builds: the executable path to act as (which decides the install route).
pub const DEBUG_INSTALL_PATH_VARIABLE: &str = "SHELLUNE_DEBUG_UPDATE_INSTALL_PATH";

/// The release page of `version` (`…/releases/tag/v<version>`).
#[must_use]
pub fn release_page(version: &semver::Version) -> String {
    format!("{RELEASES_URL}/tag/v{version}")
}

/// The manifest address: [`ENDPOINT`], or in a debug build the value of
/// [`DEBUG_ENDPOINT_VARIABLE`] when it is set.
#[must_use]
pub fn endpoint() -> String {
    debug_override(DEBUG_ENDPOINT_VARIABLE).unwrap_or_else(|| ENDPOINT.to_owned())
}

/// A non-empty environment value, read only by debug builds.
fn debug_override(name: &str) -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}
