//! What the vault holds once it is unsealed (ADR-1, ADR-5).
//!
//! Every record pairs the public metadata the core model defines — name,
//! algorithm, fingerprint, timestamps, the record version and a possible
//! tombstone — with the one secret that must never be public: the OpenSSH
//! private material of a managed key or the password of a shared credential.
//! The secret travels only inside the sealed body (see [`super::crypto`]) and
//! lives as a [`SecretString`], so it is wiped when the record is dropped.
//!
//! The content also holds the *reference snapshot*: which host references
//! which vault record. Hosts keep only identifiers, and the vault keeps the
//! mirror of that, so a write can be checked against the host book as a whole
//! before it reaches a file — a mirror that drifted from the book is a
//! corruption, not a truth to pick over. The accepted server identities
//! ([`HostTrust`]) live here too: from format 2 on, the vault is the authority
//! for trust decisions, and the plaintext file is only a migration input.

use std::collections::BTreeMap;
use std::time::SystemTime;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use shellune_core::{
    AuthMethodKind, CredentialId, CredentialMeta, HostConfig, HostId, KeyId, ManagedKeyMeta,
    SecretString, VaultCatalog, VaultRecordVersion, VaultTombstone,
};
use zeroize::Zeroize as _;
use zeroize::Zeroizing;

use crate::trust::HostTrust;

use super::store::VaultStoreError;

/// One managed key: its public metadata and its private material.
#[derive(Clone, Debug)]
pub struct VaultKeyRecord {
    meta: ManagedKeyMeta,
    private_key: SecretString,
}

impl VaultKeyRecord {
    /// A live key record: the metadata and the OpenSSH private material it guards.
    #[must_use]
    pub fn new(meta: ManagedKeyMeta, private_key: SecretString) -> Self {
        Self { meta, private_key }
    }

    /// The public metadata, as the core model defines it.
    #[must_use]
    pub fn meta(&self) -> &ManagedKeyMeta {
        &self.meta
    }

    /// The private material, when the record is live. A tombstone keeps the
    /// metadata and drops the body: a deleted key is a fact about the record,
    /// not a secret worth keeping.
    #[must_use]
    pub fn private_key(&self) -> Option<&SecretString> {
        self.meta.is_live().then_some(&self.private_key)
    }
}

/// One shared credential: its public metadata and its password.
#[derive(Clone, Debug)]
pub struct VaultCredentialRecord {
    meta: CredentialMeta,
    secret: SecretString,
}

impl VaultCredentialRecord {
    /// A live credential record.
    #[must_use]
    pub fn new(meta: CredentialMeta, secret: SecretString) -> Self {
        Self { meta, secret }
    }

    /// The public metadata, as the core model defines it.
    #[must_use]
    pub fn meta(&self) -> &CredentialMeta {
        &self.meta
    }

    /// The password, when the record is live.
    #[must_use]
    pub fn secret(&self) -> Option<&SecretString> {
        self.meta.is_live().then_some(&self.secret)
    }
}

/// The mirror of one host's vault references.
///
/// This is the vault's copy of what the host book says, taken as a whole
/// before every write, so a host that points at nothing — or a vault that
/// claims a host nobody saved — is refused instead of persisted.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostReference {
    host: HostId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    managed_key_id: Option<KeyId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credential_id: Option<CredentialId>,
}

impl HostReference {
    /// The references one host holds today.
    #[must_use]
    pub fn of(host: &HostConfig) -> Self {
        Self {
            host: host.id(),
            managed_key_id: host.managed_key_id(),
            credential_id: host.credential_id(),
        }
    }

    /// The reference snapshot of a whole host book: every host that names a
    /// vault record, in a fixed order, without duplicates.
    #[must_use]
    pub fn snapshot(hosts: &[HostConfig]) -> Vec<Self> {
        let mut references: Vec<Self> = hosts
            .iter()
            .map(Self::of)
            .filter(|reference| {
                reference.managed_key_id.is_some() || reference.credential_id.is_some()
            })
            .collect();
        references.sort();
        references.dedup();
        references
    }

    /// The host the reference belongs to.
    #[must_use]
    pub fn host(&self) -> HostId {
        self.host
    }

    /// The managed key the host signs in with, when it names one.
    #[must_use]
    pub fn managed_key_id(&self) -> Option<KeyId> {
        self.managed_key_id
    }

    /// The shared credential the host signs in with, when it names one.
    #[must_use]
    pub fn credential_id(&self) -> Option<CredentialId> {
        self.credential_id
    }
}

/// Everything the vault guards, before it is sealed.
///
/// Constructing and editing is deliberate and checked: a record needs a name,
/// an identifier may be taken only once, and removing a record writes a
/// tombstone instead of an absence, so a later merge can tell "deleted here"
/// from "never saw it".
#[derive(Clone, Debug, Default)]
pub struct VaultContent {
    keys: BTreeMap<KeyId, VaultKeyRecord>,
    credentials: BTreeMap<CredentialId, VaultCredentialRecord>,
    references: Vec<HostReference>,
    trust: HostTrust,
}

impl VaultContent {
    /// An empty vault.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every key record, live and deleted, ordered by identifier.
    pub fn keys(&self) -> impl Iterator<Item = &VaultKeyRecord> {
        self.keys.values()
    }

    /// The key record with this identifier, live or deleted.
    #[must_use]
    pub fn key(&self, id: KeyId) -> Option<&VaultKeyRecord> {
        self.keys.get(&id)
    }

    /// Adds a key record.
    ///
    /// # Errors
    ///
    /// Returns [`VaultStoreError::Invalid`] for a nameless record and for an
    /// identifier the vault already holds.
    pub fn insert_key(&mut self, record: VaultKeyRecord) -> Result<(), VaultStoreError> {
        if record.meta.name().trim().is_empty() {
            return Err(VaultStoreError::Invalid {
                reason: "a vault record needs a name to be listed by".to_owned(),
            });
        }
        let id = record.meta.id();
        if self.keys.contains_key(&id) {
            return Err(VaultStoreError::Invalid {
                reason: format!("the vault already holds managed key {id}"),
            });
        }
        self.keys.insert(id, record);
        Ok(())
    }

    /// Deletes a key record, keeping its metadata as a tombstone and dropping
    /// the private material. Whether the record existed.
    ///
    /// # Errors
    ///
    /// Returns [`VaultStoreError::Encode`] or [`VaultStoreError::Corrupt`] in
    /// the impossible case the metadata cannot be written as a tombstone; the
    /// record is then left exactly as it was.
    pub fn remove_key(&mut self, id: KeyId) -> Result<bool, VaultStoreError> {
        match self.keys.remove(&id) {
            Some(record) if record.meta().is_live() => {
                let meta = tombstoned(&record.meta, record.meta.version())?;
                // The body was dropped with the record; a tombstone holds none.
                self.keys
                    .insert(id, VaultKeyRecord::new(meta, SecretString::new("")));
                Ok(true)
            }
            // A tombstone is a record, not an absence: deleting it again is no
            // change, and it stays.
            Some(record) => {
                self.keys.insert(id, record);
                Ok(false)
            }
            None => Ok(false),
        }
    }

    /// Every credential record, live and deleted, ordered by identifier.
    pub fn credentials(&self) -> impl Iterator<Item = &VaultCredentialRecord> {
        self.credentials.values()
    }

    /// The credential record with this identifier, live or deleted.
    #[must_use]
    pub fn credential(&self, id: CredentialId) -> Option<&VaultCredentialRecord> {
        self.credentials.get(&id)
    }

    /// Adds a credential record.
    ///
    /// # Errors
    ///
    /// Returns [`VaultStoreError::Invalid`] for a nameless record and for an
    /// identifier the vault already holds.
    pub fn insert_credential(
        &mut self,
        record: VaultCredentialRecord,
    ) -> Result<(), VaultStoreError> {
        if record.meta.name().trim().is_empty() {
            return Err(VaultStoreError::Invalid {
                reason: "a vault record needs a name to be listed by".to_owned(),
            });
        }
        let id = record.meta.id();
        if self.credentials.contains_key(&id) {
            return Err(VaultStoreError::Invalid {
                reason: format!("the vault already holds shared credential {id}"),
            });
        }
        self.credentials.insert(id, record);
        Ok(())
    }

    /// Replaces the metadata of a live credential, which is how every host that
    /// references it sees the change at once.
    ///
    /// # Errors
    ///
    /// Returns [`VaultStoreError::Invalid`] when no live record with this
    /// identifier exists.
    pub fn update_credential(&mut self, meta: CredentialMeta) -> Result<(), VaultStoreError> {
        if meta.name().trim().is_empty() {
            return Err(VaultStoreError::Invalid {
                reason: "a vault record needs a name to be listed by".to_owned(),
            });
        }
        let existing =
            self.credentials
                .get_mut(&meta.id())
                .ok_or_else(|| VaultStoreError::Invalid {
                    reason: format!("the vault holds no shared credential {}", meta.id()),
                })?;
        if !existing.meta.is_live() {
            return Err(VaultStoreError::Invalid {
                reason: format!("shared credential {} was deleted", meta.id()),
            });
        }
        existing.meta = meta;
        Ok(())
    }

    /// Deletes a credential record, keeping its metadata as a tombstone and
    /// dropping the password. Whether the record existed.
    ///
    /// # Errors
    ///
    /// Returns [`VaultStoreError::Encode`] or [`VaultStoreError::Corrupt`] in
    /// the impossible case the metadata cannot be written as a tombstone; the
    /// record is then left exactly as it was.
    pub fn remove_credential(&mut self, id: CredentialId) -> Result<bool, VaultStoreError> {
        match self.credentials.remove(&id) {
            Some(record) if record.meta().is_live() => {
                let meta = tombstoned(&record.meta, record.meta.version())?;
                self.credentials
                    .insert(id, VaultCredentialRecord::new(meta, SecretString::new("")));
                Ok(true)
            }
            Some(record) => {
                self.credentials.insert(id, record);
                Ok(false)
            }
            None => Ok(false),
        }
    }

    /// The reference snapshot the vault holds.
    #[must_use]
    pub fn references(&self) -> &[HostReference] {
        &self.references
    }

    /// Replaces the reference snapshot, in the fixed order of
    /// [`HostReference::snapshot`]. A reference that names no record is dropped:
    /// it would say nothing.
    pub fn set_references(&mut self, references: &[HostReference]) {
        let mut references: Vec<HostReference> = references
            .iter()
            .filter(|reference| {
                reference.managed_key_id.is_some() || reference.credential_id.is_some()
            })
            .cloned()
            .collect();
        references.sort();
        references.dedup();
        self.references = references;
    }

    /// The accepted server identities the vault guards.
    #[must_use]
    pub fn trust(&self) -> &HostTrust {
        &self.trust
    }

    /// Replaces the accepted server identities.
    pub fn set_trust(&mut self, trust: HostTrust) {
        self.trust = trust;
    }

    /// The public metadata of every record, as one catalog.
    ///
    /// # Errors
    ///
    /// Returns [`VaultStoreError::Invalid`] when two records could not live in
    /// one catalog, for example a second copy of an already imported public key.
    pub fn catalog(&self) -> Result<VaultCatalog, VaultStoreError> {
        let mut catalog = VaultCatalog::new();
        for record in self.keys.values() {
            catalog
                .insert_key(record.meta.clone())
                .map_err(|error| VaultStoreError::Invalid {
                    reason: error.to_string(),
                })?;
        }
        for record in self.credentials.values() {
            catalog
                .insert_credential(record.meta.clone())
                .map_err(|error| VaultStoreError::Invalid {
                    reason: error.to_string(),
                })?;
        }
        Ok(catalog)
    }

    /// Checks the content against a whole host book before it is written.
    ///
    /// This is the consumer side of the core checks: the vault refuses to be
    /// saved when a host points at a record it does not hold, when a reference
    /// travels without the sign-in method that reads it, or when the reference
    /// snapshot is not the snapshot of exactly these hosts.
    ///
    /// # Errors
    ///
    /// Returns [`VaultStoreError::Invalid`] naming the first problem; nothing
    /// is written.
    pub fn validate_for(&self, hosts: &[HostConfig]) -> Result<(), VaultStoreError> {
        let invalid = |reason: String| VaultStoreError::Invalid { reason };
        let catalog = self.catalog()?;
        catalog
            .validate_references(hosts)
            .map_err(|error| invalid(error.to_string()))?;
        for host in hosts {
            if host.managed_key_id().is_some() && host.auth() != Some(AuthMethodKind::ManagedKey) {
                return Err(invalid(format!(
                    "the host {} keeps a managed key reference without the sign-in method that reads it",
                    host.id()
                )));
            }
            if host.credential_id().is_some() && host.auth() != Some(AuthMethodKind::Password) {
                return Err(invalid(format!(
                    "the host {} keeps a shared credential reference without the sign-in method that reads it",
                    host.id()
                )));
            }
        }
        let expected = HostReference::snapshot(hosts);
        if self.references != expected {
            return Err(invalid(
                "the host references saved in the vault are not the references of this host book"
                    .to_owned(),
            ));
        }
        self.trust.validate().map_err(invalid)?;
        Ok(())
    }
}

/// Marks a metadata record deleted by re-reading it with a tombstone.
///
/// The core model keeps the field private so a host can never grow one by
/// accident; storage, which owns deletions, writes it the same way the sealed
/// file does and reads it back, which is the one path the model itself offers.
fn tombstoned<T>(meta: &T, version: &VaultRecordVersion) -> Result<T, VaultStoreError>
where
    T: Serialize + DeserializeOwned,
{
    let mut value = serde_json::to_value(meta).map_err(|error| VaultStoreError::Encode {
        reason: error.to_string(),
    })?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "deleted".to_owned(),
            serde_json::to_value(VaultTombstone::new(SystemTime::now(), version.clone())).map_err(
                |error| VaultStoreError::Encode {
                    reason: error.to_string(),
                },
            )?,
        );
    }
    serde_json::from_value(value).map_err(|error| VaultStoreError::Corrupt {
        reason: error.to_string(),
    })
}

/// The body as it is sealed: metadata in the clear of the plaintext, secrets as
/// the one place they exist between memory and the cipher.
#[derive(Serialize, Deserialize)]
struct WireBody {
    keys: Vec<WireKey>,
    credentials: Vec<WireCredential>,
    #[serde(default)]
    references: Vec<HostReference>,
    #[serde(default)]
    trust: HostTrust,
}

#[derive(Serialize, Deserialize)]
struct WireKey {
    meta: ManagedKeyMeta,
    private_key: String,
}

#[derive(Serialize, Deserialize)]
struct WireCredential {
    meta: CredentialMeta,
    secret: String,
}

/// Serializes the content into the bytes that will be sealed. The bytes are
/// returned zeroing, so the plaintext leaves no copy behind once it is used.
pub(super) fn encode(content: &VaultContent) -> Result<Zeroizing<Vec<u8>>, VaultStoreError> {
    let wire = WireBody {
        keys: content
            .keys
            .values()
            .map(|record| WireKey {
                meta: record.meta.clone(),
                // A tombstone holds no body; writing an empty string keeps the
                // field of a deleted record from growing meaning.
                private_key: if record.meta.is_live() {
                    record.private_key.expose().to_owned()
                } else {
                    String::new()
                },
            })
            .collect(),
        credentials: content
            .credentials
            .values()
            .map(|record| WireCredential {
                meta: record.meta.clone(),
                secret: if record.meta.is_live() {
                    record.secret.expose().to_owned()
                } else {
                    String::new()
                },
            })
            .collect(),
        references: content.references.clone(),
        trust: content.trust.clone(),
    };
    serde_json::to_vec(&wire)
        .map(Zeroizing::new)
        .map_err(|error| VaultStoreError::Encode {
            reason: error.to_string(),
        })
}

/// Reads the content back out of the bytes the cipher produced. The bytes are
/// wiped afterwards; the secrets move into [`SecretString`] values, which wipe
/// themselves when dropped.
pub(super) fn decode(bytes: &mut [u8]) -> Result<VaultContent, VaultStoreError> {
    let corrupt = |reason: String| VaultStoreError::Corrupt { reason };
    let parsed = (|| {
        let wire: WireBody = serde_json::from_slice(bytes).map_err(|error| {
            corrupt(format!(
                "the opened vault is not a document this build wrote: {error}"
            ))
        })?;
        wire.trust.validate().map_err(corrupt)?;
        let mut content = VaultContent::new();
        for key in wire.keys {
            content
                .insert_key(VaultKeyRecord::new(
                    key.meta,
                    SecretString::new(key.private_key),
                ))
                .map_err(|error| match error {
                    VaultStoreError::Invalid { reason } => corrupt(reason),
                    other => other,
                })?;
        }
        for credential in wire.credentials {
            content
                .insert_credential(VaultCredentialRecord::new(
                    credential.meta,
                    SecretString::new(credential.secret),
                ))
                .map_err(|error| match error {
                    VaultStoreError::Invalid { reason } => corrupt(reason),
                    other => other,
                })?;
        }
        content.set_references(&wire.references);
        content.trust = wire.trust;
        Ok(content)
    })();
    bytes.zeroize();
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;
    use shellune_core::VaultRecordVersion;

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
    fn a_deletion_keeps_the_metadata_as_a_tombstone_and_drops_the_body() {
        let id = KeyId::new();
        let mut content = VaultContent::new();
        content
            .insert_key(VaultKeyRecord::new(
                key(id, "abc123"),
                SecretString::new("-----BEGIN OPENSSH PRIVATE KEY-----"),
            ))
            .expect("inserts");
        assert!(
            content
                .key(id)
                .and_then(VaultKeyRecord::private_key)
                .is_some()
        );

        assert!(content.remove_key(id).expect("the metadata serializes"));
        let record = content.key(id).expect("the record stays");
        assert!(record.meta().deleted().is_some(), "deleted, not gone");
        assert!(
            record.private_key().is_none(),
            "a tombstone holds no private material"
        );
        assert!(
            !content.remove_key(id).expect("the metadata serializes"),
            "deleting twice changes nothing"
        );
    }

    #[test]
    fn a_credential_deletion_keeps_the_tombstone_and_wipes_the_password() {
        let id = CredentialId::new();
        let mut content = VaultContent::new();
        content
            .insert_credential(VaultCredentialRecord::new(
                credential(id),
                SecretString::new("hunter2"),
            ))
            .expect("inserts");

        assert!(content.remove_credential(id).expect("serializes"));
        let record = content.credential(id).expect("the record stays");
        assert!(record.meta().deleted().is_some());
        assert!(record.secret().is_none());

        assert!(
            matches!(
                content.update_credential(credential(id)),
                Err(VaultStoreError::Invalid { .. })
            ),
            "a deleted record is not editable"
        );
    }

    #[test]
    fn nameless_and_duplicate_records_are_refused() {
        let mut content = VaultContent::new();
        let nameless = ManagedKeyMeta::new(
            KeyId::new(),
            "   ",
            "ssh-ed25519",
            "ssh-ed25519 AAAAblank",
            "SHA256:blank",
            None,
            VaultRecordVersion::new(1, "device-a"),
        );
        assert!(
            content
                .insert_key(VaultKeyRecord::new(nameless, SecretString::new("k")))
                .is_err()
        );

        let id = KeyId::new();
        content
            .insert_key(VaultKeyRecord::new(key(id, "one"), SecretString::new("k")))
            .expect("inserts");
        assert!(
            content
                .insert_key(VaultKeyRecord::new(key(id, "one"), SecretString::new("k")))
                .is_err(),
            "the identifier is taken"
        );

        // A second record of the same public key sits in the content but not
        // in one catalog: the write is refused before it reaches a file.
        content
            .insert_key(VaultKeyRecord::new(
                key(KeyId::new(), "one"),
                SecretString::new("k"),
            ))
            .expect("a different identifier is not a duplicate entry");

        assert!(
            matches!(
                content.catalog().map_err(|error| error.to_string()),
                Err(reason) if reason.contains("fingerprint")
            ),
            "two records of one public key cannot sit in one catalog"
        );
    }

    #[test]
    fn the_reference_snapshot_matches_only_the_book_it_was_taken_from() {
        let mut first = host("Build");
        first.set_auth(Some(AuthMethodKind::ManagedKey));
        let key_id = KeyId::new();
        first.set_managed_key_id(Some(key_id));
        let mut second = host("Web");
        second.set_auth(Some(AuthMethodKind::Password));
        let credential_id = CredentialId::new();
        second.set_credential_id(Some(credential_id));

        let mut content = VaultContent::new();
        content
            .insert_key(VaultKeyRecord::new(
                key(key_id, "live"),
                SecretString::new("k"),
            ))
            .expect("inserts");
        content
            .insert_credential(VaultCredentialRecord::new(
                credential(credential_id),
                SecretString::new("hunter2"),
            ))
            .expect("inserts");
        let hosts = [first, second];
        content.set_references(&HostReference::snapshot(&hosts));
        content.validate_for(&hosts).expect("the mirror matches");

        // A stale mirror — one host lost from the book — is refused as a whole.
        let shorter = &hosts[..1];
        assert!(content.validate_for(shorter).is_err());

        // A reference without the method that reads it is the caller's mistake,
        // and storage refuses it even though the catalog alone would not notice:
        // set_auth would clear the reference, so the mismatch is built directly.
        let mut third = host("Db");
        third.set_auth(Some(AuthMethodKind::Agent));
        third.set_managed_key_id(Some(key_id));
        let mut mismatched = content.clone();
        let mismatched_hosts = [hosts[0].clone(), third];
        mismatched.set_references(&HostReference::snapshot(&mismatched_hosts));
        assert!(mismatched.validate_for(&mismatched_hosts).is_err());
    }

    #[test]
    fn the_body_round_trips_with_its_secrets_and_wipes_the_buffer() {
        let key_id = KeyId::new();
        let credential_id = CredentialId::new();
        let mut content = VaultContent::new();
        content
            .insert_key(VaultKeyRecord::new(
                key(key_id, "round"),
                SecretString::new("private-material"),
            ))
            .expect("inserts");
        content
            .insert_credential(VaultCredentialRecord::new(
                credential(credential_id),
                SecretString::new("hunter2"),
            ))
            .expect("inserts");
        let mut host = host("Build");
        host.set_auth(Some(AuthMethodKind::ManagedKey));
        host.set_managed_key_id(Some(key_id));
        content.set_references(&HostReference::snapshot(&[host]));
        let mut trust = HostTrust::new();
        let presented = crate::trust::HostKey::new(
            "ssh-ed25519",
            shellune_core::sha256_fingerprint(&[7; 32]).expect("digest"),
            "public-key",
        )
        .expect("identity");
        trust.accept("web", 22, &presented, 1).expect("accepted");
        content.set_trust(trust);

        let mut bytes = encode(&content).expect("encodes");
        assert!(
            std::str::from_utf8(&bytes)
                .expect("json")
                .contains("private-material"),
            "the plaintext holds the secret before it is sealed"
        );
        let back = decode(bytes.as_mut()).expect("decodes");
        assert!(
            bytes.iter().all(|byte| *byte == 0),
            "the plaintext buffer is wiped after reading"
        );
        assert_eq!(
            back.key(key_id)
                .and_then(VaultKeyRecord::private_key)
                .map(|secret| secret.expose().to_owned())
                .as_deref(),
            Some("private-material")
        );
        assert_eq!(
            back.credential(credential_id)
                .and_then(VaultCredentialRecord::secret)
                .map(|secret| secret.expose().to_owned())
                .as_deref(),
            Some("hunter2")
        );
        assert_eq!(back.references(), content.references());
        assert_eq!(back.trust().entries().len(), 1);

        // A deletion survives the sealed round trip: the tombstone is a record
        // and travels, while the password it guarded does not.
        let mut deleted = back;
        assert!(
            deleted
                .remove_credential(credential_id)
                .expect("serializes")
        );
        let mut bytes = encode(&deleted).expect("encodes");
        assert!(
            !std::str::from_utf8(&bytes)
                .expect("json")
                .contains("hunter2"),
            "a tombstone carries no password"
        );
        let reopened = decode(bytes.as_mut()).expect("decodes");
        let record = reopened
            .credential(credential_id)
            .expect("the tombstone travels");
        assert!(record.meta().deleted().is_some());
        assert!(record.secret().is_none());
    }
}
