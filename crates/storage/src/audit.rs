//! The closed set of keys the store writes, and the closed set of fields it logs.
//!
//! Two allowlists stand between a future change and a leaked secret:
//!
//! * `CONFIG_KEYS` lists every key the persisted document may contain. A test
//!   walks the serialized JSON and fails on any key outside it, so adding a
//!   password field has to be a deliberate edit to this list as well.
//! * `LOG_FIELDS` lists every field a `StoreEvent` line may contain, with the same
//!   test over the rendered lines. The store logs counts, paths and reasons —
//!   never the contents of a host record.
//!
//! Both lists are also checked for names that mean "secret", so the allowlist
//! itself cannot be the way a credential gets written.

use std::path::PathBuf;

/// Every key the persisted document may contain, in one flat list.
///
/// The list is flat rather than nested because the audit walks the whole document:
/// a key found at any depth must be one of these.
pub const CONFIG_KEYS: &[&str] = &[
    "version",
    "hosts",
    "recents",
    "id",
    "name",
    "address",
    "port",
    "user",
    "group",
    // A path, never key material: an imported `IdentityFile` is remembered so the
    // next connection can offer it, and the key itself stays on disk where the
    // user put it.
    "identity_file",
    "host",
    "last_connected_unix",
    "connections",
    // Jump targets are host ids. The command is the ProxyCommand text, not a
    // secret and not something the store executes.
    "jumps",
    "proxy_command",
    // Forwarding rules: endpoints and the host that would carry the tunnel.
    "rules",
    "kind",
    "listen",
    "target",
    // Workspace snapshot: tabs, pane tree and panel arrangement.
    "tabs",
    "active_tab",
    "panel",
    "title",
    // A tab's own name and distinguishing colour: what the user called it, not a
    // secret. Both are optional, so a file written before they existed still loads.
    "custom_title",
    "accent",
    "root",
    "active_pane",
    "node",
    "pane",
    "axis",
    "ratio",
    "first",
    "second",
    "mode",
    "shell",
    "visible",
    "section",
];

/// Every field a log line may contain.
pub const LOG_FIELDS: &[&str] = &[
    "storage.loaded",
    "storage.saved",
    "storage.load_failed",
    "storage.save_failed",
    "path",
    "hosts",
    "groups",
    "recents",
    "reason",
];

/// One thing the store did, in the shape it is logged in.
///
/// A value rather than a formatted string so a caller can decide where it goes and
/// a test can inspect the fields without parsing free text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreEvent {
    /// A file was read, or did not exist yet.
    Loaded {
        /// The file that was read.
        path: PathBuf,
        /// How many hosts it contained.
        hosts: usize,
    },
    /// The records were written.
    Saved {
        /// The file that was written.
        path: PathBuf,
        /// How many hosts were written.
        hosts: usize,
        /// How many distinct groups they use.
        groups: usize,
        /// How many recent connections were written.
        recents: usize,
    },
    /// The file could not be read or understood.
    LoadFailed {
        /// The file that failed.
        path: PathBuf,
        /// What the layer reported.
        reason: String,
    },
    /// The records could not be written.
    SaveFailed {
        /// The file that failed.
        path: PathBuf,
        /// What the operating system reported.
        reason: String,
    },
}

impl StoreEvent {
    /// The field names this event logs, in order.
    #[must_use]
    pub fn field_names(&self) -> &'static [&'static str] {
        match self {
            Self::Loaded { .. } => &["storage.loaded", "path", "hosts"],
            Self::Saved { .. } => &["storage.saved", "path", "hosts", "groups", "recents"],
            Self::LoadFailed { .. } => &["storage.load_failed", "path", "reason"],
            Self::SaveFailed { .. } => &["storage.save_failed", "path", "reason"],
        }
    }

    /// The event name, which is the first token of the line.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Loaded { .. } => "storage.loaded",
            Self::Saved { .. } => "storage.saved",
            Self::LoadFailed { .. } => "storage.load_failed",
            Self::SaveFailed { .. } => "storage.save_failed",
        }
    }

    /// The line to log.
    ///
    /// Values are quoted so a path with a space stays one field; nothing else is
    /// escaped, because nothing else is logged.
    #[must_use]
    pub fn line(&self) -> String {
        let mut line = self.name().to_owned();
        match self {
            Self::Loaded { path, hosts } => {
                line.push_str(&format!(" path=\"{}\" hosts={hosts}", path.display()));
            }
            Self::Saved {
                path,
                hosts,
                groups,
                recents,
            } => {
                line.push_str(&format!(
                    " path=\"{}\" hosts={hosts} groups={groups} recents={recents}",
                    path.display()
                ));
            }
            Self::LoadFailed { path, reason } | Self::SaveFailed { path, reason } => {
                line.push_str(&format!(" path=\"{}\" reason=\"{reason}\"", path.display()));
            }
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> PathBuf {
        PathBuf::from("C:/Users/example/AppData/Roaming/Shellune/hosts.json")
    }

    /// Splits a line into its `key=value` fields, honouring double quotes.
    ///
    /// The first word is the event name and has no value.
    fn fields(line: &str) -> Vec<(&str, String)> {
        let mut out = Vec::new();
        let (event, rest) = line.trim().split_once(' ').expect("a line has fields");
        out.push((event, String::new()));
        let mut rest = rest;
        while !rest.is_empty() {
            let (key, tail) = rest
                .split_once('=')
                .expect("every field after the event name has a value");
            let key = key.trim();
            let (value, tail) = if let Some(tail) = tail.strip_prefix('"') {
                let (value, tail) = tail.split_once('"').expect("a quoted value is closed");
                (value.to_owned(), tail)
            } else {
                match tail.split_once(' ') {
                    Some((value, tail)) => (value.to_owned(), tail),
                    None => (tail.to_owned(), ""),
                }
            };
            out.push((key, value));
            rest = tail.trim_start();
        }
        out
    }

    #[test]
    fn every_logged_field_is_on_the_allowlist() {
        let events = [
            StoreEvent::Loaded {
                path: path(),
                hosts: 2,
            },
            StoreEvent::Saved {
                path: path(),
                hosts: 2,
                groups: 1,
                recents: 0,
            },
            StoreEvent::LoadFailed {
                path: path(),
                reason: "the file is not valid JSON".to_owned(),
            },
            StoreEvent::SaveFailed {
                path: path(),
                reason: "access is denied".to_owned(),
            },
        ];

        for event in events {
            let line = event.line();
            let parsed = fields(&line);
            assert_eq!(
                parsed.first().map(|(key, _)| *key),
                Some(event.name()),
                "the line must start with its event name: {line}"
            );
            for (key, _) in &parsed {
                assert!(
                    LOG_FIELDS.contains(key),
                    "the log field {key:?} is not on the allowlist: {line}"
                );
            }
            assert_eq!(
                parsed.len(),
                event.field_names().len(),
                "the line and the declared field list disagree: {line}"
            );
            for name in event.field_names() {
                assert!(
                    LOG_FIELDS.contains(name),
                    "the declared field {name:?} is not on the allowlist"
                );
            }
        }
    }

    #[test]
    fn no_logged_field_name_means_secret() {
        for name in LOG_FIELDS {
            let lower = name.to_ascii_lowercase();
            for forbidden in [
                "password",
                "passphrase",
                "secret",
                "token",
                "credential",
                "key",
            ] {
                assert!(
                    !lower.contains(forbidden),
                    "the log field {name:?} looks like a secret"
                );
            }
        }
    }

    #[test]
    fn no_persisted_key_means_secret() {
        for name in CONFIG_KEYS {
            let lower = name.to_ascii_lowercase();
            for forbidden in [
                "password",
                "passphrase",
                "secret",
                "token",
                "credential",
                "private",
            ] {
                assert!(
                    !lower.contains(forbidden),
                    "the persisted key {name:?} looks like a secret"
                );
            }
        }
    }
}
