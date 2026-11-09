//! The interface preferences: theme, language, text size, motion, sidebar,
//! terminal font size and the custom key bindings.
//!
//! These are conveniences, not records. A missing, damaged or newer file yields
//! the defaults instead of an error, and the next change writes a fresh file, so a
//! bad preferences file can never keep the application from starting. Every field
//! is optional: a value this build does not know is ignored and the default used.
//!
//! Because a damaged file is silently replaced by defaults, the reader also has a
//! reporting form ([`PreferencesStore::load_reported`]): the shell shows that the
//! saved choices were not applied instead of pretending they were read.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::PersistError;
use crate::store::save_versioned;

/// Name of the preferences file inside the data directory.
pub const FILE_NAME: &str = "preferences.json";

const LABEL: &str = "preferences";

/// One saved key binding: which action, in which input context, on which platform.
///
/// The action identifier and the context are written down next to the keystroke so
/// that a record this build does not know - a renamed action, a context that moved -
/// can be recognised and reported instead of being applied to the wrong thing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeymapEntry {
    /// Catalogue identifier of the action, such as `palette.open`.
    pub action: String,
    /// Input context the binding applies in, such as `shell`.
    pub context: String,
    /// The keystroke in the GPUI keymap syntax; absent means the action is unbound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keystroke: Option<String>,
}

impl KeymapEntry {
    /// A binding record for one action.
    #[must_use]
    pub fn new(
        action: impl Into<String>,
        context: impl Into<String>,
        keystroke: Option<String>,
    ) -> Self {
        Self {
            action: action.into(),
            context: context.into(),
            keystroke,
        }
    }
}

/// The user's own key bindings, one list per platform.
///
/// Bindings are per platform on purpose: Ctrl and Cmd are different keys, and a
/// combination that is free on Windows may be the system's on macOS. An action that
/// is absent from a list keeps its default; an action present with no keystroke is
/// unbound, which is how a shortcut is handed back to the terminal program.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeymapPreferences {
    /// Bindings made on Windows.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<KeymapEntry>,
    /// Bindings made on macOS.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub macos: Vec<KeymapEntry>,
    /// Bindings made on Linux.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub linux: Vec<KeymapEntry>,
}

impl KeymapPreferences {
    /// The bindings saved for one platform, by [`KeymapPreferences::set_for_platform`] name.
    #[must_use]
    pub fn for_platform(&self, platform: &str) -> &[KeymapEntry] {
        match platform {
            "macos" => &self.macos,
            "linux" => &self.linux,
            _ => &self.windows,
        }
    }

    /// Replaces the bindings saved for one platform.
    pub fn set_for_platform(&mut self, platform: &str, entries: Vec<KeymapEntry>) {
        match platform {
            "macos" => self.macos = entries,
            "linux" => self.linux = entries,
            _ => self.windows = entries,
        }
    }

    /// Whether no binding was ever changed on any platform.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty() && self.macos.is_empty() && self.linux.is_empty()
    }
}

/// Why a preferences file could not be used as it stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreferencesProblem {
    /// The file exists but is not the document this build writes. The defaults are
    /// in use and the next change replaces the file.
    Unreadable,
}

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
    /// The terminal's palette: `app`, `dark` or `light`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_scheme: Option<String>,
    /// The shortcuts the user changed, per platform. Absent means every action
    /// uses the catalogue's default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keymap: Option<KeymapPreferences>,
    /// Whether a bell in a background tab marks it (R7). Absent means on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bell_notice: Option<bool>,
    /// Whether a command end the shell reported in a background tab marks it (R7).
    /// Absent means on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_notice: Option<bool>,
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
        self.load_reported().0
    }

    /// Reads the preferences and says whether the saved document could be used.
    ///
    /// A missing file is not a problem: it is simply nothing saved yet. A file that
    /// cannot be parsed is reported with the defaults, so the caller can tell the
    /// user that their choices were not applied rather than let them believe the
    /// defaults are what they chose.
    #[must_use]
    pub fn load_reported(&self) -> (Preferences, Option<PreferencesProblem>) {
        match std::fs::read(self.path()) {
            Err(_) => (Preferences::default(), None),
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(preferences) => (preferences, None),
                Err(_) => (Preferences::default(), Some(PreferencesProblem::Unreadable)),
            },
        }
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
            terminal_scheme: Some("dark".to_owned()),
            keymap: Some(KeymapPreferences {
                windows: vec![
                    KeymapEntry::new("palette.open", "shell", Some("ctrl-alt-p".to_owned())),
                    KeymapEntry::new("terminal.copy", "terminal", None),
                ],
                macos: Vec::new(),
                linux: Vec::new(),
            }),
            bell_notice: Some(false),
            command_notice: Some(true),
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
        let (damaged, problem) = store.load_reported();
        assert_eq!(damaged, Preferences::default());
        assert_eq!(problem, Some(PreferencesProblem::Unreadable));
        std::fs::write(store.path(), br#"{"theme":"dark","future":1}"#).expect("write");
        let (unknown, problem) = store.load_reported();
        assert_eq!(unknown.theme.as_deref(), Some("dark"));
        assert_eq!(
            problem, None,
            "a field this build does not know is not damage"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_file_is_not_reported_as_a_problem() {
        let store = PreferencesStore::at(scratch("absent"));
        let (preferences, problem) = store.load_reported();
        assert_eq!(preferences, Preferences::default());
        assert_eq!(problem, None);
    }

    #[test]
    fn a_keymap_of_the_wrong_shape_is_reported_rather_than_guessed_at() {
        let dir = scratch("keymap-damaged");
        std::fs::create_dir_all(&dir).expect("dir");
        let store = PreferencesStore::at(&dir);
        std::fs::write(
            store.path(),
            br#"{"theme":"light","keymap":{"windows":[{"action":7}]}}"#,
        )
        .expect("write");
        let (preferences, problem) = store.load_reported();
        assert_eq!(problem, Some(PreferencesProblem::Unreadable));
        assert_eq!(preferences, Preferences::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn keymaps_are_kept_per_platform() {
        let mut keymap = KeymapPreferences::default();
        assert!(keymap.is_empty());
        keymap.set_for_platform(
            "macos",
            vec![KeymapEntry::new(
                "palette.open",
                "shell",
                Some("cmd-shift-k".to_owned()),
            )],
        );
        keymap.set_for_platform("linux", Vec::new());
        assert!(!keymap.is_empty());
        assert_eq!(keymap.for_platform("macos").len(), 1);
        assert_eq!(
            keymap.for_platform("macos")[0].keystroke.as_deref(),
            Some("cmd-shift-k")
        );
        assert!(keymap.for_platform("linux").is_empty());
        assert!(
            keymap.for_platform("windows").is_empty(),
            "another platform's bindings are not leaked"
        );
        keymap.set_for_platform("macos", Vec::new());
        assert!(keymap.is_empty());
    }
}
