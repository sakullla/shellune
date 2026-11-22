//! The encrypted vault: sealing, opening, tampering, snapshots, re-wrapping
//! and the migration out of a version 1 portable document (R2, R9).
//!
//! Every test works in its own scratch directory and with [`MemorySecrets`],
//! so the credential store of the machine running the tests is never touched
//! and no test needs a passphrase of a real user.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use shellune_core::{
    AuthMethodKind, CredentialId, CredentialMeta, HostConfig, HostId, KeyId, ManagedKeyMeta,
    SecretString, VaultRecordVersion,
};
use shellune_storage::{
    AppSecret, HostKey, HostReference, HostTrust, KdfParams, KeySource, MemorySecrets,
    PassphrasePurpose, Platform, SNAPSHOT_DIRECTORY, SecretStore, ShelluneData, UnsealSource,
    VAULT_FILE_NAME, VaultCipher, VaultContent, VaultCredentialRecord, VaultKeyRecord, VaultStore,
    VaultStoreError,
};

const WEBDAV_PASSWORD: &str = "the webdav account password";
const SYNC_PASSPHRASE: &str = "the one sync passphrase";

fn scratch(name: &str) -> PathBuf {
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "shellune-vault-{name}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Cheap enough for a debug build; the default cost is exercised by the
/// portable document tests.
fn cheap_kdf() -> KdfParams {
    KdfParams::new(64, 1, 1).expect("valid parameters")
}

fn key_meta(id: KeyId, fingerprint: &str) -> ManagedKeyMeta {
    ManagedKeyMeta::new(
        id,
        "Deploy key",
        "ssh-ed25519",
        format!("ssh-ed25519 AAAA{fingerprint} deploy@laptop"),
        format!("SHA256:{fingerprint}"),
        Some("deploy@laptop".to_owned()),
        VaultRecordVersion::new(1, "device-a"),
    )
}

fn credential_meta(id: CredentialId, name: &str) -> CredentialMeta {
    CredentialMeta::new(
        id,
        name,
        Some("deploy".to_owned()),
        VaultRecordVersion::new(2, "device-a"),
    )
}

/// A host that signs in with the managed key.
fn key_host(key: KeyId, name: &str) -> HostConfig {
    let mut host = HostConfig::new(name, "10.0.0.5", 22, "deploy").expect("host");
    host.set_auth(Some(AuthMethodKind::ManagedKey));
    host.set_managed_key_id(Some(key));
    host
}

/// A host that signs in with a shared credential.
fn password_host(credential: CredentialId, name: &str) -> HostConfig {
    let mut host = HostConfig::new(name, "10.0.0.6", 22, "deploy").expect("host");
    host.set_auth(Some(AuthMethodKind::Password));
    host.set_credential_id(Some(credential));
    host
}

fn trust_entry(byte: u8) -> HostKey {
    let fingerprint = shellune_core::sha256_fingerprint(&[byte; 32]).expect("a digest");
    HostKey::new("ssh-ed25519", fingerprint, format!("public-key-{byte}")).expect("a key")
}

/// A vault with one key, one shared credential, the hosts that use them and
/// one accepted identity. The hosts come along, because the reference mirror
/// is checked against exactly these records.
fn content() -> (VaultContent, Vec<HostConfig>) {
    let key_id = KeyId::new();
    let credential_id = CredentialId::new();
    let hosts = vec![
        key_host(key_id, "build"),
        password_host(credential_id, "web"),
    ];

    let mut vault = VaultContent::new();
    vault
        .insert_key(VaultKeyRecord::new(
            key_meta(key_id, "abc123"),
            SecretString::new("-----BEGIN OPENSSH PRIVATE KEY-----"),
        ))
        .expect("inserts");
    vault
        .insert_credential(VaultCredentialRecord::new(
            credential_meta(credential_id, "Prod login"),
            SecretString::new("hunter2"),
        ))
        .expect("inserts");
    let mut trust = HostTrust::new();
    trust
        .accept("10.0.0.5", 22, &trust_entry(1), 1_700_000_000)
        .expect("trusted");
    vault.set_trust(trust);
    vault.set_references(&HostReference::snapshot(&hosts));
    (vault, hosts)
}

fn file_text(store: &VaultStore) -> String {
    std::fs::read_to_string(store.path()).expect("the vault file is readable")
}

#[test]
fn a_local_machine_vault_round_trips_every_record_without_plaintext() {
    let root = scratch("local-round-trip");
    let store = VaultStore::at(&root);
    let secrets = MemorySecrets::new();
    let (vault, hosts) = content();
    let key_id = vault
        .keys()
        .next()
        .map(|record| record.meta().id())
        .expect("the key");
    let credential_id = vault
        .references()
        .iter()
        .find_map(|reference| reference.credential_id())
        .expect("the credential");
    store
        .save(
            &vault,
            &hosts,
            &[UnsealSource::LocalMachine],
            cheap_kdf(),
            &secrets,
        )
        .expect("saved");

    assert!(store.path().exists(), "vault.json is written");
    let text = file_text(&store);
    assert!(
        !text.contains("hunter2"),
        "no password in the clear: {text}"
    );
    assert!(
        !text.contains("OPENSSH PRIVATE KEY"),
        "no private material in the clear"
    );
    assert!(
        secrets
            .load_app(AppSecret::VaultLocalUnseal)
            .expect("the store works")
            .is_some(),
        "this machine now holds a local unseal secret"
    );

    let opened = store
        .open(&UnsealSource::LocalMachine, &secrets)
        .expect("the local secret opens the vault");
    assert_eq!(
        opened
            .key(key_id)
            .and_then(VaultKeyRecord::private_key)
            .map(|secret| secret.expose().to_owned())
            .as_deref(),
        Some("-----BEGIN OPENSSH PRIVATE KEY-----")
    );
    assert_eq!(
        opened
            .credential(credential_id)
            .and_then(VaultCredentialRecord::secret)
            .map(|secret| secret.expose().to_owned())
            .as_deref(),
        Some("hunter2")
    );
    assert_eq!(opened.references(), vault.references());
    assert_eq!(opened.trust().entries().len(), 1);

    // None of the passphrase secrets open a vault sealed without them.
    assert!(matches!(
        store.open(
            &UnsealSource::WebDavPassword(SecretString::new(WEBDAV_PASSWORD)),
            &secrets
        ),
        Err(VaultStoreError::Sealed)
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_vault_a_missing_local_secret_and_a_broken_store_are_named_separately() {
    let root = scratch("missing");
    let store = VaultStore::at(&root);
    assert!(!store.exists());
    assert_eq!(store.read_cipher(), Ok(None));
    assert!(matches!(
        store.open(&UnsealSource::LocalMachine, &MemorySecrets::new()),
        Err(VaultStoreError::Missing { .. })
    ));

    let (vault, hosts) = content();
    let writer = MemorySecrets::new();
    store
        .save(
            &vault,
            &hosts,
            &[UnsealSource::LocalMachine],
            cheap_kdf(),
            &writer,
        )
        .expect("saved");

    // Another machine's store holds no local secret for this vault; that is
    // "no entry", not a failure to read.
    assert!(matches!(
        store.open(&UnsealSource::LocalMachine, &MemorySecrets::new()),
        Err(VaultStoreError::NoLocalSecret)
    ));
    // A store that cannot be read is never reported as empty.
    assert!(matches!(
        store.open(&UnsealSource::LocalMachine, &MemorySecrets::unavailable()),
        Err(VaultStoreError::SystemStore { .. })
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_changed_file_is_refused_and_opening_never_rewrites_it() {
    let root = scratch("tampered");
    let store = VaultStore::at(&root);
    let secrets = MemorySecrets::new();
    let (vault, hosts) = content();
    store
        .save(
            &vault,
            &hosts,
            &[UnsealSource::LocalMachine],
            cheap_kdf(),
            &secrets,
        )
        .expect("saved");

    // A changed ciphertext byte fails the authentication tag; the file is
    // exactly as it was, because opening writes nothing.
    let mut document: serde_json::Value = serde_json::from_str(&file_text(&store)).expect("json");
    let ciphertext = document["cipher"]["ciphertext"]
        .as_str()
        .expect("base64")
        .to_owned();
    let mut bytes = ciphertext.into_bytes();
    bytes[0] = if bytes[0] == b'A' { b'B' } else { b'A' };
    document["cipher"]["ciphertext"] =
        serde_json::Value::String(String::from_utf8(bytes).expect("base64 text"));
    let tampered = serde_json::to_string_pretty(&document).expect("json");
    std::fs::write(store.path(), &tampered).expect("write");
    assert!(
        matches!(
            store.open(&UnsealSource::LocalMachine, &secrets),
            Err(VaultStoreError::Sealed)
        ),
        "a wrong secret and changed bytes fail alike"
    );
    assert_eq!(file_text(&store), tampered);

    // A file that is not a vault this build understands is not overwritten by
    // a save: the last ciphertext is kept even though it cannot be read.
    std::fs::write(
        store.path(),
        "{\"format\":\"shellune-vault\",\"version\":9}",
    )
    .expect("write");
    assert!(matches!(
        store.read_cipher(),
        Err(VaultStoreError::UnsupportedVersion { found: 9, .. })
    ));
    let before = file_text(&store);
    assert!(matches!(
        store.save(
            &vault,
            &hosts,
            &[UnsealSource::LocalMachine],
            cheap_kdf(),
            &secrets
        ),
        Err(VaultStoreError::UnsupportedVersion { .. })
    ));
    assert_eq!(
        file_text(&store),
        before,
        "the refused save changed nothing"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn saving_keeps_an_encrypted_copy_of_the_previous_ciphertext() {
    let root = scratch("snapshot");
    let store = VaultStore::at(&root);
    let secrets = MemorySecrets::new();
    let (mut vault, hosts) = content();
    let credential_id = vault
        .references()
        .iter()
        .find_map(|reference| reference.credential_id())
        .expect("the credential");
    store
        .save(
            &vault,
            &hosts,
            &[UnsealSource::LocalMachine],
            cheap_kdf(),
            &secrets,
        )
        .expect("first save");
    let first = std::fs::read(store.path()).expect("the ciphertext");

    // The credential's metadata changes; the vault is saved again.
    vault
        .update_credential(credential_meta(credential_id, "Prod login"))
        .expect("updates");
    store
        .save(
            &vault,
            &hosts,
            &[UnsealSource::LocalMachine],
            cheap_kdf(),
            &secrets,
        )
        .expect("second save");
    let second = std::fs::read(store.path()).expect("the new ciphertext");
    assert_ne!(first, second);

    let snapshots = root.join(SNAPSHOT_DIRECTORY);
    let copies: Vec<PathBuf> = std::fs::read_dir(&snapshots)
        .expect("the snapshot directory exists")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(VAULT_FILE_NAME))
        .collect();
    assert_eq!(copies.len(), 1, "one copy of the previous ciphertext");
    let copy = std::fs::read(&copies[0]).expect("the snapshot holds the vault file");
    assert_eq!(copy, first, "the copy is the previous ciphertext");
    assert!(
        !String::from_utf8_lossy(&copy).contains("hunter2"),
        "the copy is ciphertext, not records"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_four_secrets_seal_their_own_scenarios_and_a_rewrap_retires_the_old_one() {
    let root = scratch("four-secrets");
    let store = VaultStore::at(&root);
    let secrets = MemorySecrets::new();
    let (vault, hosts) = content();

    // All four scenarios in one ciphertext: this machine's own secret, the
    // WebDAV password, and the sync passphrase of a passwordless WebDAV.
    store
        .save(
            &vault,
            &hosts,
            &[
                UnsealSource::LocalMachine,
                UnsealSource::WebDavPassword(SecretString::new(WEBDAV_PASSWORD)),
                UnsealSource::SyncPassphrase(SecretString::new(SYNC_PASSPHRASE)),
            ],
            cheap_kdf(),
            &secrets,
        )
        .expect("saved");
    let vault_id = store
        .read_cipher()
        .expect("reads")
        .expect("present")
        .vault_id()
        .to_owned();

    for source in [
        UnsealSource::LocalMachine,
        UnsealSource::WebDavPassword(SecretString::new(WEBDAV_PASSWORD)),
        UnsealSource::SyncPassphrase(SecretString::new(SYNC_PASSPHRASE)),
    ] {
        let opened = store.open(&source, &secrets).expect("each source opens");
        assert_eq!(opened.credentials().count(), 1);
    }
    assert!(matches!(
        store.open(
            &UnsealSource::WebDavPassword(SecretString::new("wrong password")),
            &secrets
        ),
        Err(VaultStoreError::Sealed)
    ));
    assert!(matches!(
        store.open(
            &UnsealSource::SyncPassphrase(SecretString::new("wrong passphrase")),
            &secrets
        ),
        Err(VaultStoreError::Sealed)
    ));

    // The WebDAV password changes: the vault is re-sealed for the new one, the
    // identity stays, and the old password is gone with its wrap.
    store
        .save(
            &vault,
            &hosts,
            &[UnsealSource::WebDavPassword(SecretString::new(
                "a newer password",
            ))],
            cheap_kdf(),
            &secrets,
        )
        .expect("re-wrapped");
    assert_eq!(
        store
            .read_cipher()
            .expect("reads")
            .expect("present")
            .vault_id(),
        vault_id,
        "a re-save keeps the identity the wraps are bound to"
    );
    assert!(
        matches!(
            store.open(
                &UnsealSource::WebDavPassword(SecretString::new(WEBDAV_PASSWORD)),
                &secrets
            ),
            Err(VaultStoreError::Sealed)
        ),
        "the old password opened its wrap, which is gone"
    );
    store
        .open(
            &UnsealSource::WebDavPassword(SecretString::new("a newer password")),
            &secrets,
        )
        .expect("the new password opens the vault");
    assert!(
        matches!(
            store.open(&UnsealSource::LocalMachine, &secrets),
            Err(VaultStoreError::Sealed)
        ),
        "the local wrap was dropped with the re-wrap"
    );

    // An empty passphrase is refused before anything is written.
    let before = std::fs::read(store.path()).expect("the ciphertext");
    assert_eq!(
        store.save(
            &vault,
            &hosts,
            &[UnsealSource::SyncPassphrase(SecretString::new("   "))],
            cheap_kdf(),
            &secrets,
        ),
        Err(VaultStoreError::EmptySecret {
            which: "sync passphrase"
        })
    );
    assert_eq!(
        std::fs::read(store.path()).expect("the ciphertext"),
        before,
        "the refused save changed nothing"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_local_export_copy_needs_the_sync_passphrase_and_shares_no_ciphertext() {
    let (vault, _) = content();
    let copy = VaultCipher::export_copy(&vault, &SecretString::new(SYNC_PASSPHRASE), cheap_kdf())
        .expect("sealed");
    let opens = |passphrase: &str| {
        copy.open(&KeySource::Passphrase {
            purpose: PassphrasePurpose::SyncPassphrase,
            value: SecretString::new(passphrase),
        })
    };
    assert!(opens(SYNC_PASSPHRASE).is_ok());

    // The WebDAV password is not the export secret, and neither is an empty
    // passphrase: the export never falls back to a weaker key.
    assert!(matches!(
        opens(WEBDAV_PASSWORD),
        Err(VaultStoreError::Sealed)
    ));
    assert!(matches!(
        opens("   "),
        Err(VaultStoreError::EmptySecret { .. })
    ));
    let wrong_purpose = copy.open(&KeySource::Passphrase {
        purpose: PassphrasePurpose::WebDavPassword,
        value: SecretString::new(SYNC_PASSPHRASE),
    });
    assert!(
        matches!(wrong_purpose, Err(VaultStoreError::Sealed)),
        "the right passphrase under the wrong purpose stays shut"
    );

    let again = VaultCipher::export_copy(&vault, &SecretString::new(SYNC_PASSPHRASE), cheap_kdf())
        .expect("sealed again");
    assert_ne!(
        copy, again,
        "the same content sealed twice shares no ciphertext"
    );
}

#[test]
fn a_write_the_host_book_contradicts_is_refused_before_anything_happens() {
    let root = scratch("invalid");
    let store = VaultStore::at(&root);
    let secrets = MemorySecrets::new();
    let (vault, hosts) = content();
    let credential_id = vault
        .references()
        .iter()
        .find_map(|reference| reference.credential_id())
        .expect("the credential");

    // A host points at a key the vault does not hold.
    let missing = key_host(KeyId::new(), "stranger");
    assert!(matches!(
        store.save(
            &vault,
            &[missing],
            &[UnsealSource::LocalMachine],
            cheap_kdf(),
            &secrets
        ),
        Err(VaultStoreError::Invalid { .. })
    ));
    assert!(!store.exists(), "no file was written");
    assert!(
        secrets
            .load_app(AppSecret::VaultLocalUnseal)
            .expect("the store works")
            .is_none(),
        "and no local unseal secret was created for it"
    );

    // A stale mirror: the hosts reference records, the mirror says none.
    let mut stale = vault.clone();
    stale.set_references(&[]);
    assert!(matches!(
        store.save(
            &stale,
            &hosts,
            &[UnsealSource::LocalMachine],
            cheap_kdf(),
            &secrets
        ),
        Err(VaultStoreError::Invalid { .. })
    ));

    // A deleted record is not a reference a host may keep.
    let mut deleted = vault.clone();
    deleted
        .remove_credential(credential_id)
        .expect("the record exists");
    assert!(matches!(
        store.save(
            &deleted,
            &hosts,
            &[UnsealSource::LocalMachine],
            cheap_kdf(),
            &secrets
        ),
        Err(VaultStoreError::Invalid { .. })
    ));
    assert!(!store.exists(), "still nothing was written");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_failing_system_store_never_costs_the_last_ciphertext() {
    let root = scratch("store-failure");
    let store = VaultStore::at(&root);
    let (vault, hosts) = content();
    let secrets = MemorySecrets::new();
    store
        .save(
            &vault,
            &hosts,
            &[UnsealSource::LocalMachine],
            cheap_kdf(),
            &secrets,
        )
        .expect("saved");
    let before = std::fs::read(store.path()).expect("the ciphertext");

    let broken = MemorySecrets::unavailable();
    assert!(matches!(
        store.save(
            &vault,
            &hosts,
            &[UnsealSource::LocalMachine],
            cheap_kdf(),
            &broken
        ),
        Err(VaultStoreError::SystemStore { .. })
    ));
    assert_eq!(
        std::fs::read(store.path()).expect("the ciphertext"),
        before,
        "the last good file survives the failing store"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A version 1 machine: a host with a remembered password, a host with a
/// remembered key passphrase, and one accepted identity, exported with the
/// sealed password block the version 1 format carries.
fn version_one_document(secrets: &MemorySecrets) -> (ShelluneData, HostId, HostId) {
    use shellune_storage::{ExportOptions, HostBook};

    let mut hosts = HostBook::new();
    let mut web = HostConfig::new("web", "10.0.0.5", 22, "deploy").expect("host");
    web.set_auth(Some(AuthMethodKind::Password));
    web.set_remember_secret(true);
    let web_id = web.id();
    hosts.insert(web).expect("inserted");
    let mut db = HostConfig::new("db", "10.0.0.6", 22, "postgres").expect("host");
    db.set_auth(Some(AuthMethodKind::EncryptedKeyFile));
    db.set_remember_secret(true);
    let db_id = db.id();
    hosts.insert(db).expect("inserted");
    secrets
        .save(web_id, &SecretString::new("web-password"))
        .expect("saved");
    secrets
        .save(db_id, &SecretString::new("key-passphrase"))
        .expect("saved");
    let mut known_hosts = HostTrust::new();
    known_hosts
        .accept("10.0.0.5", 22, &trust_entry(1), 1_700_000_000)
        .expect("trusted");

    let state = shellune_storage::LocalState {
        hosts,
        known_hosts,
        ..shellune_storage::LocalState::default()
    };
    let mut options = ExportOptions::new(Platform::Macos);
    options.kdf = cheap_kdf();
    options.created_unix = 1_800_000_000;
    let outcome = ShelluneData::export(
        &state,
        &options.with_passphrase(SecretString::new(SYNC_PASSPHRASE)),
        secrets,
        None,
    )
    .expect("exported");
    (outcome.document, web_id, db_id)
}

#[test]
fn a_v1_document_migrates_read_only_and_a_retry_cannot_double_it() {
    let root = scratch("migrate");
    let store = VaultStore::at(&root);
    let secrets = MemorySecrets::new();
    let (document, web_id, db_id) = version_one_document(&secrets);
    let document_path = root.join("backup-v1.json");
    std::fs::create_dir_all(&root).expect("directory");
    let document_bytes = document.to_json().expect("json");
    std::fs::write(&document_path, &document_bytes).expect("write");

    let hosts: Vec<HostConfig> = document.shared().hosts().hosts().to_vec();
    let report = store
        .migrate_from_v1(
            &document,
            Some(&SecretString::new(SYNC_PASSPHRASE)),
            &hosts,
            "device-a",
            &secrets,
        )
        .expect("migrated");
    assert_eq!(report.credentials, 2, "one record per travelling password");
    assert_eq!(report.trust_entries, 1);

    let opened = store
        .open(&UnsealSource::LocalMachine, &secrets)
        .expect("the new vault opens");
    let names: Vec<&str> = opened
        .credentials()
        .map(|record| record.meta().name())
        .collect();
    assert!(names.contains(&"web") && names.contains(&"db"), "{names:?}");
    let secrets_of: Vec<String> = {
        let mut values: Vec<String> = opened
            .credentials()
            .filter_map(|record| record.secret().map(|secret| secret.expose().to_owned()))
            .collect();
        values.sort();
        values
    };
    assert_eq!(
        secrets_of,
        vec!["key-passphrase".to_owned(), "web-password".to_owned()]
    );
    assert_eq!(opened.trust().entries()[0].host(), "10.0.0.5");
    assert!(
        opened.references().is_empty(),
        "binding hosts to the new records is the application's next step"
    );

    // Read-only for everything else: the source file, the old system
    // passwords.
    assert_eq!(
        std::fs::read(&document_path).expect("read"),
        document_bytes,
        "the source document is untouched"
    );
    assert_eq!(
        secrets
            .load(web_id)
            .expect("read")
            .map(|s| s.expose().to_owned()),
        Some("web-password".to_owned()),
        "the old system password is kept by default"
    );
    assert_eq!(
        secrets
            .load(db_id)
            .expect("read")
            .map(|s| s.expose().to_owned()),
        Some("key-passphrase".to_owned())
    );

    // A retry cannot double the records: the vault exists.
    assert_eq!(
        store.migrate_from_v1(
            &document,
            Some(&SecretString::new(SYNC_PASSPHRASE)),
            &hosts,
            "device-a",
            &secrets,
        ),
        Err(VaultStoreError::AlreadyExists { path: store.path() })
    );
    assert_eq!(
        store
            .open(&UnsealSource::LocalMachine, &secrets)
            .expect("opens")
            .credentials()
            .count(),
        2
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_failed_migration_writes_no_vault_no_secret_and_no_half_record() {
    let root = scratch("failed-migration");
    let store = VaultStore::at(&root);
    let secrets = MemorySecrets::new();
    let (document, _, _) = version_one_document(&secrets);
    let hosts: Vec<HostConfig> = document.shared().hosts().hosts().to_vec();

    // The wrong passphrase fails and leaves nothing behind.
    assert_eq!(
        store.migrate_from_v1(
            &document,
            Some(&SecretString::new("not the passphrase")),
            &hosts,
            "device-a",
            &secrets,
        ),
        Err(VaultStoreError::Sealed)
    );
    assert!(!store.exists(), "no vault file was written");
    assert!(
        secrets
            .load_app(AppSecret::VaultLocalUnseal)
            .expect("the store works")
            .is_none(),
        "no local unseal secret was stored"
    );

    // A missing passphrase for a document that carries passwords fails too.
    assert!(matches!(
        store.migrate_from_v1(&document, None, &hosts, "device-a", &secrets),
        Err(VaultStoreError::EmptySecret { .. })
    ));

    // A retry with the right passphrase starts from nothing and lands whole.
    store
        .migrate_from_v1(
            &document,
            Some(&SecretString::new(SYNC_PASSPHRASE)),
            &hosts,
            "device-a",
            &secrets,
        )
        .expect("the retry migrates");
    assert_eq!(
        store
            .open(&UnsealSource::LocalMachine, &secrets)
            .expect("opens")
            .credentials()
            .count(),
        2,
        "no record was doubled by the failed first attempt"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_version_two_document_is_not_migrated_and_a_v1_document_without_passwords_moves_trust() {
    let root = scratch("not-v2");
    let store = VaultStore::at(&root);
    let secrets = MemorySecrets::new();

    // A version 2 document is not the version 1 format that is migrated.
    let (vault, _) = content();
    let cipher = VaultCipher::export_copy(&vault, &SecretString::new(SYNC_PASSPHRASE), cheap_kdf())
        .expect("sealed");
    let mut state = shellune_storage::LocalState::default();
    let mut trust = HostTrust::new();
    trust
        .accept("10.0.0.5", 22, &trust_entry(2), 1_700_000_000)
        .expect("trusted");
    state.known_hosts = trust;
    let mut options = shellune_storage::ExportOptions::new(Platform::Linux);
    options.kdf = cheap_kdf();
    let version_two =
        ShelluneData::export(&state, &options.clone().with_vault(cipher), &secrets, None)
            .expect("exported")
            .document;
    assert!(matches!(
        store.migrate_from_v1(&version_two, None, &[], "device-a", &secrets),
        Err(VaultStoreError::Invalid { .. })
    ));
    assert!(!store.exists());

    // A version 1 document without a password block still moves its trust.
    let plain = ShelluneData::export(&state, &options, &secrets, None)
        .expect("exported")
        .document;
    let report = store
        .migrate_from_v1(&plain, None, &[], "device-a", &secrets)
        .expect("migrated");
    assert_eq!(report.credentials, 0);
    assert_eq!(report.trust_entries, 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_vault_file_of_another_kind_of_document_is_named_as_such() {
    let root = scratch("foreign");
    let store = VaultStore::at(&root);
    std::fs::create_dir_all(&root).expect("directory");
    std::fs::write(store.path(), "{ not json").expect("write");
    assert!(matches!(
        store.read_cipher(),
        Err(VaultStoreError::Corrupt { .. })
    ));
    std::fs::write(store.path(), r#"{"format":"shellune-data","version":1}"#).expect("write");
    assert!(matches!(
        store.read_cipher(),
        Err(VaultStoreError::Corrupt { .. })
    ));
    let _ = std::fs::remove_dir_all(&root);
}
