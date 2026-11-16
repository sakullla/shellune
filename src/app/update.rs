//! Self-update in the application: where an update is, what each failure is called,
//! and the one seam the window talks to (R2, R3).
//!
//! The update crate (`shellune-update`) does the work - read the manifest, download
//! and verify, put the file in place - and every one of its calls blocks. This module
//! holds what the window needs around it without drawing anything:
//!
//! * [`UpdateBackend`], the calls the window makes, implemented by the crate's
//!   [`Updater`]. The tests hand the window a backend of their own (a real updater
//!   against a loopback server, with the installer and the restart replaced), which is
//!   how an update is driven without starting an installer or a second process;
//! * [`UpdateStage`], the one place an update is: checking, the newest version, a
//!   release on offer, downloading, downloaded and waiting to be installed, installing,
//!   installed and waiting for a restart, or failed - with the step that failed;
//! * the sentences of every failure and of every reason a release is only offered on
//!   its page, kept apart so a missing signature, a release without a file for this
//!   computer, a signature that does not verify and a download that stopped or went
//!   quiet never read as the same thing.

use std::io;
use std::time::Duration;

use shellune_update::{
    CheckOutcome, DownloadProgress, DownloadedUpdate, InstallOutcome, InstallRoute, ManualReason,
    PlatformKey, Release, RestartTarget, Unavailable, UpdateError, Updater,
};

use crate::app::copy::{self, UpdateStrings};

/// How long after the window opens the automatic check runs. Starting is not slowed
/// by it, and a window closed at once never sends anything.
pub const STARTUP_CHECK_DELAY: Duration = Duration::from_secs(20);

/// How often the progress of a running download is drawn again.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);

/// The update calls the window makes. Every one blocks, so the window only calls them
/// on its background executor.
pub trait UpdateBackend: Send + Sync {
    /// How this copy is updated.
    fn route(&self) -> InstallRoute;

    /// Reads the manifest and compares it with the running version.
    ///
    /// # Errors
    ///
    /// As [`Updater::check`].
    fn check(&self) -> Result<CheckOutcome, UpdateError>;

    /// Downloads and verifies the release's file for this copy.
    ///
    /// # Errors
    ///
    /// As [`Updater::download`].
    fn download(
        &self,
        release: &Release,
        progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<DownloadedUpdate, UpdateError>;

    /// Puts a verified file in place for this copy's route.
    ///
    /// # Errors
    ///
    /// As [`Updater::install`].
    fn install(&self, update: &DownloadedUpdate) -> Result<InstallOutcome, UpdateError>;

    /// Starts the new version after an in-place update.
    ///
    /// # Errors
    ///
    /// The system's error when it cannot be started.
    fn relaunch(&self, target: &RestartTarget) -> io::Result<()>;

    /// Removes what earlier updates left: old downloads, and the previous portable
    /// executable. Called once when the window opens.
    fn clean_up(&self);
}

impl UpdateBackend for Updater {
    fn route(&self) -> InstallRoute {
        Updater::route(self).clone()
    }

    fn check(&self) -> Result<CheckOutcome, UpdateError> {
        Updater::check(self)
    }

    fn download(
        &self,
        release: &Release,
        progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<DownloadedUpdate, UpdateError> {
        Updater::download(self, release, progress)
    }

    fn install(&self, update: &DownloadedUpdate) -> Result<InstallOutcome, UpdateError> {
        Updater::install(self, update)
    }

    fn relaunch(&self, target: &RestartTarget) -> io::Result<()> {
        shellune_update::relaunch(target)
    }

    fn clean_up(&self) {
        self.clean_downloads();
        clean_portable_leftovers(Updater::route(self));
    }
}

/// Removes the `.old` executable a portable update left beside this copy. Nothing
/// else is touched, and a file still in use is left for the next start.
pub fn clean_portable_leftovers(route: &InstallRoute) {
    if let InstallRoute::WindowsPortable { executable } = route {
        let _ = shellune_update::cleanup_portable_leftovers(executable);
    }
}

/// The step an update failed in, which decides what "try again" does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateStep {
    /// Reading the manifest.
    Check,
    /// Downloading and verifying the file.
    Download,
    /// Putting it in place.
    Install,
}

/// Where the window's update is.
#[derive(Clone, Debug)]
pub enum UpdateStage {
    /// Nothing has been checked in this run.
    Idle,
    /// The manifest is being read. `manual` when a person asked, so a failure is said.
    Checking {
        /// Whether a person asked.
        manual: bool,
    },
    /// This is the newest version.
    Latest {
        /// The manifest's version.
        latest: String,
        /// When the check ended.
        checked_unix: u64,
    },
    /// A newer release, not downloaded. When it is not installable by this copy its
    /// [`Release::unavailable`] says why, and only its page is offered.
    Available(Release),
    /// Its file is being downloaded.
    Downloading {
        /// The release.
        release: Release,
        /// Bytes received so far.
        downloaded: u64,
        /// The size the server announced, when it did.
        total: Option<u64>,
    },
    /// Its file is downloaded and verified, and waits to be installed.
    Ready {
        /// The release.
        release: Release,
        /// The file.
        update: DownloadedUpdate,
    },
    /// The file is being put in place.
    Installing {
        /// The release.
        release: Release,
    },
    /// The new version is in place and runs from the next start.
    Restart {
        /// The release.
        release: Release,
        /// What to start.
        target: RestartTarget,
    },
    /// A step failed. Nothing of this copy was changed.
    Failed {
        /// The step.
        step: UpdateStep,
        /// Why.
        error: UpdateError,
        /// The release the step was for, when one is known.
        release: Option<Release>,
        /// The verified file, when the install failed after the download.
        update: Option<DownloadedUpdate>,
    },
}

impl UpdateStage {
    /// The newer release this stage is about, if any.
    #[must_use]
    pub fn release(&self) -> Option<&Release> {
        match self {
            Self::Available(release)
            | Self::Downloading { release, .. }
            | Self::Ready { release, .. }
            | Self::Installing { release }
            | Self::Restart { release, .. } => Some(release),
            Self::Failed { release, .. } => release.as_ref(),
            Self::Idle | Self::Checking { .. } | Self::Latest { .. } => None,
        }
    }

    /// Whether a background call is running for it.
    #[must_use]
    pub fn busy(&self) -> bool {
        matches!(
            self,
            Self::Checking { .. } | Self::Downloading { .. } | Self::Installing { .. }
        )
    }

    /// A stable name of the stage, for the tests and the captures.
    #[must_use]
    #[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Checking { manual: false } => "checking",
            Self::Checking { manual: true } => "checking-manual",
            Self::Latest { .. } => "latest",
            Self::Available(_) => "available",
            Self::Downloading { .. } => "downloading",
            Self::Ready { .. } => "ready",
            Self::Installing { .. } => "installing",
            Self::Restart { .. } => "restart",
            Self::Failed { .. } => "failed",
        }
    }
}

/// The work an install or a restart would stop, counted for the question asked first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActiveWork {
    /// Terminal sessions: local shells and SSH, open or connecting.
    pub terminals: usize,
    /// File transfers still running.
    pub transfers: usize,
    /// Remote desktops (RDP, VNC), open or connecting.
    pub desktops: usize,
    /// Serial sessions, open or opening.
    pub serials: usize,
}

impl ActiveWork {
    /// Whether nothing would stop.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.terminals + self.transfers + self.desktops + self.serials == 0
    }

    /// One line per kind that has any, in the window's language.
    #[must_use]
    pub fn lines(self, strings: &UpdateStrings) -> Vec<String> {
        [
            (self.terminals, strings.confirm_terminals),
            (self.transfers, strings.confirm_transfers),
            (self.desktops, strings.confirm_desktops),
            (self.serials, strings.confirm_serials),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, template)| copy::fill(template, &[("count", &count.to_string())]))
        .collect()
    }
}

/// What a press on "install" or "restart" goes on to do once it is confirmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmPurpose {
    /// Put the downloaded file in place.
    Install,
    /// Start the new version and quit.
    Restart,
}

/// A platform key as a person reads it.
#[must_use]
pub fn platform_text(key: PlatformKey, strings: &UpdateStrings) -> &'static str {
    match key {
        PlatformKey::WindowsX86_64 => strings.platform_windows_installer,
        PlatformKey::WindowsX86_64Portable => strings.platform_windows_portable,
        PlatformKey::DarwinAarch64 => strings.platform_macos,
        PlatformKey::LinuxX86_64 => strings.platform_linux,
    }
}

/// Why a newer release is only offered on its page.
#[must_use]
pub fn unavailable_text(reason: Unavailable, strings: &UpdateStrings) -> String {
    match reason {
        Unavailable::ManualRoute(ManualReason::Platform) => strings.manual_platform.to_owned(),
        Unavailable::ManualRoute(ManualReason::DebugBuild) => strings.manual_debug.to_owned(),
        Unavailable::ManualRoute(ManualReason::NoPermission) => {
            strings.manual_no_permission.to_owned()
        }
        Unavailable::NoAsset(key) => copy::fill(
            strings.no_asset,
            &[("platform", platform_text(key, strings))],
        ),
        Unavailable::SignatureMissing(key) => copy::fill(
            strings.signature_missing_listed,
            &[("platform", platform_text(key, strings))],
        ),
    }
}

/// Why a step failed, in the window's language. A download that went quiet is a
/// timeout of the download, not of the server's first answer.
#[must_use]
pub fn error_text(error: &UpdateError, step: UpdateStep, strings: &UpdateStrings) -> String {
    let detail = |reason: &str| copy::fill(strings.error_detail, &[("reason", reason)]);
    match error {
        UpdateError::InvalidConfig { reason } => {
            format!("{} {}", strings.error_config, detail(reason))
        }
        UpdateError::Timeout if step == UpdateStep::Check => strings.error_check_timeout.to_owned(),
        UpdateError::Timeout => strings.error_download_timeout.to_owned(),
        UpdateError::Unreachable { reason } => {
            format!("{} {}", strings.error_unreachable, detail(reason))
        }
        UpdateError::HttpStatus { status } => {
            copy::fill(strings.error_http, &[("status", &status.to_string())])
        }
        UpdateError::InvalidManifest { reason } => {
            format!("{} {}", strings.error_manifest, detail(reason))
        }
        UpdateError::NoInstallableAsset => strings.error_no_asset.to_owned(),
        UpdateError::SignatureMissing => strings.error_signature_missing.to_owned(),
        UpdateError::SignatureInvalid { reason } => {
            format!("{} {}", strings.error_signature_invalid, detail(reason))
        }
        UpdateError::VersionMismatch { expected, signed } => copy::fill(
            strings.error_version_mismatch,
            &[("expected", expected), ("signed", signed)],
        ),
        UpdateError::DownloadInterrupted { reason } => {
            format!("{} {}", strings.error_interrupted, detail(reason))
        }
        UpdateError::Io { context, reason } => {
            format!(
                "{} {}",
                strings.error_io,
                detail(&format!("{context}: {reason}"))
            )
        }
        UpdateError::Install { reason } => {
            format!("{} {}", strings.error_install, detail(reason))
        }
    }
}

/// A byte count as a person reads it.
#[must_use]
pub fn size_text(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    let value = bytes as f64;
    if value >= MIB {
        format!("{:.1} MB", value / MIB)
    } else {
        format!("{:.0} KB", (value / 1024.0).ceil())
    }
}

/// How far a download is, as one line.
#[must_use]
pub fn progress_text(downloaded: u64, total: Option<u64>, strings: &UpdateStrings) -> String {
    match total {
        Some(total) if total > 0 => {
            let percent = (downloaded.min(total) * 100 / total).min(100);
            copy::fill(
                strings.progress_of,
                &[
                    ("percent", &percent.to_string()),
                    ("done", &size_text(downloaded)),
                    ("total", &size_text(total)),
                ],
            )
        }
        _ => copy::fill(strings.progress_bytes, &[("done", &size_text(downloaded))]),
    }
}

/// The fraction of a download that arrived, when its size is known.
#[must_use]
pub fn progress_fraction(downloaded: u64, total: Option<u64>) -> Option<f32> {
    total
        .filter(|total| *total > 0)
        .map(|total| (downloaded.min(total) as f32 / total as f32).clamp(0.0, 1.0))
}

/// The release notes as a few lines: long notes are cut where a line ends, so the card
/// stays a card. The release page has the rest.
#[must_use]
pub fn notes_excerpt(notes: &str) -> String {
    const MOST_CHARS: usize = 600;
    const MOST_LINES: usize = 12;
    let notes = notes.trim();
    let mut out = String::new();
    for (index, line) in notes.lines().enumerate() {
        if index >= MOST_LINES || out.chars().count() + line.chars().count() > MOST_CHARS {
            if out.is_empty() {
                out = line.chars().take(MOST_CHARS).collect();
            }
            out.push_str("\n…");
            return out;
        }
        if index > 0 {
            out.push('\n');
        }
        out.push_str(line.trim_end());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_failure_has_its_own_sentence() {
        let en = copy::update_for(copy::Locale::English);
        let zh = copy::update_for(copy::Locale::Chinese);
        for strings in [en, zh] {
            let missing = error_text(
                &UpdateError::SignatureMissing,
                UpdateStep::Download,
                strings,
            );
            let no_asset = error_text(
                &UpdateError::NoInstallableAsset,
                UpdateStep::Download,
                strings,
            );
            let invalid = error_text(
                &UpdateError::SignatureInvalid {
                    reason: "bad".to_owned(),
                },
                UpdateStep::Download,
                strings,
            );
            let interrupted = error_text(
                &UpdateError::DownloadInterrupted {
                    reason: "10 of 20 bytes arrived".to_owned(),
                },
                UpdateStep::Download,
                strings,
            );
            let quiet = error_text(&UpdateError::Timeout, UpdateStep::Download, strings);
            let slow_check = error_text(&UpdateError::Timeout, UpdateStep::Check, strings);
            let all = [
                &missing,
                &no_asset,
                &invalid,
                &interrupted,
                &quiet,
                &slow_check,
            ];
            for (index, one) in all.iter().enumerate() {
                for other in &all[index + 1..] {
                    assert_ne!(one, other, "two failures read the same");
                }
            }
            assert!(missing.starts_with(strings.error_signature_missing));
            assert!(invalid.starts_with(strings.error_signature_invalid));
            assert!(interrupted.contains("10 of 20 bytes arrived"));
            assert_eq!(quiet, strings.error_download_timeout);
        }
    }

    #[test]
    fn each_reason_for_the_release_page_has_its_own_sentence() {
        let strings = copy::update_for(copy::Locale::Chinese);
        let reasons = [
            Unavailable::ManualRoute(ManualReason::Platform),
            Unavailable::ManualRoute(ManualReason::DebugBuild),
            Unavailable::ManualRoute(ManualReason::NoPermission),
            Unavailable::NoAsset(PlatformKey::WindowsX86_64Portable),
            Unavailable::SignatureMissing(PlatformKey::WindowsX86_64Portable),
        ];
        let texts: Vec<String> = reasons
            .iter()
            .map(|reason| unavailable_text(*reason, strings))
            .collect();
        for (index, one) in texts.iter().enumerate() {
            assert!(!one.contains('{'), "{one}");
            for other in &texts[index + 1..] {
                assert_ne!(one, other);
            }
        }
        assert!(texts[3].contains(strings.platform_windows_portable));
    }

    #[test]
    fn progress_reads_as_a_share_and_a_size() {
        let strings = copy::update_for(copy::Locale::English);
        let text = progress_text(512 * 1024, Some(1024 * 1024), strings);
        assert!(text.contains("50%"), "{text}");
        assert!(text.contains("512 KB") && text.contains("1.0 MB"), "{text}");
        assert_eq!(progress_fraction(5, Some(10)), Some(0.5));
        assert_eq!(progress_fraction(5, None), None);
        assert!(progress_text(2048, None, strings).contains("2 KB"));
    }

    #[test]
    fn long_notes_are_cut_at_a_line() {
        let notes: String = (0..40).map(|line| format!("- change {line}\n")).collect();
        let excerpt = notes_excerpt(&notes);
        assert!(excerpt.ends_with('…'));
        assert!(excerpt.lines().count() <= 13);
        assert_eq!(notes_excerpt("  one\ntwo  "), "one\ntwo");
    }

    #[test]
    fn active_work_lists_only_what_runs() {
        let strings = copy::update_for(copy::Locale::English);
        let work = ActiveWork {
            terminals: 2,
            transfers: 0,
            desktops: 0,
            serials: 1,
        };
        assert!(!work.is_empty());
        let lines = work.lines(strings);
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].contains('2') && lines[1].contains('1'),
            "{lines:?}"
        );
        assert!(ActiveWork::default().is_empty());
    }
}
