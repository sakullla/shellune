//! `latest.json`: the newest version and, per platform, its file and signature.
//!
//! ```json
//! {
//!   "version": "0.5.0",
//!   "notes": "…",
//!   "pub_date": "2026-11-20T08:00:00Z",
//!   "platforms": {
//!     "windows-x86_64": { "url": "…-setup.exe", "signature": "<base64 .sig>" },
//!     "windows-x86_64-portable": { "url": "…-portable.zip", "signature": "…" },
//!     "darwin-aarch64": { "url": "…-macos-arm64.app.tar.gz", "signature": "…" },
//!     "linux-x86_64": { "url": "…_amd64.deb", "signature": "…" }
//!   }
//! }
//! ```
//!
//! Platform keys this build does not know are kept and ignored.

use std::collections::BTreeMap;
use std::fmt;

use semver::Version;
use serde::Deserialize;

use crate::UpdateError;

/// The largest manifest read.
pub(crate) const MANIFEST_LIMIT: u64 = 1024 * 1024;

/// A platform entry of the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PlatformKey {
    /// The Windows installer (`setup.exe`).
    WindowsX86_64,
    /// The Windows portable archive (`.zip` holding `shellune.exe`).
    WindowsX86_64Portable,
    /// The macOS application archive (`.app.tar.gz`).
    DarwinAarch64,
    /// The Linux package (`.deb`); only shown, never installed by the app.
    LinuxX86_64,
}

impl PlatformKey {
    /// Every key, in manifest order.
    pub const ALL: [Self; 4] = [
        Self::WindowsX86_64,
        Self::WindowsX86_64Portable,
        Self::DarwinAarch64,
        Self::LinuxX86_64,
    ];

    /// The key as written in the manifest.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WindowsX86_64 => "windows-x86_64",
            Self::WindowsX86_64Portable => "windows-x86_64-portable",
            Self::DarwinAarch64 => "darwin-aarch64",
            Self::LinuxX86_64 => "linux-x86_64",
        }
    }

    /// The key a manifest name stands for.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.as_str() == name)
    }

    /// The key of the platform this build runs on, for showing what a release
    /// offers (the install route decides what is downloaded).
    #[must_use]
    pub const fn native() -> Option<Self> {
        if cfg!(all(windows, target_arch = "x86_64")) {
            Some(Self::WindowsX86_64)
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Some(Self::DarwinAarch64)
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Some(Self::LinuxX86_64)
        } else {
            None
        }
    }
}

impl fmt::Display for PlatformKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One platform's file.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Asset {
    /// Where to download it.
    pub url: String,
    /// Its minisign signature: the base64 of the `.sig` file's text. May be empty
    /// in a broken manifest, which makes the file unusable.
    #[serde(default)]
    pub signature: String,
}

/// The parsed manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// The newest version.
    pub version: Version,
    /// The release notes, when the manifest has them.
    pub notes: Option<String>,
    /// When the release was published, as written in the manifest.
    pub pub_date: Option<String>,
    /// The files by manifest key; unknown keys are kept.
    pub platforms: BTreeMap<String, Asset>,
}

#[derive(Deserialize)]
struct RawManifest {
    version: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    pub_date: Option<String>,
    #[serde(default)]
    platforms: BTreeMap<String, Asset>,
}

impl Manifest {
    /// Reads a manifest.
    ///
    /// # Errors
    ///
    /// [`UpdateError::InvalidManifest`] when the text is not a manifest or its
    /// version is not a semantic version.
    pub fn parse(bytes: &[u8]) -> Result<Self, UpdateError> {
        let raw: RawManifest =
            serde_json::from_slice(bytes).map_err(|error| UpdateError::InvalidManifest {
                reason: error.to_string(),
            })?;
        let version =
            parse_version(&raw.version).map_err(|reason| UpdateError::InvalidManifest {
                reason: format!("the version \"{}\" is not valid: {reason}", raw.version),
            })?;
        let blank = |text: Option<String>| text.filter(|text| !text.trim().is_empty());
        Ok(Self {
            version,
            notes: blank(raw.notes),
            pub_date: blank(raw.pub_date),
            platforms: raw.platforms,
        })
    }

    /// The file for `key`, when the manifest lists one with a usable address.
    #[must_use]
    pub fn asset(&self, key: PlatformKey) -> Option<&Asset> {
        self.platforms
            .get(key.as_str())
            .filter(|asset| !asset.url.trim().is_empty())
    }
}

/// Reads a version, with or without a leading `v`.
///
/// # Errors
///
/// The parser's message when the text is not a semantic version.
pub fn parse_version(text: &str) -> Result<Version, String> {
    let text = text.trim();
    let text = text
        .strip_prefix('v')
        .or_else(|| text.strip_prefix('V'))
        .unwrap_or(text);
    Version::parse(text).map_err(|error| error.to_string())
}

/// Whether `candidate` is newer than `current` (semantic version order: a
/// pre-release is older than its release; build metadata is ignored).
#[must_use]
pub fn is_newer(candidate: &Version, current: &Version) -> bool {
    candidate.cmp_precedence(current).is_gt()
}
