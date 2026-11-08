//! The interface preferences: theme, language, text size, motion, sidebar and
//! terminal font size.
//!
//! These are conveniences, not records. A missing, damaged or newer file yields
//! the defaults instead of an error, and the next change writes a fresh file, so a
//! bad preferences file can never keep the application from starting. Every field
//! is optional: a value this build does not know is ignored and the default used.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::PersistError;
use crate::store::save_versioned;

/// Name of the preferences file inside the data directory.
pub const FILE_NAME: &str = "preferences.json";

const LABEL: &str = "preferences";

/// The saved interface preferences, as plain values the application maps back to
/// its own types.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// `system`, `light` or `dark`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// `en` or `zh`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Interface text size in percent: 90, 100, 115 or 200.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_size: Option<u16>,
    /// `full` or `reduced`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motion: Option<String>,
    /// Whether the user collapsed the sidebar to its icons.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar_collapsed: Option<bool>,
    /// Terminal font size in points.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_font_size: Option<f32>,
}

/// A preferences file at a known location.
#[derive(Clone, Debug)]
pub struct PreferencesStore {
    root: PathBuf,
}

impl PreferencesStore {
    /// A store inside the given directory.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The preferences file.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root.join(FILE_NAME)
    }

    /// The directory the file lives in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Reads the preferences. Anything unreadable gives the defaults.
    #[must_use]
    pub fn load(&self) -> Preferences {
        std::fs::read(self.path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Writes the preferences atomically.
    ///
    /// # Errors
    ///
    /// Returns a directory, write, replace or encode failure. The previous file
    /// stays in place.
    pub fn save(&self, preferences: &Preferences) -> Result<(), PersistError> {
        let bytes =
            serde_json::to_vec_pretty(preferences).map_err(|error| PersistError::Encode {
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
        let dir = std::env::temp_dir().join(format!(
            "shellune-preferences-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_missing_file_gives_defaults() {
        let store = PreferencesStore::at(scratch("missing"));
        assert_eq!(store.load(), Preferences::default());
    }

    #[test]
    fn saved_preferences_load_back() {
        let dir = scratch("round-trip");
        let store = PreferencesStore::at(&dir);
        let preferences = Preferences {
            theme: Some("dark".to_owned()),
            language: Some("zh".to_owned()),
            text_size: Some(115),
            motion: Some("reduced".to_owned()),
            sidebar_collapsed: Some(true),
            terminal_font_size: Some(15.0),
        };
        store.save(&preferences).expect("saves");
        assert_eq!(store.load(), preferences);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_file_gives_defaults_and_unknown_fields_are_ignored() {
        let dir = scratch("damaged");
        std::fs::create_dir_all(&dir).expect("dir");
        let store = PreferencesStore::at(&dir);
        std::fs::write(store.path(), b"{not json").expect("write");
        assert_eq!(store.load(), Preferences::default());
        std::fs::write(store.path(), br#"{"theme":"dark","future":1}"#).expect("write");
        assert_eq!(store.load().theme.as_deref(), Some("dark"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
