//! Secret-free metadata for the managed vault (R1, ADR-1).
//!
//! Key bodies and passwords live in the encrypted vault the storage layer keeps;
//! they are decrypted for one connection attempt at a time. What lives here is
//! everything a list, a log line or a sync merge may see: public metadata, stable
//! identifiers, record versions and tombstones. A host never copies any of it — it
//! keeps the identifier, which is what makes a shared credential one record instead
//! of the same password pasted into every host.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::host::HostConfig;
use crate::ids::{CredentialId, HostId, KeyId};

/// The version of one vault record: how many times it changed and which device
/// wrote the change (ADR-5).
///
/// The counter is monotonic per record and the device names the writer; sync uses
/// the pair to tell "both sides changed this" from "one side changed this", and it
/// is never a last-write-wins clock, which is why neither field alone orders
/// anything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultRecordVersion {
    counter: u64,
    device_id: String,
}

impl VaultRecordVersion {
    /// The version a freshly created record carries.
    #[must_use]
    pub fn new(counter: u64, device_id: impl Into<String>) -> Self {
        Self {
            counter,
            device_id: device_id.into(),
        }
    }

    /// How many changes the record has had.
    #[must_use]
    pub fn counter(&self) -> u64 {
        self.counter
    }

    /// Which device wrote this version.
    #[must_use]
    pub fn device_id(&self) -> &str {
        &self.device_id
    }
}

/// A deletion, kept instead of a missing record (ADR-5).
///
/// Removing a row outright would make "this device never saw the record" and "this
/// device deleted the record" the same state, and a sync that cannot tell them
/// apart resurrects deleted keys. A tombstone states the deletion, when it happened
/// and the version the record had when it was deleted, so a merge can report
/// "deleted here, changed there" instead of guessing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultTombstone {
    deleted_at: SystemTime,
    /// The last version the record had before it was deleted.
    version: VaultRecordVersion,
}

impl VaultTombstone {
    /// The tombstone written when a record is deleted at `deleted_at`.
    #[must_use]
    pub fn new(deleted_at: SystemTime, version: VaultRecordVersion) -> Self {
        Self {
            deleted_at,
            version,
        }
    }

    /// When the deletion happened.
    #[must_use]
    pub fn deleted_at(&self) -> SystemTime {
        self.deleted_at
    }

    /// The version the record had when it was deleted.
    #[must_use]
    pub fn version(&self) -> &VaultRecordVersion {
        &self.version
    }
}

/// The public metadata of one managed key.
///
/// The public key is the `authorized_keys` line a server would hold, so what the
/// list shows is exactly what an administrator needs to compare. The fingerprint
/// is the SHA-256 of the public key wire encoding in the `SHA256:…` form users
/// compare with `ssh-keygen -lf`. There is no private material here and no field
/// that could grow one: the body stays in the encrypted vault.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedKeyMeta {
    id: KeyId,
    name: String,
    /// The key algorithm as OpenSSH names it, for example `ssh-ed25519`.
    algorithm: String,
    /// The public key in `authorized_keys` format.
    public_key: String,
    /// SHA-256 fingerprint of the public key wire encoding.
    fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    comment: Option<String>,
    created_at: SystemTime,
    modified_at: SystemTime,
    version: VaultRecordVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deleted: Option<VaultTombstone>,
}

impl ManagedKeyMeta {
    /// A live key record.
    #[must_use]
    pub fn new(
        id: KeyId,
        name: impl Into<String>,
        algorithm: impl Into<String>,
        public_key: impl Into<String>,
        fingerprint: impl Into<String>,
        comment: Option<String>,
        version: VaultRecordVersion,
    ) -> Self {
        let now = SystemTime::now();
        Self {
            id,
            name: name.into(),
            algorithm: algorithm.into(),
            public_key: public_key.into(),
            fingerprint: fingerprint.into(),
            comment,
            created_at: now,
            modified_at: now,
            version,
            deleted: None,
        }
    }

    /// Stable identifier the host record and the vault storage share.
    #[must_use]
    pub fn id(&self) -> KeyId {
        self.id
    }

    /// User visible name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The key algorithm as OpenSSH names it.
    #[must_use]
    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }

    /// The public key in `authorized_keys` format.
    #[must_use]
    pub fn public_key(&self) -> &str {
        &self.public_key
    }

    /// SHA-256 fingerprint of the public key wire encoding.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// The comment of the `authorized_keys` line, when it carried one.
    #[must_use]
    pub fn comment(&self) -> Option<&str> {
        self.comment.as_deref()
    }

    /// When the record was created.
    #[must_use]
    pub fn created_at(&self) -> SystemTime {
        self.created_at
    }

    /// When the record last changed.
    #[must_use]
    pub fn modified_at(&self) -> SystemTime {
        self.modified_at
    }

    /// The version of the record.
    #[must_use]
    pub fn version(&self) -> &VaultRecordVersion {
        &self.version
    }

    /// The deletion, when the record is a tombstone.
    #[must_use]
    pub fn deleted(&self) -> Option<&VaultTombstone> {
        self.deleted.as_ref()
    }

    /// Whether the record exists, as opposed to having been deleted.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.deleted.is_none()
    }
}

/// The public metadata of one shared credential.
///
/// The password itself stays in the encrypted vault. Hosts reference the record by
/// identifier, so editing it here is immediately the reality for every host that
/// uses it; a credential that should change independently needs its own record,
/// which is what [`Self::duplicated`] starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialMeta {
    id: CredentialId,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    created_at: SystemTime,
    modified_at: SystemTime,
    version: VaultRecordVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deleted: Option<VaultTombstone>,
}

impl CredentialMeta {
    /// A live credential record.
    #[must_use]
    pub fn new(
        id: CredentialId,
        name: impl Into<String>,
        user: Option<String>,
        version: VaultRecordVersion,
    ) -> Self {
        let now = SystemTime::now();
        Self {
            id,
            name: name.into(),
            user,
            created_at: now,
            modified_at: now,
            version,
            deleted: None,
        }
    }

    /// Stable identifier the host record and the vault storage share.
    #[must_use]
    pub fn id(&self) -> CredentialId {
        self.id
    }

    /// User visible name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The login user this credential stands for, when it names one.
    #[must_use]
    pub fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    /// When the record was created.
    #[must_use]
    pub fn created_at(&self) -> SystemTime {
        self.created_at
    }

    /// When the record last changed.
    #[must_use]
    pub fn modified_at(&self) -> SystemTime {
        self.modified_at
    }

    /// The version of the record.
    #[must_use]
    pub fn version(&self) -> &VaultRecordVersion {
        &self.version
    }

    /// The deletion, when the record is a tombstone.
    #[must_use]
    pub fn deleted(&self) -> Option<&VaultTombstone> {
        self.deleted.as_ref()
    }

    /// Whether the record exists, as opposed to having been deleted.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.deleted.is_none()
    }

    /// An independent copy with a fresh identity, written by `device_id`.
    ///
    /// The copy shares nothing with the original: it gets a new identifier, so a
    /// host that keeps referencing the original is unaffected, and its version
    /// history starts at the copy. This is the model's answer to "I want the same
    /// password but mine alone" — sharing is a reference, and un-sharing is a new
    /// record.
    #[must_use]
    pub fn duplicated(&self, device_id: impl Into<String>) -> Self {
        let now = SystemTime::now();
        Self {
            id: CredentialId::new(),
            name: self.name.clone(),
            user: self.user.clone(),
            created_at: now,
            modified_at: now,
            version: VaultRecordVersion::new(1, device_id),
            deleted: None,
        }
    }
}

/// The public metadata of one host's remembered password.
///
/// A host whose password the user asked Shellune to remember keeps only the flag;
/// the password lives in the encrypted vault under the host's identifier, and this
/// is what a list, a log line or a sync merge may see of it. There is no name and
/// no field that could grow a secret: the record belongs to its host and to
/// nothing else, so it is never offered as a shared credential.
///
/// A deletion keeps the version the record had, exactly as the other vault
/// records do: the tombstone beside the version is what tells a deletion from a
/// record that did not change, and two devices that delete the same password do
/// not disagree about anything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSecretMeta {
    host: HostId,
    updated_at: SystemTime,
    version: VaultRecordVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deleted: Option<VaultTombstone>,
}

impl HostSecretMeta {
    /// A live record of `host`, written at `version`.
    #[must_use]
    pub fn new(host: HostId, version: VaultRecordVersion) -> Self {
        Self {
            host,
            updated_at: SystemTime::now(),
            version,
            deleted: None,
        }
    }

    /// The host the password belongs to, which is also the record's identifier.
    #[must_use]
    pub fn host(&self) -> HostId {
        self.host
    }

    /// When the record last changed, deletion included.
    #[must_use]
    pub fn updated_at(&self) -> SystemTime {
        self.updated_at
    }

    /// The version of the record.
    #[must_use]
    pub fn version(&self) -> &VaultRecordVersion {
        &self.version
    }

    /// The deletion, when the record is a tombstone.
    #[must_use]
    pub fn deleted(&self) -> Option<&VaultTombstone> {
        self.deleted.as_ref()
    }

    /// Whether the record exists, as opposed to having been deleted.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.deleted.is_none()
    }

    /// The live record that replaces this one, live or deleted, when
    /// `device_id` writes a new password: the counter goes up by one and the
    /// writer is named.
    #[must_use]
    pub fn revised(&self, device_id: impl Into<String>) -> Self {
        Self {
            host: self.host,
            updated_at: SystemTime::now(),
            version: VaultRecordVersion::new(self.version.counter() + 1, device_id),
            deleted: None,
        }
    }

    /// This record as a tombstone, deleted now. The version stays the one the
    /// record had; a tombstone stays as it is.
    #[must_use]
    pub fn tombstone(&self) -> Self {
        if self.deleted.is_some() {
            return self.clone();
        }
        let now = SystemTime::now();
        Self {
            host: self.host,
            updated_at: now,
            version: self.version.clone(),
            deleted: Some(VaultTombstone::new(now, self.version.clone())),
        }
    }
}

/// One key rotation as a single transaction: the key that takes over, the hosts
/// moved onto it and the key they stop using.
///
/// Nothing in here is applied piecemeal. [`VaultCatalog::rotate_key`] checks the
/// whole unit first and refuses it as a whole when any part would leave a dangling
/// reference or a half rotation behind, which is what a caller writes to storage
/// only after the check passes.
#[derive(Clone, Debug)]
pub struct KeyRotation {
    /// The key the rotated hosts use afterwards.
    pub new_key: ManagedKeyMeta,
    /// The key those hosts use today.
    pub previous_key: KeyId,
    /// The hosts moved from the previous key onto the new one.
    pub hosts: Vec<HostId>,
    /// Whether the previous key is tombstoned once the rotation leaves it unused.
    pub retire_previous: bool,
}

/// Why the vault refused an insert, a reference check or a rotation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VaultError {
    /// A record needs a name to be listed by.
    #[error("a vault record needs a name")]
    BlankName,
    /// A host or a rotation names a key the vault does not hold.
    #[error("no managed key {0}")]
    MissingKey(KeyId),
    /// A record was deleted, so a new reference to it would be a dangling one.
    #[error("managed key {0} was deleted")]
    DeletedKey(KeyId),
    /// A host names a credential the vault does not hold.
    #[error("no shared credential {0}")]
    MissingCredential(CredentialId),
    /// A record was deleted, so a new reference to it would be a dangling one.
    #[error("shared credential {0} was deleted")]
    DeletedCredential(CredentialId),
    /// The vault already holds a record with this identifier.
    #[error("the vault already holds managed key {0}")]
    DuplicateKey(KeyId),
    /// A live key with this public key fingerprint is already in the vault.
    #[error("a key with this fingerprint is already in the vault")]
    DuplicateFingerprint,
    /// The vault already holds a record with this identifier.
    #[error("the vault already holds shared credential {0}")]
    DuplicateCredential(CredentialId),
    /// A rotation lists a host the caller did not hand over with it.
    #[error("host {0} is not among the hosts of the rotation")]
    UnknownHost(HostId),
    /// A rotation moves a host that does not use the key being rotated.
    #[error("host {0} does not use the rotated key")]
    RotationHostNotOnKey(HostId),
    /// A rotation would retire a key another host still uses.
    #[error("managed key {0} is still used by host {1}")]
    KeyStillReferenced(KeyId, HostId),
}

/// The public metadata of every vault record this device holds.
///
/// Storage and sync fill this before a write and call the checks here first: a
/// write that would leave a host pointing at a missing or deleted record, a second
/// copy of an already imported public key, or half of a rotation is refused before
/// it reaches a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VaultCatalog {
    keys: Vec<ManagedKeyMeta>,
    credentials: Vec<CredentialMeta>,
}

impl VaultCatalog {
    /// An empty catalog.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every key record, live and deleted.
    #[must_use]
    pub fn keys(&self) -> &[ManagedKeyMeta] {
        &self.keys
    }

    /// Every credential record, live and deleted.
    #[must_use]
    pub fn credentials(&self) -> &[CredentialMeta] {
        &self.credentials
    }

    /// The key record with this identifier, live or deleted.
    #[must_use]
    pub fn key(&self, id: KeyId) -> Option<&ManagedKeyMeta> {
        self.keys.iter().find(|key| key.id() == id)
    }

    /// The credential record with this identifier, live or deleted.
    #[must_use]
    pub fn credential(&self, id: CredentialId) -> Option<&CredentialMeta> {
        self.credentials.iter().find(|record| record.id() == id)
    }

    /// Adds a key record.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::BlankName`] for a nameless record,
    /// [`VaultError::DuplicateKey`] when the identifier is taken and
    /// [`VaultError::DuplicateFingerprint`] when a live key already carries the
    /// same public key, so the same key cannot be imported twice under two names.
    pub fn insert_key(&mut self, key: ManagedKeyMeta) -> Result<(), VaultError> {
        if key.name.trim().is_empty() {
            return Err(VaultError::BlankName);
        }
        if self.key(key.id()).is_some() {
            return Err(VaultError::DuplicateKey(key.id()));
        }
        if self
            .keys
            .iter()
            .any(|existing| existing.is_live() && existing.fingerprint() == key.fingerprint())
        {
            return Err(VaultError::DuplicateFingerprint);
        }
        self.keys.push(key);
        Ok(())
    }

    /// Adds a credential record.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::BlankName`] for a nameless record and
    /// [`VaultError::DuplicateCredential`] when the identifier is taken.
    pub fn insert_credential(&mut self, credential: CredentialMeta) -> Result<(), VaultError> {
        if credential.name.trim().is_empty() {
            return Err(VaultError::BlankName);
        }
        if self.credential(credential.id()).is_some() {
            return Err(VaultError::DuplicateCredential(credential.id()));
        }
        self.credentials.push(credential);
        Ok(())
    }

    /// Replaces the metadata of an existing credential record.
    ///
    /// Every host that references the credential sees the change at once, because
    /// the hosts hold the identifier and not a copy of the record.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::MissingCredential`] or
    /// [`VaultError::DeletedCredential`] when there is no live record to replace.
    pub fn update_credential(&mut self, credential: CredentialMeta) -> Result<(), VaultError> {
        let existing = self
            .credentials
            .iter_mut()
            .find(|record| record.id() == credential.id())
            .ok_or(VaultError::MissingCredential(credential.id()))?;
        if !existing.is_live() {
            return Err(VaultError::DeletedCredential(credential.id()));
        }
        *existing = credential;
        Ok(())
    }

    /// Checks that every vault reference of these hosts resolves to a live record.
    ///
    /// # Errors
    ///
    /// Returns the first missing or deleted reference as a [`VaultError`], so a
    /// write is refused before a host can end up pointing at nothing.
    pub fn validate_references(&self, hosts: &[HostConfig]) -> Result<(), VaultError> {
        for host in hosts {
            if let Some(key_id) = host.managed_key_id() {
                match self.key(key_id) {
                    None => return Err(VaultError::MissingKey(key_id)),
                    Some(key) if !key.is_live() => {
                        return Err(VaultError::DeletedKey(key_id));
                    }
                    Some(_) => {}
                }
            }
            if let Some(credential_id) = host.credential_id() {
                match self.credential(credential_id) {
                    None => return Err(VaultError::MissingCredential(credential_id)),
                    Some(record) if !record.is_live() => {
                        return Err(VaultError::DeletedCredential(credential_id));
                    }
                    Some(_) => {}
                }
            }
        }
        Ok(())
    }

    /// Applies a rotation as one transaction, or refuses it as a whole.
    ///
    /// Every check runs before anything changes: a rotation that would retire a key
    /// another host still uses, move a host that is not on the rotated key, or
    /// import a fingerprint the vault already holds leaves the catalog and the
    /// hosts exactly as they were. On success the new key exists, every listed host
    /// points at it, and the previous key is either untouched or a tombstone.
    ///
    /// # Errors
    ///
    /// Returns the first problem as a [`VaultError`] and changes nothing.
    pub fn rotate_key(
        &mut self,
        rotation: KeyRotation,
        hosts: &mut [HostConfig],
    ) -> Result<(), VaultError> {
        let previous = self
            .key(rotation.previous_key)
            .ok_or(VaultError::MissingKey(rotation.previous_key))?
            .clone();
        if !previous.is_live() {
            return Err(VaultError::DeletedKey(rotation.previous_key));
        }
        // The new record has to be insertable: a taken identifier or a known
        // fingerprint would leave the rotation half applied, with hosts moved onto
        // a key that is not the one the caller described.
        if self.key(rotation.new_key.id()).is_some() {
            return Err(VaultError::DuplicateKey(rotation.new_key.id()));
        }
        if self.keys.iter().any(|existing| {
            existing.is_live() && existing.fingerprint() == rotation.new_key.fingerprint()
        }) {
            return Err(VaultError::DuplicateFingerprint);
        }
        if rotation.new_key.name.trim().is_empty() {
            return Err(VaultError::BlankName);
        }
        for rotated_host in &rotation.hosts {
            let host = hosts
                .iter_mut()
                .find(|host| host.id() == *rotated_host)
                .ok_or(VaultError::UnknownHost(*rotated_host))?;
            if host.managed_key_id() != Some(rotation.previous_key) {
                return Err(VaultError::RotationHostNotOnKey(*rotated_host));
            }
        }
        // Retiring a key that still has users would leave their references
        // dangling the moment the tombstone lands.
        if rotation.retire_previous {
            for host in &*hosts {
                if host.managed_key_id() == Some(rotation.previous_key)
                    && !rotation.hosts.contains(&host.id())
                {
                    return Err(VaultError::KeyStillReferenced(
                        rotation.previous_key,
                        host.id(),
                    ));
                }
            }
        }

        let new_key_id = rotation.new_key.id();
        self.keys.push(rotation.new_key);
        for rotated_host in &rotation.hosts {
            if let Some(host) = hosts.iter_mut().find(|host| host.id() == *rotated_host) {
                host.set_managed_key_id(Some(new_key_id));
            }
        }
        if rotation.retire_previous {
            let retired = self
                .keys
                .iter_mut()
                .find(|key| key.id() == rotation.previous_key)
                .expect("the previous key was found and cloned above");
            let tombstone = VaultTombstone::new(SystemTime::now(), previous.version().clone());
            retired.deleted = Some(tombstone);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: KeyId, fingerprint: &str) -> ManagedKeyMeta {
        ManagedKeyMeta::new(
            id,
            "Deploy key",
            "ssh-ed25519",
            format!("ssh-ed25519 AAAA{fingerprint} deploy@laptop"),
            format!("SHA256:{fingerprint}"),
            Some("deploy@laptop".to_owned()),
            VaultRecordVersion::new(3, "device-a"),
        )
    }

    fn credential(id: CredentialId) -> CredentialMeta {
        CredentialMeta::new(
            id,
            "Prod login",
            Some("deploy".to_owned()),
            VaultRecordVersion::new(2, "device-a"),
        )
    }

    fn host(name: &str) -> HostConfig {
        HostConfig::new(name, "example.com", 22, "deploy").expect("valid host")
    }

    #[test]
    fn metadata_versions_and_tombstones_round_trip() {
        let key_record = key(KeyId::new(), "abc123");
        let credential_record = credential(CredentialId::new());

        let encoded = serde_json::to_string(&key_record).expect("key serializes");
        assert!(
            encoded.contains("\"fingerprint\":\"SHA256:abc123\""),
            "{encoded}"
        );
        assert!(!encoded.contains("private"), "{encoded}");
        let back: ManagedKeyMeta = serde_json::from_str(&encoded).expect("key reads");
        assert_eq!(back, key_record);
        assert!(back.is_live());

        let back: CredentialMeta =
            serde_json::from_str(&serde_json::to_string(&credential_record).expect("serializes"))
                .expect("credential reads");
        assert_eq!(back, credential_record);
        assert_eq!(back.version().counter(), 2);
        assert_eq!(back.version().device_id(), "device-a");

        // A tombstone is a record, not an absence: it survives the round trip and
        // says when and at which version the deletion happened.
        let mut deleted = credential(CredentialId::new());
        let tombstone = VaultTombstone::new(
            SystemTime::UNIX_EPOCH,
            VaultRecordVersion::new(5, "device-b"),
        );
        deleted.deleted = Some(tombstone.clone());
        let back: CredentialMeta =
            serde_json::from_str(&serde_json::to_string(&deleted).expect("serializes"))
                .expect("tombstoned credential reads");
        assert_eq!(back.deleted(), Some(&tombstone));
        assert!(!back.is_live(), "a tombstoned record is not a live one");

        let encoded = serde_json::to_string(&VaultRecordVersion::new(7, "device-c"))
            .expect("version serializes");
        assert_eq!(
            encoded, "{\"counter\":7,\"device_id\":\"device-c\"}",
            "{encoded}"
        );
    }

    #[test]
    fn a_host_secret_record_is_revised_and_tombstoned_under_its_host() {
        let host = HostId::new();
        let live = HostSecretMeta::new(host, VaultRecordVersion::new(1, "device-a"));
        assert!(live.is_live());
        assert_eq!(live.host(), host);

        let back: HostSecretMeta =
            serde_json::from_str(&serde_json::to_string(&live).expect("serializes"))
                .expect("reads");
        assert_eq!(back, live);
        let encoded = serde_json::to_string(&live).expect("serializes");
        assert!(
            !encoded.contains("deleted"),
            "a live record says nothing of deletion"
        );

        let revised = live.revised("device-b");
        assert!(revised.is_live());
        assert_eq!(revised.version(), &VaultRecordVersion::new(2, "device-b"));

        let deleted = revised.tombstone();
        assert!(!deleted.is_live());
        assert_eq!(deleted.host(), host);
        assert_eq!(
            deleted.version(),
            revised.version(),
            "a deletion keeps the version it had"
        );
        assert_eq!(
            deleted.deleted().map(VaultTombstone::version),
            Some(revised.version())
        );
        assert_eq!(deleted.tombstone(), deleted, "a tombstone stays as it is");
        let back: HostSecretMeta =
            serde_json::from_str(&serde_json::to_string(&deleted).expect("serializes"))
                .expect("reads");
        assert_eq!(back, deleted);

        let again = deleted.revised("device-a");
        assert!(again.is_live(), "a new password revives the record");
        assert_eq!(again.version(), &VaultRecordVersion::new(3, "device-a"));
    }

    #[test]
    fn a_host_holds_only_stable_references() {
        let key_id = KeyId::new();
        let credential_id = CredentialId::new();
        let mut referenced = host("Build");
        referenced.set_auth(Some(crate::auth::AuthMethodKind::ManagedKey));
        referenced.set_managed_key_id(Some(key_id));
        referenced.set_credential_id(Some(credential_id));

        assert_eq!(referenced.managed_key_id(), Some(key_id));
        assert_eq!(referenced.credential_id(), Some(credential_id));
        let back: HostConfig =
            serde_json::from_str(&serde_json::to_string(&referenced).expect("serializes"))
                .expect("reads");
        assert_eq!(back.managed_key_id(), Some(key_id));
        assert_eq!(back.credential_id(), Some(credential_id));

        // Without references the fields are not written at all, which is what
        // keeps a record saved by a build without a vault byte-identical.
        let plain = host("Web");
        let encoded = serde_json::to_string(&plain).expect("serializes");
        assert!(!encoded.contains("managed_key_id"), "{encoded}");
        assert!(!encoded.contains("credential_id"), "{encoded}");
    }

    #[test]
    fn switching_the_sign_in_method_clears_references_it_no_longer_uses() {
        use crate::auth::AuthMethodKind as Kind;

        let mut host = host("Build");
        host.set_auth(Some(Kind::ManagedKey));
        host.set_managed_key_id(Some(KeyId::new()));
        host.set_auth(Some(Kind::Password));
        host.set_credential_id(Some(CredentialId::new()));

        // A password method cannot read a managed key, so the key reference went
        // with the method that used it; the credential it can read stays.
        assert_eq!(host.managed_key_id(), None);
        assert!(host.credential_id().is_some());

        host.set_auth(Some(Kind::Agent));
        assert_eq!(host.credential_id(), None, "the agent reads no credential");

        // The same holds when the choice is removed entirely.
        host.set_auth(Some(Kind::ManagedKey));
        host.set_managed_key_id(Some(KeyId::new()));
        host.set_auth(None);
        assert_eq!(host.managed_key_id(), None);
    }

    #[test]
    fn editing_a_shared_credential_reaches_every_host_that_uses_it() {
        let id = CredentialId::new();
        let mut catalog = VaultCatalog::new();
        catalog
            .insert_credential(credential(id))
            .expect("first insert");

        let mut first = host("Build");
        let mut second = host("Web");
        first.set_credential_id(Some(id));
        second.set_credential_id(Some(id));
        let hosts = [first, second];
        for host in &hosts {
            assert_eq!(host.credential_id(), Some(id));
        }
        assert_eq!(hosts.len(), 2, "both hosts reference the one record");

        let mut edited = credential(id);
        edited.user = Some("root".to_owned());
        catalog
            .update_credential(edited)
            .expect("live record is editable");

        // The hosts hold an identifier, not a copy, so every one of them resolves
        // the edited record and none of them had to change.
        for host in &hosts {
            let record = catalog
                .credential(host.credential_id().expect("reference kept"))
                .expect("record found");
            assert_eq!(record.user(), Some("root"));
        }
    }

    #[test]
    fn an_independent_copy_gets_a_new_credential_id() {
        let original = credential(CredentialId::new());
        let copy = original.duplicated("device-b");

        assert_ne!(original.id(), copy.id());
        assert_eq!(original.name(), copy.name());
        assert_eq!(copy.version().counter(), 1);
        assert_eq!(copy.version().device_id(), "device-b");

        let mut catalog = VaultCatalog::new();
        catalog
            .insert_credential(original.clone())
            .expect("original inserts");
        catalog
            .insert_credential(copy.clone())
            .expect("copy inserts under its own identity");
        assert_ne!(
            catalog.credential(original.id()),
            catalog.credential(copy.id())
        );
    }

    #[test]
    fn importing_the_same_public_key_twice_is_refused() {
        let mut catalog = VaultCatalog::new();
        catalog
            .insert_key(key(KeyId::new(), "same"))
            .expect("first import");
        let duplicate = key(KeyId::new(), "same");
        assert_eq!(
            catalog.insert_key(duplicate),
            Err(VaultError::DuplicateFingerprint)
        );
        assert_eq!(catalog.keys().len(), 1);

        // A deleted key does not block a later import of the same public key:
        // the tombstone says the old record is gone, not that the key is banned.
        let mut retired = key(KeyId::new(), "other");
        retired.deleted = Some(VaultTombstone::new(
            SystemTime::now(),
            VaultRecordVersion::new(4, "device-a"),
        ));
        catalog.insert_key(retired).expect("tombstone inserts");
        catalog
            .insert_key(key(KeyId::new(), "other"))
            .expect("a fingerprint may return after deletion");

        // A record the list could not show by name is refused before it lands.
        let nameless = ManagedKeyMeta::new(
            KeyId::new(),
            "   ",
            "ssh-ed25519",
            "ssh-ed25519 AAAAblank",
            "SHA256:blank",
            None,
            VaultRecordVersion::new(1, "device-a"),
        );
        assert_eq!(catalog.insert_key(nameless), Err(VaultError::BlankName));
    }

    #[test]
    fn a_dangling_reference_is_refused() {
        let mut catalog = VaultCatalog::new();
        let key_id = KeyId::new();
        catalog.insert_key(key(key_id, "live")).expect("inserts");
        let mut deleted = key(KeyId::new(), "retired");
        deleted.deleted = Some(VaultTombstone::new(
            SystemTime::now(),
            VaultRecordVersion::new(6, "device-a"),
        ));
        catalog.insert_key(deleted.clone()).expect("inserts");

        let mut host = host("Build");
        host.set_managed_key_id(Some(key_id));
        host.set_credential_id(None);
        catalog
            .validate_references(&[host.clone()])
            .expect("live key");

        host.set_managed_key_id(Some(deleted.id()));
        assert_eq!(
            catalog.validate_references(&[host.clone()]),
            Err(VaultError::DeletedKey(deleted.id()))
        );

        let missing = KeyId::new();
        host.set_managed_key_id(Some(missing));
        assert_eq!(
            catalog.validate_references(&[host.clone()]),
            Err(VaultError::MissingKey(missing))
        );

        let missing_credential = CredentialId::new();
        host.set_managed_key_id(None);
        host.set_credential_id(Some(missing_credential));
        assert_eq!(
            catalog.validate_references(&[host]),
            Err(VaultError::MissingCredential(missing_credential))
        );
    }

    #[test]
    fn a_rotation_moves_hosts_and_retires_the_old_key_as_one_unit() {
        let mut catalog = VaultCatalog::new();
        let previous_id = KeyId::new();
        catalog
            .insert_key(key(previous_id, "old"))
            .expect("previous key inserts");

        let mut first = host("Build");
        let mut second = host("Web");
        for host in [&mut first, &mut second] {
            host.set_managed_key_id(Some(previous_id));
        }
        let new_key = key(KeyId::new(), "new");
        let mut hosts = vec![first, second];

        catalog
            .rotate_key(
                KeyRotation {
                    new_key: new_key.clone(),
                    previous_key: previous_id,
                    hosts: vec![hosts[0].id(), hosts[1].id()],
                    retire_previous: true,
                },
                &mut hosts,
            )
            .expect("rotation applies");

        for host in &hosts {
            assert_eq!(host.managed_key_id(), Some(new_key.id()));
        }
        let retired = catalog
            .key(previous_id)
            .expect("record kept as a tombstone");
        assert!(
            retired.deleted().is_some(),
            "the old key is deleted, not gone"
        );
        assert!(
            catalog
                .key(new_key.id())
                .is_some_and(ManagedKeyMeta::is_live)
        );
        catalog
            .validate_references(&hosts)
            .expect("no dangling reference");
    }

    #[test]
    fn a_refused_rotation_changes_nothing() {
        let mut catalog = VaultCatalog::new();
        let previous_id = KeyId::new();
        catalog
            .insert_key(key(previous_id, "old"))
            .expect("previous key inserts");

        let mut first = host("Build");
        let mut second = host("Web");
        first.set_managed_key_id(Some(previous_id));
        second.set_managed_key_id(Some(previous_id));
        let mut hosts = vec![first.clone(), second.clone()];
        let mut before = hosts_snapshot(&hosts);
        let catalog_before = catalog.clone();

        // Retiring is refused while a referencing host is left behind, and the
        // refusal is total: no host moved, no key inserted, no tombstone written.
        let new_key = key(KeyId::new(), "new");
        assert_eq!(
            catalog.rotate_key(
                KeyRotation {
                    new_key: new_key.clone(),
                    previous_key: previous_id,
                    hosts: vec![first.id()],
                    retire_previous: true,
                },
                &mut hosts,
            ),
            Err(VaultError::KeyStillReferenced(previous_id, second.id()))
        );
        assert_eq!(hosts, before);
        assert!(catalog.key(new_key.id()).is_none());
        assert!(
            catalog
                .key(previous_id)
                .is_some_and(ManagedKeyMeta::is_live)
        );
        assert_eq!(catalog, catalog_before);

        // A host that never used the rotated key cannot be moved by it.
        let stranger = host("Db");
        hosts.push(stranger.clone());
        before.push(stranger.clone());
        assert_eq!(
            catalog.rotate_key(
                KeyRotation {
                    new_key: key(KeyId::new(), "new"),
                    previous_key: previous_id,
                    hosts: vec![stranger.id()],
                    retire_previous: false,
                },
                &mut hosts,
            ),
            Err(VaultError::RotationHostNotOnKey(stranger.id()))
        );
        assert_eq!(
            hosts, before,
            "a refused rotation leaves every host exactly as it was"
        );

        // The same public key cannot arrive twice, not even by rotation.
        assert_eq!(
            catalog.rotate_key(
                KeyRotation {
                    new_key: key(KeyId::new(), "old"),
                    previous_key: previous_id,
                    hosts: vec![first.id()],
                    retire_previous: false,
                },
                &mut hosts,
            ),
            Err(VaultError::DuplicateFingerprint)
        );
        assert_eq!(hosts, before);
    }

    #[test]
    fn a_rotation_from_a_missing_or_deleted_key_is_refused() {
        let mut catalog = VaultCatalog::new();
        let mut retired = key(KeyId::new(), "retired");
        retired.deleted = Some(VaultTombstone::new(
            SystemTime::now(),
            VaultRecordVersion::new(2, "device-a"),
        ));
        catalog.insert_key(retired.clone()).expect("inserts");

        let mut host = host("Build");
        host.set_managed_key_id(Some(retired.id()));
        assert_eq!(
            catalog.rotate_key(
                KeyRotation {
                    new_key: key(KeyId::new(), "new"),
                    previous_key: retired.id(),
                    hosts: vec![host.id()],
                    retire_previous: false,
                },
                &mut [host.clone()],
            ),
            Err(VaultError::DeletedKey(retired.id()))
        );

        let missing = KeyId::new();
        assert_eq!(
            catalog.rotate_key(
                KeyRotation {
                    new_key: key(KeyId::new(), "new"),
                    previous_key: missing,
                    hosts: vec![host.id()],
                    retire_previous: false,
                },
                &mut [host],
            ),
            Err(VaultError::MissingKey(missing))
        );
        assert_eq!(catalog.keys().len(), 1, "no half rotation was written");
    }

    fn hosts_snapshot(hosts: &[HostConfig]) -> Vec<HostConfig> {
        hosts.to_vec()
    }
}
