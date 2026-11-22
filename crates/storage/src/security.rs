//! The device-local lock policy: when the vault asks the user to prove
//! themselves again, and whether this machine unlocks with the system's user
//! verification (R3, R4).
//!
//! These are choices about this computer, not about the user's data. They are
//! saved in `security.json` in the data directory and never travel: a backup,
//! a sync document and a portable export all leave them alone, so another
//! machine keeps its own policy and its own biometric setting.
//!
//! Reading follows the preferences contract: a missing file is nothing saved
//! yet (the documented defaults apply), and a damaged, newer or unusable file
//! yields the defaults together with a report, so the user can be told their
//! choices were not applied instead of believing the defaults are what they
//! chose. Saving is atomic; a failed write leaves the previous file intact.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::PersistError;
use crate::store::save_versioned;

/// Name of the lock policy file inside the data directory.
pub const FILE_NAME: &str = "security.json";

const LABEL: &str = "security";

/// The idle lock that applies when the user has not chosen one.
pub const DEFAULT_IDLE_LOCK_MINUTES: u16 = 15;

/// The longest idle lock this build accepts: a day.
const MAX_IDLE_LOCK_MINUTES: u16 = 1440;

/// When the vault asks the user to prove themselves again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LockPolicy {
    /// No automatic re-locking: the vault stays as the session left it.
    Disabled,
    /// Verify once to unlock (the default): after startup, a manual lock, a
    /// system sleep or the idle time passing, one verification opens the
    /// vault for reading passwords and signing with managed keys.
    #[default]
    UnlockVault,
    /// Every sensitive operation — showing or copying a password, a managed
    /// key authentication, an export — verifies again; one success is never
    /// the next operation's pass.
    EverySensitiveOperation,
}

impl LockPolicy {
    /// Whether this policy re-locks at all.
    #[must_use]
    pub const fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }
}

/// The lock settings of this one machine.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecuritySettings {
    /// When the vault re-locks. Absent means the default, one verification
    /// per unlock.
    pub policy: LockPolicy,
    /// How long the vault may sit unused before it locks again, in minutes.
    /// Absent means the documented default, not "never"; this build accepts
    /// 1 to 1440 (a day).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_lock_minutes: Option<u16>,
    /// Whether the user turned biometric unlocking on for this machine.
    /// Absent means off. The setting lives here, on this machine, so another
    /// device's sync never switches this one's protection on or off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub biometric_unlock: Option<bool>,
}

impl SecuritySettings {
    /// Settings for one policy, everything else at its default.
    #[must_use]
    pub const fn new(policy: LockPolicy) -> Self {
        Self {
            policy,
            idle_lock_minutes: None,
            biometric_unlock: None,
        }
    }

    /// The idle time after which the vault locks again.
    #[must_use]
    pub fn idle_lock(&self) -> Duration {
        Duration::from_secs(
            u64::from(self.idle_lock_minutes.unwrap_or(DEFAULT_IDLE_LOCK_MINUTES)) * 60,
        )
    }

    /// Whether every field holds a value this build can honour as saved. A
    /// value it cannot is reported rather than clamped into something the
    /// user did not choose.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.idle_lock_minutes
            .is_none_or(|minutes| (1..=MAX_IDLE_LOCK_MINUTES).contains(&minutes))
    }
}

/// Why a lock policy file could not be used as it stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecurityProblem {
    /// The file exists but is not the document this build writes. The
    /// defaults are in use and the next change replaces the file.
    Unreadable,
}

/// A lock policy file at a known location.
#[derive(Clone, Debug)]
pub struct SecurityStore {
    root: PathBuf,
}

impl SecurityStore {
    /// A store inside the given directory.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The lock policy file.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root.join(FILE_NAME)
    }

    /// The directory the file lives in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Reads the settings. Anything unreadable gives the defaults.
    #[must_use]
    pub fn load(&self) -> SecuritySettings {
        self.load_reported().0
    }

    /// Reads the settings and says whether the saved document could be used.
    ///
    /// A missing file is not a problem: it is simply nothing saved yet. A
    /// file that cannot be used as saved — damaged, carrying a policy this
    /// build does not know, or an idle time it cannot honour — is reported
    /// with the defaults, so the caller can tell the user their lock choices
    /// were not applied.
    #[must_use]
    pub fn load_reported(&self) -> (SecuritySettings, Option<SecurityProblem>) {
        match std::fs::read(self.path()) {
            Err(_) => (SecuritySettings::default(), None),
            Ok(bytes) => match serde_json::from_slice::<SecuritySettings>(&bytes) {
                Ok(settings) if settings.is_usable() => (settings, None),
                Ok(_) | Err(_) => (
                    SecuritySettings::default(),
                    Some(SecurityProblem::Unreadable),
                ),
            },
        }
    }

    /// Writes the settings atomically.
    ///
    /// # Errors
    ///
    /// Returns a directory, write, replace or encode failure. The previous
    /// file stays in place.
    pub fn save(&self, settings: &SecuritySettings) -> Result<(), PersistError> {
        let bytes = serde_json::to_vec_pretty(settings).map_err(|error| PersistError::Encode {
            label: LABEL,
            reason: error.to_string(),
        })?;
        save_versioned(&self.root, FILE_NAME, LABEL, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("shellune-security-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_missing_file_gives_the_documented_defaults() {
        let store = SecurityStore::at(scratch("missing"));
        assert_eq!(store.load(), SecuritySettings::default());
        assert_eq!(store.load().policy, LockPolicy::UnlockVault);
        assert_eq!(
            store.load().idle_lock(),
            Duration::from_secs(u64::from(DEFAULT_IDLE_LOCK_MINUTES) * 60)
        );
        assert!(!store.load().policy.is_disabled());
    }

    #[test]
    fn saved_lock_settings_round_trip() {
        let dir = scratch("round-trip");
        let store = SecurityStore::at(&dir);
        let settings = SecuritySettings {
            policy: LockPolicy::EverySensitiveOperation,
            idle_lock_minutes: Some(5),
            biometric_unlock: Some(true),
        };
        store.save(&settings).expect("saves");
        assert_eq!(store.load(), settings);
        assert_eq!(store.load().idle_lock(), Duration::from_secs(5 * 60));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_absent_field_keeps_its_default_and_the_file_says_nothing() {
        let dir = scratch("sparse");
        std::fs::create_dir_all(&dir).expect("dir");
        let store = SecurityStore::at(&dir);
        std::fs::write(store.path(), br#"{"policy":"disabled"}"#).expect("write");
        let settings = store.load();
        assert!(settings.policy.is_disabled());
        assert_eq!(settings.idle_lock_minutes, None);
        assert_eq!(settings.biometric_unlock, None);
        assert_eq!(
            settings.idle_lock(),
            Duration::from_secs(u64::from(DEFAULT_IDLE_LOCK_MINUTES) * 60)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_or_unusable_file_is_reported_rather_than_guessed_at() {
        let dir = scratch("damaged");
        std::fs::create_dir_all(&dir).expect("dir");
        let store = SecurityStore::at(&dir);
        for text in [
            // Not JSON at all.
            b"{not json".as_slice(),
            // A policy this build does not know is not "probably the default".
            br#"{"policy":"on-tuesdays"}"#,
            // An idle time this build cannot honour is not clamped.
            br#"{"idle_lock_minutes":0}"#,
            br#"{"idle_lock_minutes":100000}"#,
            br#"{"idle_lock_minutes":"soon"}"#,
        ] {
            std::fs::write(store.path(), text).expect("write");
            let (settings, problem) = store.load_reported();
            assert_eq!(
                settings,
                SecuritySettings::default(),
                "the file is not half applied: {}",
                String::from_utf8_lossy(text)
            );
            assert_eq!(
                problem,
                Some(SecurityProblem::Unreadable),
                "the user is told their lock choices were not applied: {}",
                String::from_utf8_lossy(text)
            );
        }

        // A field this build does not know is not damage.
        std::fs::write(store.path(), br#"{"policy":"disabled","future":1}"#).expect("write");
        let (settings, problem) = store.load_reported();
        assert_eq!(problem, None);
        assert!(settings.policy.is_disabled());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_saved_document_names_the_three_policies_stably() {
        for (policy, text) in [
            (LockPolicy::Disabled, "disabled"),
            (LockPolicy::UnlockVault, "unlock-vault"),
            (
                LockPolicy::EverySensitiveOperation,
                "every-sensitive-operation",
            ),
        ] {
            let json = serde_json::to_string(&SecuritySettings::new(policy)).expect("encodes");
            assert!(json.contains(&format!("\"{text}\"")), "{json}");
            let back: SecuritySettings = serde_json::from_str(&json).expect("decodes");
            assert_eq!(back.policy, policy);
        }
    }
}
