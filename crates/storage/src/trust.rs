//! The host trust library: which server identities the user accepted (R6).
//!
//! The file records public material only — the key algorithm, the OpenSSH style
//! SHA-256 fingerprint and the base64 public key — because a trust decision is
//! about an identity, never about a credential. It is kept apart from the host
//! records so forgetting a server identity is a separate, deliberate act.
//!
//! Two rules decide the shape of this module:
//!
//! * an unknown identity is stored only when the user explicitly accepts it, and
//! * a stored identity is never replaced by a different one. [HostTrust::accept]
//!   refuses the change instead of overwriting, so a server that starts presenting
//!   another key cannot inherit a trust decision the user made for the old one.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::store::write_atomic_file;

/// Name of the trust file inside the data directory.
pub const TRUST_FILE_NAME: &str = "known_hosts.json";

/// The format this build writes.
pub const TRUST_FORMAT_VERSION: u32 = 1;

/// Prefix every fingerprint carries, so a value from another hash is not mistaken
/// for one this file compares.
const FINGERPRINT_PREFIX: &str = "SHA256:";

/// Length of the base64 body of a SHA-256 fingerprint (32 bytes, unpadded).
const FINGERPRINT_BODY_LEN: usize = 43;

/// One accepted server identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustEntry {
    host: String,
    port: u16,
    algorithm: String,
    fingerprint: String,
    public_key: String,
    accepted_unix: u64,
}

impl TrustEntry {
    /// The host name or address the identity belongs to.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port the identity belongs to.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Key algorithm, as the server named it (for example ssh-ed25519).
    #[must_use]
    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }

    /// The SHA256 fingerprint of the accepted key.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Base64 encoding of the accepted public key.
    #[must_use]
    pub fn public_key(&self) -> &str {
        &self.public_key
    }

    /// When the user accepted this identity, in seconds since the Unix epoch.
    #[must_use]
    pub fn accepted_unix(&self) -> u64 {
        self.accepted_unix
    }
}

/// A server identity that was just presented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostKey {
    /// Algorithm the server named.
    pub algorithm: String,
    /// The SHA256 fingerprint this build computed from the key.
    pub fingerprint: String,
    /// Base64 of the key's wire encoding, kept so a trust file stays verifiable.
    pub public_key: String,
}

impl HostKey {
    /// Builds a presented identity, refusing values a fingerprint cannot have.
    ///
    /// # Errors
    ///
    /// Returns the reason when the algorithm is empty or the fingerprint is not a
    /// SHA-256 fingerprint this build produced, so a malformed value can never be
    /// stored as if it were verifiable.
    pub fn new(
        algorithm: impl Into<String>,
        fingerprint: impl Into<String>,
        public_key: impl Into<String>,
    ) -> Result<Self, TrustError> {
        let algorithm = algorithm.into();
        let fingerprint = fingerprint.into();
        let public_key = public_key.into();
        if algorithm.trim().is_empty() {
            return Err(TrustError::Invalid {
                reason: "the key algorithm is empty".to_owned(),
            });
        }
        if !is_sha256_fingerprint(&fingerprint) {
            return Err(TrustError::Invalid {
                reason: format!("'{fingerprint}' is not a SHA256 fingerprint"),
            });
        }
        if public_key.trim().is_empty() {
            return Err(TrustError::Invalid {
                reason: "the public key is empty".to_owned(),
            });
        }
        Ok(Self {
            algorithm,
            fingerprint,
            public_key,
        })
    }
}

/// Whether a string is a fingerprint this build writes.
#[must_use]
pub fn is_sha256_fingerprint(value: &str) -> bool {
    let Some(body) = value.strip_prefix(FINGERPRINT_PREFIX) else {
        return false;
    };
    body.len() == FINGERPRINT_BODY_LEN
        && body.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '+' || character == '/'
        })
}

/// What the stored records say about a presented identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustVerdict {
    /// The same identity was accepted before.
    Trusted,
    /// Nothing is stored for this host and port.
    Unknown,
    /// Another identity is stored, and it is not the one that answered.
    Changed {
        /// Algorithm of the identity that was accepted earlier.
        algorithm: String,
        /// Fingerprint of the identity that was accepted earlier.
        fingerprint: String,
    },
}

impl TrustVerdict {
    /// Whether a connection may go ahead without asking the user.
    #[must_use]
    pub fn is_trusted(&self) -> bool {
        matches!(self, Self::Trusted)
    }
}

/// Why an identity could not be stored.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TrustError {
    /// A different identity is already trusted for this host and port.
    #[error(
        "a different server identity is already trusted for this host: {stored} was accepted, {presented} was presented"
    )]
    IdentityChanged {
        /// Fingerprint that is already trusted.
        stored: String,
        /// Fingerprint that was presented.
        presented: String,
    },
    /// The value is not something that can be trusted.
    #[error("the server identity cannot be trusted: {reason}")]
    Invalid {
        /// What is wrong with it.
        reason: String,
    },
}

/// The accepted server identities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostTrust {
    version: u32,
    entries: Vec<TrustEntry>,
}

impl Default for HostTrust {
    fn default() -> Self {
        Self {
            version: TRUST_FORMAT_VERSION,
            entries: Vec::new(),
        }
    }
}

impl HostTrust {
    /// An empty trust library.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The format version the records were read from.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Every accepted identity, in the order it was accepted.
    #[must_use]
    pub fn entries(&self) -> &[TrustEntry] {
        &self.entries
    }

    /// Whether nothing has been accepted yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The record for one host and port, when there is one.
    ///
    /// The pair is the identity: two hosts on one machine are two trust decisions,
    /// which is why a record cannot be found by name alone.
    #[must_use]
    pub fn entry(&self, host: &str, port: u16) -> Option<&TrustEntry> {
        let host = normalize_host(host);
        self.entries
            .iter()
            .find(|entry| entry.host == host && entry.port == port)
    }

    /// What the records say about a presented identity.
    #[must_use]
    pub fn verdict(&self, host: &str, port: u16, key: &HostKey) -> TrustVerdict {
        match self.entry(host, port) {
            None => TrustVerdict::Unknown,
            Some(entry) if entry.fingerprint == key.fingerprint => TrustVerdict::Trusted,
            Some(entry) => TrustVerdict::Changed {
                algorithm: entry.algorithm.clone(),
                fingerprint: entry.fingerprint.clone(),
            },
        }
    }

    /// Stores an identity the user accepted.
    ///
    /// Accepting the same identity twice is not an error — the first decision stands
    /// and its timestamp is kept — while accepting a different identity for a host
    /// that already has one is refused: replacing a trust record is a separate act
    /// that this build does not offer, and silently doing it here would defeat the
    /// point of having recorded the old one.
    ///
    /// # Errors
    ///
    /// Returns [TrustError::IdentityChanged] when another identity is stored for the
    /// same host and port, and [TrustError::Invalid] for a host that cannot be an
    /// identity of its own.
    pub fn accept(
        &mut self,
        host: &str,
        port: u16,
        key: &HostKey,
        at_unix: u64,
    ) -> Result<(), TrustError> {
        let host = normalize_host(host);
        if host.is_empty() {
            return Err(TrustError::Invalid {
                reason: "the host name is empty".to_owned(),
            });
        }
        if port == 0 {
            return Err(TrustError::Invalid {
                reason: "port 0 is not a usable SSH port".to_owned(),
            });
        }
        if !is_sha256_fingerprint(&key.fingerprint) {
            return Err(TrustError::Invalid {
                reason: format!("'{}' is not a SHA256 fingerprint", key.fingerprint),
            });
        }

        match self
            .entries
            .iter()
            .position(|entry| entry.host == host && entry.port == port)
        {
            Some(index) => {
                let stored = &self.entries[index];
                if stored.fingerprint == key.fingerprint {
                    return Ok(());
                }
                Err(TrustError::IdentityChanged {
                    stored: stored.fingerprint.clone(),
                    presented: key.fingerprint.clone(),
                })
            }
            None => {
                self.entries.push(TrustEntry {
                    host,
                    port,
                    algorithm: key.algorithm.clone(),
                    fingerprint: key.fingerprint.clone(),
                    public_key: key.public_key.clone(),
                    accepted_unix: at_unix,
                });
                Ok(())
            }
        }
    }

    /// Removes the record for one host and port, returning whether there was one.
    ///
    /// Forgetting is the only way a changed identity can be trusted again, and it
    /// discards the old fingerprint, so the next connection asks the user afresh.
    pub fn forget(&mut self, host: &str, port: u16) -> bool {
        let host = normalize_host(host);
        let before = self.entries.len();
        self.entries
            .retain(|entry| !(entry.host == host && entry.port == port));
        self.entries.len() != before
    }

    /// Checks the invariants a stored document must satisfy.
    ///
    /// # Errors
    ///
    /// Returns the reason a document cannot be used: an empty host, port 0, a
    /// fingerprint that is not SHA-256, or two records for one identity.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != TRUST_FORMAT_VERSION {
            return Err(format!(
                "format version {} is not the version this build writes",
                self.version
            ));
        }
        for entry in &self.entries {
            if entry.host != normalize_host(&entry.host) || entry.host.is_empty() {
                return Err(format!(
                    "the trust record for '{}' does not use a normalized host",
                    entry.host
                ));
            }
            if entry.port == 0 {
                return Err(format!("the trust record for {} has port 0", entry.host));
            }
            if !is_sha256_fingerprint(&entry.fingerprint) {
                return Err(format!(
                    "the trust record for {} does not hold a SHA256 fingerprint",
                    entry.host
                ));
            }
            if entry.algorithm.trim().is_empty() || entry.public_key.trim().is_empty() {
                return Err(format!(
                    "the trust record for {} is missing its key material",
                    entry.host
                ));
            }
            let duplicates = self
                .entries
                .iter()
                .filter(|other| other.host == entry.host && other.port == entry.port)
                .count();
            if duplicates > 1 {
                return Err(format!(
                    "the host {} and port {} have two trust records",
                    entry.host, entry.port
                ));
            }
        }
        Ok(())
    }
}

/// Lower-cases a host so example.COM and example.com are one identity.
fn normalize_host(host: &str) -> String {
    host.trim().to_lowercase()
}

/// A trust file at a known location.
#[derive(Clone, Debug)]
pub struct TrustStore {
    root: PathBuf,
}

impl TrustStore {
    /// A trust file inside the given directory.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// A trust file beside the host records in the per-user data directory.
    #[must_use]
    pub fn default_location() -> Self {
        Self::at(crate::store::data_directory())
    }

    /// The directory the file lives in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The file itself.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root.join(TRUST_FILE_NAME)
    }

    /// Reads the accepted identities.
    ///
    /// A missing file is the first run and yields an empty library, because an empty
    /// list is a real answer: no identity has been accepted yet.
    ///
    /// # Errors
    ///
    /// Returns the read failure when the file exists but cannot be read, the
    /// corruption when it is not a document this build understands, and the
    /// unsupported-version failure when a newer build wrote it.
    pub fn load(&self) -> Result<HostTrust, StorageError> {
        let path = self.path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(HostTrust::default());
            }
            Err(error) => {
                return Err(StorageError::Read {
                    path,
                    reason: error.to_string(),
                });
            }
        };

        #[derive(Deserialize)]
        struct VersionProbe {
            version: u32,
        }

        let probe: VersionProbe =
            serde_json::from_str(&text).map_err(|error| StorageError::Corrupt {
                path: path.clone(),
                reason: error.to_string(),
            })?;
        if probe.version > TRUST_FORMAT_VERSION {
            return Err(StorageError::UnsupportedVersion {
                path,
                found: probe.version,
                supported: TRUST_FORMAT_VERSION,
            });
        }
        if probe.version == 0 {
            return Err(StorageError::Corrupt {
                path,
                reason: "format version 0 was never written by Shellune".to_owned(),
            });
        }

        let trust: HostTrust =
            serde_json::from_str(&text).map_err(|error| StorageError::Corrupt {
                path: path.clone(),
                reason: error.to_string(),
            })?;
        trust
            .validate()
            .map_err(|reason| StorageError::Corrupt { path, reason })?;
        Ok(trust)
    }

    /// Writes the accepted identities atomically.
    ///
    /// # Errors
    ///
    /// Returns the directory, write or replace failure, keeping the previous file
    /// untouched; a trust decision that could not be written is never reported as
    /// saved.
    pub fn save(&self, trust: &HostTrust) -> Result<(), StorageError> {
        trust.validate().map_err(|reason| StorageError::Corrupt {
            path: self.path(),
            reason,
        })?;
        let bytes = serde_json::to_vec_pretty(trust).map_err(|error| StorageError::Encode {
            reason: error.to_string(),
        })?;
        write_atomic_file(&self.root, TRUST_FILE_NAME, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch_root() -> PathBuf {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let unique = format!(
            "shellune-trust-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        std::env::temp_dir().join(unique)
    }

    fn key(algorithm: &str, byte: u8) -> HostKey {
        let fingerprint = shellune_core::sha256_fingerprint(&[byte; 32]).expect("a digest");
        HostKey::new(algorithm, fingerprint, format!("public-key-{byte}"))
            .expect("a valid identity")
    }

    #[test]
    fn an_unknown_identity_becomes_trusted_only_when_it_is_accepted() {
        let presented = key("ssh-ed25519", 1);
        let mut trust = HostTrust::new();

        assert_eq!(
            trust.verdict("web.example.org", 22, &presented),
            TrustVerdict::Unknown
        );
        assert!(trust.is_empty());

        trust
            .accept("web.example.org", 22, &presented, 1_700_000_000)
            .expect("the user accepted the identity");
        assert_eq!(
            trust.verdict("web.example.org", 22, &presented),
            TrustVerdict::Trusted
        );
        let entry = trust
            .entry("web.example.org", 22)
            .expect("the record is there");
        assert_eq!(entry.algorithm(), "ssh-ed25519");
        assert_eq!(entry.fingerprint(), presented.fingerprint);
        assert_eq!(entry.accepted_unix(), 1_700_000_000);
    }

    #[test]
    fn a_changed_identity_is_reported_and_never_overwrites_the_stored_one() {
        let accepted = key("ssh-ed25519", 1);
        let presented = key("ssh-ed25519", 2);
        let mut trust = HostTrust::new();
        trust
            .accept("web.example.org", 22, &accepted, 1)
            .expect("accepted");

        assert_eq!(
            trust.verdict("web.example.org", 22, &presented),
            TrustVerdict::Changed {
                algorithm: "ssh-ed25519".to_owned(),
                fingerprint: accepted.fingerprint.clone(),
            }
        );
        let error = trust
            .accept("web.example.org", 22, &presented, 2)
            .expect_err("a different identity must not be stored over the accepted one");
        assert!(matches!(error, TrustError::IdentityChanged { .. }));
        assert!(
            error.to_string().contains(&accepted.fingerprint),
            "the reason names the fingerprint that is trusted: {error}"
        );

        let entry = trust.entry("web.example.org", 22).expect("still stored");
        assert_eq!(
            entry.fingerprint(),
            accepted.fingerprint,
            "the old record survives the refused replacement"
        );
        assert_eq!(entry.accepted_unix(), 1, "and keeps its acceptance time");
    }

    #[test]
    fn a_second_host_on_the_same_machine_is_its_own_identity() {
        let first = key("ssh-ed25519", 3);
        let second = key("ssh-ed25519", 4);
        let mut trust = HostTrust::new();
        trust.accept("10.0.0.7", 22, &first, 1).expect("accepted");
        assert_eq!(
            trust.verdict("10.0.0.7", 2222, &second),
            TrustVerdict::Unknown,
            "another port is another trust decision"
        );
        trust
            .accept("10.0.0.7", 2222, &second, 2)
            .expect("accepted");
        assert_eq!(trust.entries().len(), 2);
    }

    #[test]
    fn host_names_are_compared_without_case_or_surrounding_space() {
        let presented = key("ssh-ed25519", 5);
        let mut trust = HostTrust::new();
        trust
            .accept("  Web.Example.ORG  ", 22, &presented, 1)
            .expect("accepted");
        assert_eq!(
            trust.verdict("web.example.org", 22, &presented),
            TrustVerdict::Trusted
        );
        assert!(trust.forget("WEB.EXAMPLE.ORG", 22));
        assert!(!trust.forget("web.example.org", 22));
        assert!(trust.is_empty());
    }

    #[test]
    fn accepting_the_same_identity_twice_keeps_the_first_decision() {
        let presented = key("ssh-ed25519", 6);
        let mut trust = HostTrust::new();
        trust.accept("web", 22, &presented, 10).expect("accepted");
        trust
            .accept("web", 22, &presented, 20)
            .expect("accepting the same identity again is not a conflict");
        let entry = trust.entry("web", 22).expect("stored");
        assert_eq!(entry.accepted_unix(), 10);
        assert_eq!(trust.entries().len(), 1);
    }

    #[test]
    fn an_identity_that_cannot_be_compared_is_refused() {
        assert!(HostKey::new("", "SHA256:abc", "key").is_err());
        assert!(HostKey::new("ssh-ed25519", "MD5:aa:bb", "key").is_err());
        assert!(HostKey::new("ssh-ed25519", "SHA256:short", "key").is_err());
        assert!(HostKey::new("ssh-ed25519", "SHA256:abc", "  ").is_err());

        let mut trust = HostTrust::new();
        let valid = key("ssh-ed25519", 7);
        assert!(matches!(
            trust.accept("  ", 22, &valid, 0),
            Err(TrustError::Invalid { .. })
        ));
        assert!(matches!(
            trust.accept("web", 0, &valid, 0),
            Err(TrustError::Invalid { .. })
        ));
    }

    #[test]
    fn a_saved_file_round_trips_and_holds_no_credential_field() {
        let root = scratch_root();
        let store = TrustStore::at(&root);
        let presented = key("ssh-ed25519", 8);
        let mut trust = HostTrust::new();
        trust
            .accept("web.example.org", 22, &presented, 1_700_000_000)
            .expect("accepted");
        store.save(&trust).expect("the trust file is written");

        let reloaded = store.load().expect("the trust file is read");
        assert_eq!(reloaded, trust);
        assert!(store.path().exists());

        let text = std::fs::read_to_string(store.path()).expect("readable");
        let value: serde_json::Value = serde_json::from_str(&text).expect("json");
        let mut keys: Vec<&str> = value["entries"][0]
            .as_object()
            .expect("an entry is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "accepted_unix",
                "algorithm",
                "fingerprint",
                "host",
                "port",
                "public_key"
            ],
            "a trust record may not grow a field that could hold a secret"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_first_run_reads_as_an_empty_library() {
        let root = scratch_root();
        let trust = TrustStore::at(&root)
            .load()
            .expect("a missing file is a first run");
        assert!(trust.is_empty());
        assert_eq!(trust.version(), TRUST_FORMAT_VERSION);
    }

    #[test]
    fn a_damaged_or_newer_file_is_named_as_such() {
        let root = scratch_root();
        let store = TrustStore::at(&root);
        std::fs::create_dir_all(&root).expect("directory");

        std::fs::write(store.path(), "{ not json").expect("write");
        assert!(matches!(store.load(), Err(StorageError::Corrupt { .. })));

        std::fs::write(store.path(), r#"{"version":0,"entries":[]}"#).expect("write");
        assert!(matches!(store.load(), Err(StorageError::Corrupt { .. })));

        std::fs::write(store.path(), r#"{"version":999,"entries":[]}"#).expect("write");
        match store.load() {
            Err(StorageError::UnsupportedVersion {
                found, supported, ..
            }) => {
                assert_eq!(found, 999);
                assert_eq!(supported, TRUST_FORMAT_VERSION);
            }
            other => panic!("a newer file must be named as newer, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_file_with_two_records_for_one_identity_is_rejected() {
        let root = scratch_root();
        let store = TrustStore::at(&root);
        std::fs::create_dir_all(&root).expect("directory");
        let presented = key("ssh-ed25519", 9);
        let document = format!(
            r#"{{"version":1,"entries":[
                {{"host":"web","port":22,"algorithm":"ssh-ed25519","fingerprint":"{}","public_key":"a","accepted_unix":1}},
                {{"host":"web","port":22,"algorithm":"ssh-ed25519","fingerprint":"{}","public_key":"a","accepted_unix":2}}
            ]}}"#,
            presented.fingerprint, presented.fingerprint
        );
        std::fs::write(store.path(), document).expect("write");
        assert!(matches!(store.load(), Err(StorageError::Corrupt { .. })));
        std::fs::remove_dir_all(&root).ok();
    }
}
