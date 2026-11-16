//! The three steps: check, download (and verify), install.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use std::time::Duration;

use semver::Version;

use crate::http::{self, CHECK_TIMEOUT, DOWNLOAD_IDLE_TIMEOUT, Purpose};
use crate::install::{
    self, BundleFailure, InstallOutcome, InstallRoute, ManualReason, RestartTarget,
};
use crate::manifest::{Asset, MANIFEST_LIMIT, Manifest, PlatformKey, is_newer, parse_version};
use crate::signature::{decode_public_key, verify_file};
use crate::{CURRENT_VERSION, PUBLIC_KEY, UpdateError, release_page};

/// The largest update file downloaded.
const DOWNLOAD_LIMIT: u64 = 1024 * 1024 * 1024;

/// What the updater talks to and acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdaterConfig {
    /// The manifest address.
    pub endpoint: String,
    /// The minisign public key update files must be signed with (base64 key line).
    pub public_key: String,
    /// The running version.
    pub current_version: Version,
    /// How this copy is updated.
    pub route: InstallRoute,
    /// Where downloads are kept until they are installed.
    pub download_dir: PathBuf,
    /// How long the manifest request may take in total.
    pub check_timeout: Duration,
    /// How long a download may go without receiving data. A download that keeps
    /// receiving has no time limit.
    pub download_idle_timeout: Duration,
}

impl UpdaterConfig {
    /// The running copy's settings: [`crate::endpoint`], [`PUBLIC_KEY`],
    /// [`CURRENT_VERSION`], [`install::current_route`], a `shellune-update`
    /// folder in the system's temporary folder, a 15-second check, and downloads
    /// that give up after 60 seconds without data.
    ///
    /// # Panics
    ///
    /// Never: the crate version is a semantic version.
    #[must_use]
    pub fn from_environment() -> Self {
        Self {
            endpoint: crate::endpoint(),
            public_key: PUBLIC_KEY.to_owned(),
            current_version: parse_version(CURRENT_VERSION).expect("the crate version is valid"),
            route: install::current_route(),
            download_dir: std::env::temp_dir().join("shellune-update"),
            check_timeout: CHECK_TIMEOUT,
            download_idle_timeout: DOWNLOAD_IDLE_TIMEOUT,
        }
    }
}

/// A newer release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// Its version.
    pub version: Version,
    /// Its notes, when the manifest has them.
    pub notes: Option<String>,
    /// When it was published, as the manifest writes it.
    pub pub_date: Option<String>,
    /// Its page, for the manual route and for reading more.
    pub release_page: String,
    /// The file this copy's route installs, when the release has a signed one.
    /// `None` when [`Self::unavailable`] says why not (the update is then offered
    /// through the release page only).
    pub asset: Option<(PlatformKey, Asset)>,
    /// Why this copy cannot download and install the release itself; `None`
    /// exactly when [`Self::asset`] is set.
    pub unavailable: Option<Unavailable>,
}

impl Release {
    /// Whether this copy can download and install the release itself.
    #[must_use]
    pub fn installable(&self) -> bool {
        self.asset.is_some()
    }

    /// The error [`Updater::download`] gives for this release when it is not
    /// installable.
    #[must_use]
    pub fn unavailable_error(&self) -> Option<UpdateError> {
        if self.asset.is_some() {
            return None;
        }
        Some(match self.unavailable {
            Some(Unavailable::SignatureMissing(_)) => UpdateError::SignatureMissing,
            _ => UpdateError::NoInstallableAsset,
        })
    }
}

/// Why a newer release cannot be installed by this copy itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// This copy is updated from the release page (see the reason).
    ManualRoute(ManualReason),
    /// The release has no file for this copy's platform and packaging.
    NoAsset(PlatformKey),
    /// The release lists the file for this copy, but without a signature: it is
    /// not downloaded ([`UpdateError::SignatureMissing`]).
    SignatureMissing(PlatformKey),
}

/// What a check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    /// The newest release is not newer than this one.
    UpToDate {
        /// The manifest's version.
        latest: Version,
    },
    /// A newer release.
    Available(Release),
}

/// How far a download is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownloadProgress {
    /// Bytes received so far.
    pub downloaded: u64,
    /// The size the server announced, when it did.
    pub total: Option<u64>,
}

/// A downloaded update file whose signature verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadedUpdate {
    path: PathBuf,
    version: Version,
    platform: PlatformKey,
    signature: String,
}

impl DownloadedUpdate {
    /// The file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The version it holds.
    #[must_use]
    pub fn version(&self) -> &Version {
        &self.version
    }

    /// The manifest entry it came from.
    #[must_use]
    pub fn platform(&self) -> PlatformKey {
        self.platform
    }
}

/// Checks, downloads and installs updates. Every call blocks.
#[derive(Clone, Debug)]
pub struct Updater {
    config: UpdaterConfig,
}

impl Updater {
    /// An updater with `config`.
    ///
    /// # Errors
    ///
    /// [`UpdateError::InvalidConfig`] when the public key cannot be read.
    pub fn new(config: UpdaterConfig) -> Result<Self, UpdateError> {
        decode_public_key(&config.public_key)?;
        Ok(Self { config })
    }

    /// An updater for the running copy ([`UpdaterConfig::from_environment`]).
    ///
    /// # Errors
    ///
    /// As [`Self::new`].
    pub fn from_environment() -> Result<Self, UpdateError> {
        Self::new(UpdaterConfig::from_environment())
    }

    /// The settings.
    #[must_use]
    pub fn config(&self) -> &UpdaterConfig {
        &self.config
    }

    /// How this copy is updated.
    #[must_use]
    pub fn route(&self) -> &InstallRoute {
        &self.config.route
    }

    /// Reads the manifest (in at most [`UpdaterConfig::check_timeout`]) and compares its version with
    /// the running one.
    ///
    /// # Errors
    ///
    /// [`UpdateError::Timeout`], [`UpdateError::Unreachable`],
    /// [`UpdateError::HttpStatus`] (any status but 200) and
    /// [`UpdateError::InvalidManifest`].
    pub fn check(&self) -> Result<CheckOutcome, UpdateError> {
        let manifest = self.fetch_manifest()?;
        Ok(self.evaluate(&manifest))
    }

    /// What `manifest` means for this copy.
    #[must_use]
    pub fn evaluate(&self, manifest: &Manifest) -> CheckOutcome {
        if !is_newer(&manifest.version, &self.config.current_version) {
            return CheckOutcome::UpToDate {
                latest: manifest.version.clone(),
            };
        }
        let (asset, unavailable) = match &self.config.route {
            InstallRoute::Manual { reason } => (None, Some(Unavailable::ManualRoute(*reason))),
            route => match route.platform_key() {
                None => (None, Some(Unavailable::ManualRoute(ManualReason::Platform))),
                Some(key) => match manifest.asset(key) {
                    None => (None, Some(Unavailable::NoAsset(key))),
                    Some(asset) if asset.signature.trim().is_empty() => {
                        (None, Some(Unavailable::SignatureMissing(key)))
                    }
                    Some(asset) => (Some((key, asset.clone())), None),
                },
            },
        };
        CheckOutcome::Available(Release {
            version: manifest.version.clone(),
            notes: manifest.notes.clone(),
            pub_date: manifest.pub_date.clone(),
            release_page: release_page(&manifest.version),
            asset,
            unavailable,
        })
    }

    fn fetch_manifest(&self) -> Result<Manifest, UpdateError> {
        let url = &self.config.endpoint;
        let agent = http::agent_for(url, Purpose::Check(self.config.check_timeout))?;
        let mut response = agent.get(url).call().map_err(http::classify)?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(UpdateError::HttpStatus { status });
        }
        let bytes = response
            .body_mut()
            .with_config()
            .limit(MANIFEST_LIMIT)
            .read_to_vec()
            .map_err(|error| match error {
                ureq::Error::BodyExceedsLimit(_) => UpdateError::InvalidManifest {
                    reason: "it is too large".to_owned(),
                },
                other => http::classify(other),
            })?;
        Manifest::parse(&bytes)
    }

    /// Removes the files left in the download folder by earlier downloads
    /// (installed or abandoned update files, partial downloads). Call it when the
    /// application starts; [`Self::download`] also calls it first.
    ///
    /// Only plain files directly in the folder are removed. A file that cannot
    /// be removed now (an installer still running, another copy downloading) is
    /// left for a later call. Returns how many files were removed.
    pub fn clean_downloads(&self) -> usize {
        clean_download_folder(&self.config.download_dir)
    }

    /// Downloads the release's file into the download folder, calling `progress`
    /// as bytes arrive, and keeps it only when its signature verifies for the
    /// release's version. Earlier downloads are removed first
    /// ([`Self::clean_downloads`]); a refused or broken download is deleted. The
    /// download has no total time limit: it fails only after
    /// [`UpdaterConfig::download_idle_timeout`] without data.
    ///
    /// # Errors
    ///
    /// [`UpdateError::SignatureMissing`] for a release whose file for this route
    /// is listed unsigned, [`UpdateError::NoInstallableAsset`] for any other
    /// release without an installable file (see [`Release::unavailable`]), the
    /// transport errors of [`Self::check`] ([`UpdateError::Timeout`] when the
    /// server goes quiet), [`UpdateError::DownloadInterrupted`],
    /// [`UpdateError::SignatureInvalid`], [`UpdateError::VersionMismatch`] and
    /// [`UpdateError::Io`].
    pub fn download(
        &self,
        release: &Release,
        progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<DownloadedUpdate, UpdateError> {
        let Some((platform, asset)) = &release.asset else {
            return Err(release
                .unavailable_error()
                .unwrap_or(UpdateError::NoInstallableAsset));
        };
        if asset.signature.trim().is_empty() {
            return Err(UpdateError::SignatureMissing);
        }
        let folder = &self.config.download_dir;
        fs::create_dir_all(folder).map_err(|error| {
            UpdateError::io(
                format!("the download folder {} cannot be made", folder.display()),
                &error,
            )
        })?;
        clean_download_folder(folder);
        let target = folder.join(file_name(&asset.url, *platform, &release.version));
        let mut partial = target.clone().into_os_string();
        partial.push(".part");
        let partial = PathBuf::from(partial);

        let fetched = fetch_to(
            &asset.url,
            &partial,
            self.config.download_idle_timeout,
            progress,
        )
        .and_then(|()| {
            verify_file(
                &self.config.public_key,
                &asset.signature,
                &partial,
                &release.version,
            )
        })
        .and_then(|()| {
            let _ = fs::remove_file(&target);
            fs::rename(&partial, &target)
                .map_err(|error| UpdateError::io("the downloaded file cannot be renamed", &error))
        });
        if let Err(error) = fetched {
            let _ = fs::remove_file(&partial);
            return Err(error);
        }
        Ok(DownloadedUpdate {
            path: target,
            version: release.version.clone(),
            platform: *platform,
            signature: asset.signature.clone(),
        })
    }

    /// Puts a downloaded update in place for this copy's route, after checking
    /// its signature again (the file sat on disk since the download).
    ///
    /// The Windows installer route returns [`InstallOutcome::InstallerStarted`]:
    /// the caller saves its state and exits. The portable and macOS routes return
    /// [`InstallOutcome::RestartRequired`]. A macOS folder that cannot be written,
    /// and the manual route, return [`InstallOutcome::Manual`] without changing
    /// anything.
    ///
    /// # Errors
    ///
    /// The signature errors of [`Self::download`], and [`UpdateError::Install`]
    /// when the file is for another route or cannot be put in place (whatever was
    /// changed has then been put back).
    pub fn install(&self, update: &DownloadedUpdate) -> Result<InstallOutcome, UpdateError> {
        verify_file(
            &self.config.public_key,
            &update.signature,
            &update.path,
            &update.version,
        )?;
        let route = &self.config.route;
        if let InstallRoute::Manual { reason } = route {
            return Ok(InstallOutcome::Manual {
                release_page: release_page(&update.version),
                reason: *reason,
            });
        }
        if route.platform_key() != Some(update.platform) {
            return Err(UpdateError::install(format!(
                "the downloaded file ({}) is not for this installation",
                update.platform
            )));
        }
        match route {
            InstallRoute::WindowsInstaller { .. } => install::launch_installer(&update.path),
            InstallRoute::WindowsPortable { executable } => {
                install::replace_portable(&update.path, executable, &|| Ok(()))?;
                Ok(InstallOutcome::RestartRequired(RestartTarget::Executable(
                    executable.clone(),
                )))
            }
            InstallRoute::MacApp { bundle } => {
                match install::replace_app_bundle(&update.path, bundle, &|| Ok(())) {
                    Ok(()) => Ok(InstallOutcome::RestartRequired(RestartTarget::AppBundle(
                        bundle.clone(),
                    ))),
                    Err(BundleFailure::NoPermission) => Ok(InstallOutcome::Manual {
                        release_page: release_page(&update.version),
                        reason: ManualReason::NoPermission,
                    }),
                    Err(BundleFailure::Failed(error)) => Err(error),
                }
            }
            InstallRoute::Manual { .. } => unreachable!("handled above"),
        }
    }
}

/// Removes the plain files directly in `folder`; returns how many went.
fn clean_download_folder(folder: &Path) -> usize {
    let Ok(entries) = fs::read_dir(folder) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter(|entry| fs::remove_file(entry.path()).is_ok())
        .count()
}

/// Streams `url` into `path`, giving up after `idle` without data.
fn fetch_to(
    url: &str,
    path: &Path,
    idle: Duration,
    progress: &mut dyn FnMut(DownloadProgress),
) -> Result<(), UpdateError> {
    let agent = http::agent_for(url, Purpose::Download(idle))?;
    let mut response = agent.get(url).call().map_err(http::classify)?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(UpdateError::HttpStatus { status });
    }
    let total = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    if total.is_some_and(|total| total > DOWNLOAD_LIMIT) {
        return Err(UpdateError::DownloadInterrupted {
            reason: "the file is larger than an update can be".to_owned(),
        });
    }
    let mut file = File::create(path)
        .map_err(|error| UpdateError::io("the download cannot be written", &error))?;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(DOWNLOAD_LIMIT)
        .reader();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut downloaded = 0u64;
    progress(DownloadProgress { downloaded, total });
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                // The body reader wraps ureq's own errors (a timeout among them)
                // in an io::Error; unwrap them to tell a silent server apart.
                return Err(match ureq::Error::from(error) {
                    ureq::Error::Timeout(_) => UpdateError::Timeout,
                    ureq::Error::Io(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                        UpdateError::Timeout
                    }
                    other => UpdateError::DownloadInterrupted {
                        reason: other.to_string(),
                    },
                });
            }
        };
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|error| UpdateError::io("the download cannot be written", &error))?;
        downloaded += read as u64;
        progress(DownloadProgress { downloaded, total });
    }
    if let Some(total) = total
        && downloaded != total
    {
        return Err(UpdateError::DownloadInterrupted {
            reason: format!("{downloaded} of {total} bytes arrived"),
        });
    }
    file.sync_all()
        .map_err(|error| UpdateError::io("the download cannot be written", &error))
}

/// The name the file is kept under: the last segment of its address when that is
/// a plain file name, else a name from the version and platform.
fn file_name(url: &str, platform: PlatformKey, version: &Version) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let last = path.rsplit('/').next().unwrap_or("");
    let plain = !last.is_empty()
        && last != "."
        && last != ".."
        && last
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-+".contains(&byte));
    if plain {
        return last.to_owned();
    }
    let suffix = match platform {
        PlatformKey::WindowsX86_64 => "windows-x86_64-setup.exe",
        PlatformKey::WindowsX86_64Portable => "windows-x86_64-portable.zip",
        PlatformKey::DarwinAarch64 => "macos-arm64.app.tar.gz",
        PlatformKey::LinuxX86_64 => "amd64.deb",
    };
    format!("Shellune-{version}-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloads_keep_a_plain_file_name_or_get_one() {
        let version = Version::new(1, 2, 3);
        assert_eq!(
            file_name(
                "https://x/releases/download/v1.2.3/Shellune-1.2.3-windows-x86_64-setup.exe?a=b",
                PlatformKey::WindowsX86_64,
                &version
            ),
            "Shellune-1.2.3-windows-x86_64-setup.exe"
        );
        assert_eq!(
            file_name(
                "https://x/download/..",
                PlatformKey::DarwinAarch64,
                &version
            ),
            "Shellune-1.2.3-macos-arm64.app.tar.gz"
        );
        assert_eq!(
            file_name(
                "https://x/a%20b.zip",
                PlatformKey::WindowsX86_64Portable,
                &version
            ),
            "Shellune-1.2.3-windows-x86_64-portable.zip"
        );
    }
}
