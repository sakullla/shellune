//! The portable data file: export, read back, preview and apply, across machines.
//!
//! Every test works in its own scratch directory and with [`MemorySecrets`], so the
//! credential store of the machine running the tests is never touched.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use shellune_core::{
    AuthMethodKind, ConnectionRecord, Endpoint, ForwardBook, ForwardRule, GroupBook, GroupPath,
    HostConfig, HostId, SecretString, SerialConfig, SnippetBook,
};
use shellune_storage::{
    ApplyMode, ApplyPlan, ExportOptions, GroupStore, HostBook, HostKey, HostSecretState, HostStore,
    HostTrust, KdfParams, KeymapEntry, KeymapPreferences, LocalData, LocalState, MemorySecrets,
    PORTABLE_VAULT_VERSION, PORTABLE_VERSION, Platform, PlatformField, PortableError, Preferences,
    PreferencesStore, SNAPSHOT_LIMIT, ShelluneData, TrustConflict, UnsealSource, VaultCipher,
    VaultContent, VaultCredentialRecord, VaultStore, merge_recent_connections,
};

const PASSWORD: &str = "correct-horse-battery-staple";
const PASSPHRASE: &str = "a passphrase of my own";
const DEVICE: &str = "device-a";

fn scratch(name: &str) -> PathBuf {
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "shellune-portable-{name}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Cheap enough for a debug build; the default cost is exercised once.
fn cheap_kdf() -> KdfParams {
    KdfParams::new(64, 1, 1).expect("valid parameters")
}

fn options(platform: Platform) -> ExportOptions {
    let mut options = ExportOptions::new(platform);
    options.kdf = cheap_kdf();
    options.created_unix = 1_800_000_000;
    options
}

fn with_passphrase(platform: Platform) -> ExportOptions {
    options(platform).with_passphrase(SecretString::new(PASSPHRASE))
}

fn trust_key(byte: u8) -> HostKey {
    let fingerprint = shellune_core::sha256_fingerprint(&[byte; 32]).expect("a digest");
    HostKey::new("ssh-ed25519", fingerprint, format!("public-key-{byte}")).expect("a key")
}

struct Fixture {
    state: LocalState,
    password_host: HostId,
    key_host: HostId,
    serial: HostId,
}

/// A machine with one of everything: a host with a remembered password, a host
/// with a key file and a remembered passphrase, a serial port, a group, a rule, a
/// saved command, a trusted identity and some preferences.
fn windows_machine() -> Fixture {
    let mut hosts = HostBook::new();
    let mut web = HostConfig::new("web", "10.0.0.5", 22, "deploy").expect("host");
    web.set_auth(Some(AuthMethodKind::Password));
    web.set_remember_secret(true);
    web.set_group_path(Some(GroupPath::parse("Prod").expect("path")));
    let password_host = web.id();
    hosts.insert(web).expect("inserted");

    let mut db = HostConfig::new("db", "10.0.0.6", 22, "postgres").expect("host");
    db.set_auth(Some(AuthMethodKind::EncryptedKeyFile));
    db.set_identity_file(Some(r"C:\Users\me\.ssh\db_ed25519"));
    db.set_remember_secret(true);
    let key_host = db.id();
    hosts.insert(db).expect("inserted");

    let serial = SerialConfig::new("console", "COM3").expect("serial");
    let serial_id = serial.id();
    hosts
        .insert_connection(ConnectionRecord::Serial(serial))
        .expect("inserted");
    hosts
        .record_connection(password_host, 1_700_000_100)
        .expect("recorded");
    hosts.set_favorite(key_host, true).expect("marked");

    let groups = GroupBook::from_paths([GroupPath::parse("Prod").expect("path")]).expect("tree");
    let mut forwards = ForwardBook::new();
    forwards
        .insert(ForwardRule::local(
            password_host,
            Endpoint::loopback(15432).expect("endpoint"),
            Endpoint::new("db.internal", 5432).expect("endpoint"),
        ))
        .expect("rule");
    let mut snippets = SnippetBook::new();
    snippets.create("disk", "df -h").expect("snippet");
    let mut known_hosts = HostTrust::new();
    known_hosts
        .accept("10.0.0.5", 22, &trust_key(1), 1_700_000_000)
        .expect("trusted");

    let mut keymap = KeymapPreferences::default();
    keymap.set_for_platform(
        "windows",
        vec![KeymapEntry::new(
            "palette.open",
            "shell",
            Some("ctrl-alt-p".to_owned()),
        )],
    );
    let preferences = Preferences {
        theme: Some("dark".to_owned()),
        language: Some("zh".to_owned()),
        text_size: Some(115),
        terminal_font_size: Some(15.0),
        keymap: Some(keymap),
        ..Preferences::default()
    };

    Fixture {
        state: LocalState {
            hosts,
            groups,
            forwards,
            snippets,
            known_hosts,
            preferences,
        },
        password_host,
        key_host,
        serial: serial_id,
    }
}

/// The vault of the machine of `fixture`: both remembered passwords, as the
/// application keeps them.
fn vault_for(fixture: &Fixture) -> VaultContent {
    let mut vault = VaultContent::new();
    vault.set_host_secret(fixture.password_host, SecretString::new(PASSWORD), DEVICE);
    vault.set_host_secret(
        fixture.key_host,
        SecretString::new("key-passphrase-123"),
        DEVICE,
    );
    vault
}

/// The live password the vault holds for host.
fn password_in(vault: &VaultContent, host: HostId) -> Option<String> {
    vault
        .host_secret(host)
        .map(|secret| secret.expose().to_owned())
}

/// A commit that keeps the vault in memory, for an apply that only needs the
/// files written.
fn keep(_: &VaultContent) -> Result<(), String> {
    Ok(())
}

/// The same data on a machine whose credential store holds no password for it.
fn without_saved_passwords(state: &LocalState) -> LocalState {
    let mut state = state.clone();
    for record in state.hosts.records() {
        if let ConnectionRecord::Ssh(mut host) = record
            && host.remembers_secret()
        {
            let id = host.id();
            host.set_remember_secret(false);
            state.hosts.update(host, id).expect("updated");
        }
    }
    state
}

fn save_state(root: &Path, state: &LocalState) {
    HostStore::at(root).save(&state.hosts).expect("hosts");
    GroupStore::at(root).save(&state.groups).expect("groups");
    shellune_storage::ForwardStore::at(root)
        .save(&state.forwards)
        .expect("forwards");
    shellune_storage::SnippetStore::at(root)
        .save(&state.snippets)
        .expect("snippets");
    shellune_storage::TrustStore::at(root)
        .save(&state.known_hosts)
        .expect("trust");
    PreferencesStore::at(root)
        .save(&state.preferences)
        .expect("preferences");
}

/// Every file in a directory tree, with its bytes, for "nothing changed" checks.
fn tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return files;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            files.extend(tree(&path));
        } else {
            files.push((path.clone(), std::fs::read(&path).expect("readable")));
        }
    }
    files.sort();
    files
}

#[test]
fn a_document_round_trips_through_json_and_onto_another_machine() {
    let fixture = windows_machine();
    let outcome =
        ShelluneData::export_from_vault(&fixture.state, &options(Platform::Windows), None, None)
            .expect("exported");
    let document = outcome.document;
    assert!(!document.has_secrets());
    assert_eq!(document.source_platform(), "windows");
    assert_eq!(document.version(), 1);
    assert_eq!(document.created_unix(), 1_800_000_000);

    let bytes = document.to_json().expect("encoded");
    let text = String::from_utf8(bytes.clone()).expect("utf-8");
    assert!(text.contains("\"format\": \"shellune-data\""), "{text}");
    let read = ShelluneData::from_json(&bytes).expect("read back");
    assert_eq!(read, document);

    // A fresh Windows machine ends up with the same data, except that no record
    // remembers a password: none was saved in its credential store.
    let root = scratch("round-trip");
    let data = LocalData::at(&root);
    let empty = data.load().expect("an empty directory is empty data");
    assert!(empty.is_empty());
    let plan = read
        .plan(&empty, Platform::Windows, ApplyMode::Replace, None)
        .expect("planned");
    assert_eq!(plan.hosts().added, 3);
    assert_eq!(plan.groups().added, 1);
    assert_eq!(plan.forwards().added, 1);
    assert_eq!(plan.snippets().added, 1);
    assert_eq!(plan.known_hosts().added, 1);
    assert!(plan.preferences_changed());
    assert!(plan.fallbacks().is_empty(), "{:?}", plan.fallbacks());
    let applied = plan.apply(&data, "import").expect("applied");
    assert_eq!(applied.files_written.len(), 6);
    let loaded = data.load().expect("loads");
    let expected = without_saved_passwords(&fixture.state);
    assert_eq!(loaded, expected);
    assert_eq!(applied.state, expected);
    assert!(loaded.hosts.is_favorite(fixture.key_host));
    assert_eq!(loaded.hosts.recents().len(), 1);

    // Applying the same document again changes nothing and writes nothing.
    let again = read
        .plan(&loaded, Platform::Windows, ApplyMode::Merge, None)
        .expect("planned");
    assert!(again.is_empty(), "{again:?}");
    let before = tree(&root);
    let outcome = again.apply(&data, "import").expect("nothing to do");
    assert!(outcome.snapshot.is_none());
    assert_eq!(tree(&root), before);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_document_without_passwords_holds_no_password_at_all() {
    let fixture = windows_machine();
    let secrets = vault_for(&fixture);
    let outcome = ShelluneData::export_from_vault(
        &fixture.state,
        &options(Platform::Windows),
        Some(&secrets),
        None,
    )
    .expect("exported");
    assert!(
        outcome.secrets_included.is_empty(),
        "the store is not read without a passphrase"
    );
    let text = String::from_utf8(outcome.document.to_json().expect("encoded")).expect("utf-8");
    assert!(!text.contains(PASSWORD), "{text}");
    assert!(!text.contains("key-passphrase-123"), "{text}");
    assert!(!text.contains("\"secrets\""), "{text}");
}

#[test]
fn sealed_passwords_are_restored_into_the_credential_store_with_the_right_passphrase() {
    let fixture = windows_machine();
    let secrets = vault_for(&fixture);
    let outcome = ShelluneData::export_from_vault(
        &fixture.state,
        &with_passphrase(Platform::Windows),
        Some(&secrets),
        None,
    )
    .expect("exported");
    assert_eq!(outcome.secrets_included.len(), 2);
    assert!(outcome.secrets_unreadable.is_empty());
    assert!(outcome.secrets_missing.is_empty());
    let bytes = outcome.document.to_json().expect("encoded");
    let text = String::from_utf8(bytes.clone()).expect("utf-8");
    assert!(!text.contains(PASSWORD), "the password is sealed: {text}");
    assert!(!text.contains("key-passphrase-123"));
    assert!(
        !text.contains(PASSPHRASE),
        "the passphrase is never written"
    );
    assert!(text.contains("argon2id") && text.contains("aes-256-gcm"));

    let read = ShelluneData::from_json(&bytes).expect("read back");
    assert!(read.has_secrets());
    assert_eq!(read.secrets_kdf(), Some(cheap_kdf()));
    let bundle = read
        .open_secrets(&SecretString::new(PASSPHRASE))
        .expect("opened");
    assert_eq!(bundle.len(), 2);
    assert_eq!(
        bundle.get(fixture.password_host).map(SecretString::expose),
        Some(PASSWORD)
    );

    // The new machine's records remember their passwords only once they are saved.
    let mut forgetful = fixture.state.clone();
    let mut web = forgetful
        .hosts
        .host(fixture.password_host)
        .expect("saved")
        .clone();
    web.set_remember_secret(false);
    forgetful
        .hosts
        .update(web, fixture.password_host)
        .expect("updated");
    let root = scratch("secrets");
    let data = LocalData::at(&root);
    save_state(&root, &forgetful);
    let target = MemorySecrets::new();
    let plan = read
        .plan(
            &data.load().expect("loads"),
            Platform::Windows,
            ApplyMode::Merge,
            Some(&bundle),
        )
        .expect("planned");
    assert_eq!(plan.secrets_to_import(), 2);
    // The passwords go into the vault, and the vault is saved as part of the
    // apply: the credential store is asked for the vault's own unseal secret
    // and for no host's entry (R4).
    let store = VaultStore::at(&root);
    let mut vault = VaultContent::new();
    let applied = plan
        .apply_with_vault(&data, &mut vault, DEVICE, "import", &mut |content| {
            store
                .save_records(
                    content,
                    &plan.result().hosts.records(),
                    &[UnsealSource::LocalMachine],
                    cheap_kdf(),
                    &target,
                )
                .map_err(|error| error.to_string())
        })
        .expect("applied");
    assert_eq!(applied.secrets_saved, 2);
    let opened = store
        .open(&UnsealSource::LocalMachine, &target)
        .expect("the new vault opens");
    assert_eq!(
        password_in(&opened, fixture.password_host).as_deref(),
        Some(PASSWORD)
    );
    assert!(
        target
            .accesses()
            .iter()
            .all(|access| !access.is_host_entry()),
        "{:?}",
        target.accesses()
    );
    assert!(target.is_empty(), "no host-<uuid> entry was written");
    let loaded = data.load().expect("loads");
    assert!(
        loaded
            .hosts
            .host(fixture.password_host)
            .expect("saved")
            .remembers_secret()
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_default_key_derivation_seals_and_opens() {
    let fixture = windows_machine();
    let secrets = vault_for(&fixture);
    let mut options = with_passphrase(Platform::Windows);
    options.kdf = KdfParams::default();
    let document = ShelluneData::export_from_vault(&fixture.state, &options, Some(&secrets), None)
        .expect("exported")
        .document;
    assert_eq!(document.secrets_kdf(), Some(KdfParams::default()));
    let bundle = document
        .open_secrets(&SecretString::new(PASSPHRASE))
        .expect("opened");
    assert_eq!(bundle.len(), 2);
}

#[test]
fn a_wrong_passphrase_or_changed_bytes_fail_alike_and_write_nothing() {
    let fixture = windows_machine();
    let secrets = vault_for(&fixture);
    let document = ShelluneData::export_from_vault(
        &fixture.state,
        &with_passphrase(Platform::Windows),
        Some(&secrets),
        None,
    )
    .expect("exported")
    .document;
    let root = scratch("wrong-passphrase");
    let backup = root.join("backup.json");
    document.write_to(&backup).expect("written");
    let before = tree(&root);

    let read = ShelluneData::read(&backup).expect("read");
    let wrong = read
        .open_secrets(&SecretString::new("not the passphrase"))
        .expect_err("refused");
    assert_eq!(wrong, PortableError::WrongPassphrase);
    assert_eq!(
        wrong.to_string(),
        "the passphrase is wrong or the file is damaged"
    );
    assert_eq!(
        read.open_secrets(&SecretString::new("  "))
            .expect_err("refused"),
        PortableError::EmptyPassphrase
    );

    // Flip one byte of the sealed passwords: same answer as a wrong passphrase.
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&backup).expect("bytes")).expect("json");
    let ciphertext = value["secrets"]["ciphertext"]
        .as_str()
        .expect("ciphertext")
        .to_owned();
    let mut chars: Vec<char> = ciphertext.chars().collect();
    chars[4] = if chars[4] == 'A' { 'B' } else { 'A' };
    value["secrets"]["ciphertext"] = serde_json::Value::String(chars.into_iter().collect());
    let tampered = ShelluneData::from_json(&serde_json::to_vec(&value).expect("json"))
        .expect("the shape is still valid");
    assert_eq!(
        tampered
            .open_secrets(&SecretString::new(PASSPHRASE))
            .expect_err("refused"),
        PortableError::WrongPassphrase
    );
    assert_eq!(tree(&root), before, "nothing was written");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn passwords_the_vault_does_not_hold_are_listed_and_no_host_entry_is_read() {
    let fixture = windows_machine();
    // A vault that holds one of the two passwords: the other host was never
    // migrated, and its old system-store entry is not read to fill the gap.
    let legacy = MemorySecrets::new();
    legacy.seed_legacy_host(fixture.key_host, &SecretString::new("key-passphrase-123"));
    let mut partial = VaultContent::new();
    partial.set_host_secret(fixture.password_host, SecretString::new(PASSWORD), DEVICE);
    let outcome = ShelluneData::export_from_vault(
        &fixture.state,
        &with_passphrase(Platform::Windows),
        Some(&partial),
        None,
    )
    .expect("exported");
    assert_eq!(outcome.secrets_included, vec![fixture.password_host]);
    assert_eq!(outcome.secrets_missing, vec![fixture.key_host]);
    assert!(outcome.secrets_unreadable.is_empty());
    assert!(legacy.accesses().is_empty(), "{:?}", legacy.accesses());

    // Without any vault, every remembered password is missing.
    let outcome = ShelluneData::export_from_vault(
        &fixture.state,
        &with_passphrase(Platform::Windows),
        None,
        None,
    )
    .expect("the export still succeeds");
    assert!(outcome.secrets_included.is_empty());
    assert_eq!(
        outcome.secrets_missing,
        vec![fixture.password_host, fixture.key_host]
    );

    let empty = ShelluneData::export_from_vault(
        &fixture.state,
        &options(Platform::Windows).with_passphrase(SecretString::new("")),
        Some(&partial),
        None,
    )
    .expect_err("an empty passphrase is refused");
    assert_eq!(empty, PortableError::EmptyPassphrase);
}

#[test]
fn the_compatibility_export_reads_old_entries_only_with_a_passphrase() {
    let fixture = windows_machine();
    let unavailable = MemorySecrets::unavailable();
    let outcome = ShelluneData::export(
        &fixture.state,
        &with_passphrase(Platform::Windows),
        &unavailable,
        None,
    )
    .expect("the export still succeeds");
    assert!(outcome.secrets_included.is_empty());
    assert_eq!(
        outcome.secrets_unreadable,
        vec![fixture.password_host, fixture.key_host]
    );
    assert!(outcome.secrets_missing.is_empty());

    let partial = MemorySecrets::new();
    partial.seed_legacy_host(fixture.password_host, &SecretString::new(PASSWORD));
    let outcome = ShelluneData::export(
        &fixture.state,
        &with_passphrase(Platform::Windows),
        &partial,
        None,
    )
    .expect("exported");
    assert_eq!(outcome.secrets_included, vec![fixture.password_host]);
    assert_eq!(outcome.secrets_missing, vec![fixture.key_host]);

    partial.clear_accesses();
    ShelluneData::export(&fixture.state, &options(Platform::Windows), &partial, None)
        .expect("exported");
    assert!(
        partial.accesses().is_empty(),
        "without a passphrase the store is not read"
    );
}

#[test]
fn machine_specific_fields_survive_a_trip_between_windows_and_linux() {
    let fixture = windows_machine();
    let from_windows =
        ShelluneData::export_from_vault(&fixture.state, &options(Platform::Windows), None, None)
            .expect("exported")
            .document;

    // A Linux machine that never saw these records: the Windows paths are kept
    // and reported, the Linux key bindings are left alone.
    let linux_root = scratch("linux");
    let linux = LocalData::at(&linux_root);
    let mut linux_keymap = KeymapPreferences::default();
    linux_keymap.set_for_platform(
        "linux",
        vec![KeymapEntry::new(
            "terminal.copy",
            "terminal",
            Some("ctrl-shift-c".to_owned()),
        )],
    );
    PreferencesStore::at(&linux_root)
        .save(&Preferences {
            text_size: Some(100),
            keymap: Some(linux_keymap.clone()),
            ..Preferences::default()
        })
        .expect("preferences");
    let plan = from_windows
        .plan(
            &linux.load().expect("loads"),
            Platform::Linux,
            ApplyMode::Merge,
            None,
        )
        .expect("planned");
    assert!(!plan.keymap_changed());
    let mut fields: Vec<(HostId, PlatformField, String)> = plan
        .fallbacks()
        .iter()
        .map(|fallback| (fallback.id, fallback.field, fallback.value.clone()))
        .collect();
    fields.sort_by_key(|(id, _, _)| *id);
    let mut expected = vec![
        (
            fixture.key_host,
            PlatformField::IdentityFile,
            r"C:\Users\me\.ssh\db_ed25519".to_owned(),
        ),
        (
            fixture.serial,
            PlatformField::SerialDevice,
            "COM3".to_owned(),
        ),
    ];
    expected.sort_by_key(|(id, _, _)| *id);
    assert_eq!(fields, expected);
    plan.apply(&linux, "import").expect("applied");
    let mut on_linux = linux.load().expect("loads");
    assert_eq!(on_linux.preferences.keymap, Some(linux_keymap.clone()));
    assert_eq!(
        on_linux.preferences.text_size,
        Some(100),
        "the screen of this machine is not the other one's"
    );
    assert_eq!(on_linux.preferences.theme.as_deref(), Some("dark"));

    // The Linux user points the records at this machine's files and exports on
    // top of the document they started from.
    let mut db = on_linux
        .hosts
        .host(fixture.key_host)
        .expect("saved")
        .clone();
    db.set_identity_file(Some("/home/me/.ssh/db_ed25519"));
    on_linux
        .hosts
        .update(db, fixture.key_host)
        .expect("updated");
    let ConnectionRecord::Serial(mut console) =
        on_linux.hosts.connection(fixture.serial).expect("saved")
    else {
        panic!("still a serial connection");
    };
    console.set_device("/dev/ttyUSB0").expect("device");
    on_linux
        .hosts
        .update_connection(ConnectionRecord::Serial(console), fixture.serial)
        .expect("updated");
    let from_linux = ShelluneData::export_from_vault(
        &on_linux,
        &options(Platform::Linux),
        None,
        Some(&from_windows),
    )
    .expect("exported")
    .document;
    assert_eq!(
        from_linux.platform(Platform::Windows),
        from_windows.platform(Platform::Windows),
        "the Windows section is carried over unchanged"
    );
    let linux_section = from_linux.platform(Platform::Linux).expect("written");
    assert_eq!(
        linux_section.keymap.as_deref(),
        Some(linux_keymap.for_platform("linux"))
    );
    assert_eq!(
        linux_section.hosts[&fixture.key_host]
            .identity_file
            .as_deref(),
        Some("/home/me/.ssh/db_ed25519")
    );

    // Back on Windows, the Windows paths and bindings come back, not the Linux ones.
    let windows_root = scratch("windows");
    let windows = LocalData::at(&windows_root);
    save_state(&windows_root, &fixture.state);
    let plan = from_linux
        .plan(
            &windows.load().expect("loads"),
            Platform::Windows,
            ApplyMode::Merge,
            None,
        )
        .expect("planned");
    assert!(plan.fallbacks().is_empty(), "{:?}", plan.fallbacks());
    assert!(plan.is_empty(), "nothing changed for Windows: {plan:?}");

    // A Windows machine without these records still gets its own paths.
    let fresh_root = scratch("windows-fresh");
    let fresh = LocalData::at(&fresh_root);
    from_linux
        .plan(
            &LocalState::default(),
            Platform::Windows,
            ApplyMode::Replace,
            None,
        )
        .expect("planned")
        .apply(&fresh, "import")
        .expect("applied");
    let on_windows = fresh.load().expect("loads");
    assert_eq!(
        on_windows
            .hosts
            .host(fixture.key_host)
            .and_then(HostConfig::identity_file),
        Some(r"C:\Users\me\.ssh\db_ed25519")
    );
    let Some(ConnectionRecord::Serial(console)) = on_windows.hosts.connection(fixture.serial)
    else {
        panic!("a serial connection");
    };
    assert_eq!(console.device(), "COM3");
    assert_eq!(
        on_windows
            .preferences
            .keymap
            .as_ref()
            .map(|keymap| keymap.for_platform("windows").to_vec()),
        fixture
            .state
            .preferences
            .keymap
            .as_ref()
            .map(|keymap| keymap.for_platform("windows").to_vec())
    );
    assert_eq!(on_windows.preferences.text_size, Some(115));
    for root in [linux_root, windows_root, fresh_root] {
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn a_section_of_an_unknown_platform_is_carried_over_untouched() {
    let fixture = windows_machine();
    let document =
        ShelluneData::export_from_vault(&fixture.state, &options(Platform::Windows), None, None)
            .expect("exported")
            .document;
    let mut value: serde_json::Value =
        serde_json::from_slice(&document.to_json().expect("json")).expect("json");
    value["platforms"]["freebsd"] = serde_json::json!({ "keymap": [], "future_field": 7 });
    value["platforms"]["linux"] =
        serde_json::json!({ "text_size": 90, "future_field": { "nested": true } });
    let base = ShelluneData::from_json(&serde_json::to_vec(&value).expect("json"))
        .expect("an unknown platform and field are not damage");
    let next = ShelluneData::export_from_vault(
        &fixture.state,
        &options(Platform::Windows),
        None,
        Some(&base),
    )
    .expect("exported")
    .document;
    let written: serde_json::Value =
        serde_json::from_slice(&next.to_json().expect("json")).expect("json");
    assert_eq!(
        written["platforms"]["freebsd"],
        value["platforms"]["freebsd"]
    );
    assert_eq!(written["platforms"]["linux"], value["platforms"]["linux"]);
    assert_eq!(
        next.platform_names().collect::<Vec<_>>(),
        vec!["freebsd", "linux", "windows"]
    );
}

#[test]
fn damaged_foreign_and_newer_files_are_refused_and_change_nothing() {
    let fixture = windows_machine();
    let root = scratch("refused");
    save_state(&root, &fixture.state);
    let before = tree(&root);
    let document =
        ShelluneData::export_from_vault(&fixture.state, &options(Platform::Windows), None, None)
            .expect("exported")
            .document;
    let good: serde_json::Value =
        serde_json::from_slice(&document.to_json().expect("json")).expect("json");

    assert!(matches!(
        ShelluneData::from_json(b"{not json").expect_err("refused"),
        PortableError::NotShelluneData { .. }
    ));
    assert!(matches!(
        ShelluneData::from_json(br#"{"version":1,"hosts":[]}"#).expect_err("refused"),
        PortableError::NotShelluneData { .. }
    ));
    assert!(matches!(
        ShelluneData::from_json(b"[1,2]").expect_err("refused"),
        PortableError::NotShelluneData { .. }
    ));

    let mut newer = good.clone();
    newer["version"] = serde_json::json!(3);
    newer["something_new"] = serde_json::json!(true);
    assert_eq!(
        ShelluneData::from_json(&serde_json::to_vec(&newer).expect("json")).expect_err("refused"),
        PortableError::UnsupportedVersion {
            found: 3,
            supported: PORTABLE_VAULT_VERSION
        }
    );
    let mut newer_hosts = good.clone();
    newer_hosts["shared"]["hosts"]["version"] = serde_json::json!(9);
    assert!(matches!(
        ShelluneData::from_json(&serde_json::to_vec(&newer_hosts).expect("json"))
            .expect_err("refused"),
        PortableError::UnsupportedVersion { found: 9, .. }
    ));

    let mut damaged = good.clone();
    damaged["shared"]["hosts"]["favorites"] = serde_json::json!([HostId::new().to_string()]);
    assert!(matches!(
        ShelluneData::from_json(&serde_json::to_vec(&damaged).expect("json")).expect_err("refused"),
        PortableError::Corrupt { .. }
    ));
    let mut smuggled = good.clone();
    smuggled["shared"]["preferences"]["password"] = serde_json::json!("hunter2");
    let error = ShelluneData::from_json(&serde_json::to_vec(&smuggled).expect("json"))
        .expect_err("refused");
    assert!(
        matches!(&error, PortableError::Corrupt { reason } if reason.contains("password") && !reason.contains("hunter2")),
        "{error}"
    );
    let mut truncated = serde_json::to_vec(&good).expect("json");
    truncated.truncate(truncated.len() / 2);
    assert!(ShelluneData::from_json(&truncated).is_err());
    let missing = root.join("no-such-backup.json");
    assert!(matches!(
        ShelluneData::read(&missing).expect_err("refused"),
        PortableError::Read { .. }
    ));

    assert_eq!(tree(&root), before, "the local data is untouched");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn merging_keeps_local_records_and_replacing_does_not() {
    let fixture = windows_machine();
    let root = scratch("merge");
    let data = LocalData::at(&root);

    // This machine: the web host under another name, reached later; a host only it
    // has; another identity for the same server.
    let mut local = fixture.state.clone();
    let mut web = local
        .hosts
        .host(fixture.password_host)
        .expect("saved")
        .clone();
    web.set_name("web (old name)").expect("named");
    local
        .hosts
        .update(web, fixture.password_host)
        .expect("updated");
    local
        .hosts
        .record_connection(fixture.password_host, 1_700_000_900)
        .expect("recorded");
    let mine = HostConfig::new("mine", "10.9.9.9", 22, "me").expect("host");
    let mine_id = mine.id();
    local.hosts.insert(mine).expect("inserted");
    local
        .hosts
        .record_connection(mine_id, 1_700_000_500)
        .expect("recorded");
    local.hosts.set_favorite(mine_id, true).expect("marked");
    local.known_hosts = HostTrust::new();
    local
        .known_hosts
        .accept("10.0.0.5", 22, &trust_key(2), 1_600_000_000)
        .expect("trusted");
    local
        .known_hosts
        .accept("10.9.9.9", 22, &trust_key(3), 1_600_000_000)
        .expect("trusted");
    local.snippets = SnippetBook::new();
    local.snippets.create("mine", "uptime").expect("snippet");
    local.preferences.theme = Some("light".to_owned());
    save_state(&root, &local);

    // The document: the fixture, plus a host the machine does not have.
    let mut incoming = fixture.state.clone();
    let new_host = HostConfig::new("new", "10.1.1.1", 22, "ops").expect("host");
    let new_id = new_host.id();
    incoming.hosts.insert(new_host).expect("inserted");
    incoming.preferences.theme = None;
    let document =
        ShelluneData::export_from_vault(&incoming, &options(Platform::Windows), None, None)
            .expect("exported")
            .document;

    let loaded = data.load().expect("loads");
    let merge = document
        .plan(&loaded, Platform::Windows, ApplyMode::Merge, None)
        .expect("planned");
    assert_eq!(merge.mode(), ApplyMode::Merge);
    assert_eq!(merge.source_platform(), "windows");
    assert_eq!(merge.hosts().added, 1, "{merge:?}");
    assert_eq!(merge.hosts().updated, 1);
    assert_eq!(merge.hosts().removed, 0);
    assert_eq!(
        merge.known_hosts(),
        Default::default(),
        "a different identity for the same server does not replace this machine's"
    );
    assert_eq!(
        merge.trust_conflicts(),
        &[TrustConflict {
            host: "10.0.0.5".to_owned(),
            port: 22,
            local_fingerprint: trust_key(2).fingerprint,
            document_fingerprint: trust_key(1).fingerprint,
        }]
    );
    assert_eq!(merge.snippets().added, 1);
    assert_eq!(merge.snippets().removed, 0);
    merge.apply(&data, "merge").expect("applied");
    let merged = data.load().expect("loads");
    assert_eq!(
        merged
            .hosts
            .host(fixture.password_host)
            .expect("kept")
            .name(),
        "web"
    );
    assert!(
        merged.hosts.host(mine_id).is_some(),
        "a local-only record is kept"
    );
    assert!(merged.hosts.host(new_id).is_some());
    assert_eq!(
        merged
            .hosts
            .recent_of(fixture.password_host)
            .map(|recent| recent.last_connected_unix()),
        Some(1_700_000_900),
        "the newer connection time wins"
    );
    assert!(merged.hosts.recent_of(mine_id).is_some());
    assert!(merged.hosts.is_favorite(mine_id));
    assert!(merged.hosts.is_favorite(fixture.key_host));
    assert_eq!(
        merged.known_hosts.entries().len(),
        2,
        "identities are combined"
    );
    assert_eq!(
        merged
            .known_hosts
            .entry("10.0.0.5", 22)
            .map(|entry| entry.fingerprint().to_owned()),
        Some(trust_key(2).fingerprint),
        "this machine's identity stays for the same server"
    );
    assert_eq!(merged.snippets.len(), 2);
    assert_eq!(
        merged.preferences.theme.as_deref(),
        Some("light"),
        "a choice the document leaves unset is kept by a merge"
    );

    let replace = document
        .plan(&merged, Platform::Windows, ApplyMode::Replace, None)
        .expect("planned");
    assert_eq!(replace.hosts().removed, 1);
    assert_eq!(replace.known_hosts().removed, 1);
    assert_eq!(replace.known_hosts().updated, 1);
    assert_eq!(replace.trust_conflicts().len(), 1);
    assert_eq!(replace.snippets().removed, 1);
    replace.apply(&data, "replace").expect("applied");
    let replaced = data.load().expect("loads");
    assert!(replaced.hosts.host(mine_id).is_none());
    assert_eq!(replaced.hosts.connection_count(), 4);
    assert_eq!(replaced.known_hosts.entries().len(), 1);
    assert_eq!(
        replaced
            .known_hosts
            .entry("10.0.0.5", 22)
            .map(|entry| entry.fingerprint().to_owned()),
        Some(trust_key(1).fingerprint),
        "a replace takes the document's identity"
    );
    assert_eq!(replaced.snippets.len(), 1);
    assert_eq!(replaced.preferences.theme, None);
    assert_eq!(
        replaced
            .hosts
            .recent_of(fixture.password_host)
            .map(|recent| recent.last_connected_unix()),
        Some(1_700_000_100)
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn applying_snapshots_the_previous_files_and_keeps_the_newest_ten() {
    let fixture = windows_machine();
    let root = scratch("snapshots");
    let data = LocalData::at(&root);
    let mut local = fixture.state.clone();
    local.preferences.theme = Some("light".to_owned());
    save_state(&root, &local);
    let previous_hosts = std::fs::read(root.join("hosts.json")).expect("hosts");
    let previous_preferences = std::fs::read(root.join("preferences.json")).expect("prefs");

    let document =
        ShelluneData::export_from_vault(&fixture.state, &options(Platform::Windows), None, None)
            .expect("exported")
            .document;
    let plan = document
        .plan(
            &data.load().expect("loads"),
            Platform::Windows,
            ApplyMode::Replace,
            None,
        )
        .expect("planned");
    assert_eq!(plan.files().collect::<Vec<_>>(), vec!["preferences.json"]);
    let outcome = plan.apply(&data, "Import backup").expect("applied");
    let snapshot = outcome.snapshot.expect("a snapshot was made");
    assert!(snapshot.starts_with(root.join("snapshots")));
    assert!(
        snapshot
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-import-backup")),
        "{}",
        snapshot.display()
    );
    assert_eq!(
        std::fs::read(snapshot.join("hosts.json")).expect("copied"),
        previous_hosts
    );
    assert_eq!(
        std::fs::read(snapshot.join("preferences.json")).expect("copied"),
        previous_preferences
    );

    for round in 0..(SNAPSHOT_LIMIT + 2) {
        let mut changed = fixture.state.clone();
        changed.preferences.terminal_font_size = Some(10.0 + round as f32);
        let document =
            ShelluneData::export_from_vault(&changed, &options(Platform::Windows), None, None)
                .expect("exported")
                .document;
        document
            .plan(
                &data.load().expect("loads"),
                Platform::Windows,
                ApplyMode::Merge,
                None,
            )
            .expect("planned")
            .apply(&data, "pull")
            .expect("applied");
    }
    let kept = data.snapshots().list();
    assert_eq!(kept.len(), SNAPSHOT_LIMIT);
    assert!(
        kept.iter()
            .all(|snapshot| snapshot.reason().starts_with("pull"))
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_failed_password_save_puts_the_files_back() {
    let fixture = windows_machine();
    let secrets = vault_for(&fixture);
    let document = ShelluneData::export_from_vault(
        &fixture.state,
        &with_passphrase(Platform::Windows),
        Some(&secrets),
        None,
    )
    .expect("exported")
    .document;
    let bundle = document
        .open_secrets(&SecretString::new(PASSPHRASE))
        .expect("opened");

    let root = scratch("secret-failure");
    let data = LocalData::at(&root);
    let mut local = LocalState::default();
    let only = HostConfig::new("only", "10.2.2.2", 22, "me").expect("host");
    local.hosts.insert(only).expect("inserted");
    save_state(&root, &local);
    let before: Vec<(PathBuf, Vec<u8>)> = tree(&root);

    let plan = document
        .plan(
            &data.load().expect("loads"),
            Platform::Windows,
            ApplyMode::Merge,
            Some(&bundle),
        )
        .expect("planned");
    let mut vault = VaultContent::new();
    let error = plan
        .apply_with_vault(&data, &mut vault, DEVICE, "import", &mut |_| {
            Err("the vault could not be written".to_owned())
        })
        .expect_err("the vault cannot be saved");
    let PortableError::Apply {
        restored, snapshot, ..
    } = &error
    else {
        panic!("{error}");
    };
    assert!(restored, "{error}");
    assert!(snapshot.starts_with(root.join("snapshots")));
    assert!(!root.join(shellune_storage::VAULT_FILE_NAME).exists());

    // Without a vault to keep them in, the passwords refuse the whole apply
    // before anything is written.
    assert!(matches!(
        plan.apply(&data, "import"),
        Err(PortableError::Local { .. })
    ));
    let after: Vec<(PathBuf, Vec<u8>)> = tree(&root)
        .into_iter()
        .filter(|(path, _)| !path.starts_with(root.join("snapshots")))
        .collect();
    assert_eq!(after, before, "every data file has its previous bytes");
    assert_eq!(data.load().expect("loads"), local);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_sync_view_ignores_the_time_stamp_and_other_platforms() {
    let fixture = windows_machine();
    let first =
        ShelluneData::export_from_vault(&fixture.state, &options(Platform::Windows), None, None)
            .expect("exported")
            .document;
    let mut later = options(Platform::Windows);
    later.created_unix += 3_600;
    let second = ShelluneData::export_from_vault(&fixture.state, &later, None, Some(&first))
        .expect("exported")
        .document;
    assert_eq!(
        first.sync_view(Platform::Windows).expect("view"),
        second.sync_view(Platform::Windows).expect("view")
    );
    let reread = ShelluneData::from_json(&second.to_json().expect("json")).expect("read");
    assert_eq!(
        reread.sync_view(Platform::Windows).expect("view"),
        first.sync_view(Platform::Windows).expect("view")
    );
    let mut changed = fixture.state.clone();
    changed.snippets.create("new", "ls").expect("snippet");
    let third = ShelluneData::export_from_vault(&changed, &options(Platform::Windows), None, None)
        .expect("exported")
        .document;
    assert_ne!(
        first.sync_view(Platform::Windows).expect("view"),
        third.sync_view(Platform::Windows).expect("view")
    );
}

#[test]
fn the_core_view_leaves_out_only_the_recent_connections() {
    let fixture = windows_machine();
    let export = |state: &LocalState| {
        ShelluneData::export_from_vault(state, &options(Platform::Windows), None, None)
            .expect("exported")
            .document
    };
    let first = export(&fixture.state);
    // Connecting changes the sync view but not the core view.
    let mut connected = fixture.state.clone();
    connected
        .hosts
        .record_connection(fixture.key_host, 1_700_000_900)
        .expect("recorded");
    connected
        .hosts
        .record_connection(fixture.password_host, 1_700_000_950)
        .expect("recorded");
    let second = export(&connected);
    assert_ne!(
        first.sync_view(Platform::Windows).expect("view"),
        second.sync_view(Platform::Windows).expect("view")
    );
    assert_eq!(
        first.core_view(Platform::Windows).expect("view"),
        second.core_view(Platform::Windows).expect("view")
    );
    let core = String::from_utf8(first.core_view(Platform::Windows).expect("view")).expect("utf-8");
    assert!(!core.contains("recents"), "{core}");
    assert!(core.contains("favorites"), "{core}");
    // Any other change shows in the core view too.
    let mut changed = fixture.state.clone();
    changed.snippets.create("new", "ls").expect("snippet");
    assert_ne!(
        first.core_view(Platform::Windows).expect("view"),
        export(&changed).core_view(Platform::Windows).expect("view")
    );
    let mut moved = fixture.state.clone();
    moved.preferences.text_size = Some(140);
    assert_ne!(
        first.core_view(Platform::Windows).expect("view"),
        export(&moved).core_view(Platform::Windows).expect("view")
    );
}

#[test]
fn recent_connections_merge_by_host_keeping_the_later_one() {
    let fixture = windows_machine();
    let (web, db) = (fixture.password_host, fixture.key_host);
    let mut with_cache = fixture.state.clone();
    let cache_host = HostConfig::new("cache", "10.0.0.7", 22, "redis").expect("host");
    let cache = cache_host.id();
    with_cache.hosts.insert(cache_host).expect("inserted");
    // Here: web at 100 (once before), db at 500 twice.
    let mut here = with_cache.hosts.clone();
    here.record_connection(web, 100).expect("recorded");
    here.record_connection(db, 500).expect("recorded");
    here.record_connection(db, 500).expect("recorded");
    // There: web at 300, db at 500 once, cache at 200.
    let mut there = HostBook::new();
    for record in with_cache.hosts.records() {
        there.insert_connection(record).expect("inserted");
    }
    there.record_connection(web, 300).expect("recorded");
    there.record_connection(db, 500).expect("recorded");
    there.record_connection(cache, 200).expect("recorded");

    let merged = merge_recent_connections(here.recents(), there.recents());
    let seen: Vec<(HostId, u64, u32)> = merged
        .iter()
        .map(|recent| {
            (
                recent.host(),
                recent.last_connected_unix(),
                recent.connections(),
            )
        })
        .collect();
    // Newest first; db at the same time keeps the higher count.
    assert_eq!(seen, vec![(db, 500, 2), (web, 300, 1), (cache, 200, 1)]);
    // The order of the two sides does not matter.
    assert_eq!(
        merge_recent_connections(there.recents(), here.recents()),
        merged
    );

    // A document takes the merged log; an entry whose host is gone is dropped.
    let document =
        ShelluneData::export_from_vault(&fixture.state, &options(Platform::Windows), None, None)
            .expect("exported")
            .document;
    let replaced = document.with_recents(&merged).expect("replaced");
    let hosts: Vec<HostId> = replaced
        .shared()
        .hosts()
        .recents()
        .iter()
        .map(|recent| recent.host())
        .collect();
    assert_eq!(hosts, vec![db, web]);
    assert_eq!(
        replaced.core_view(Platform::Windows).expect("view"),
        document.core_view(Platform::Windows).expect("view")
    );
    assert_eq!(
        replaced.shared().hosts().favorites(),
        document.shared().hosts().favorites()
    );
}

#[test]
fn a_recents_plan_writes_only_the_host_file_after_a_snapshot() {
    let root = scratch("recents-plan");
    let fixture = windows_machine();
    save_state(&root, &fixture.state);
    let data = LocalData::at(&root);
    let local = data.load().expect("loads");
    let before = tree(&root);
    let mut other = local.hosts.clone();
    other
        .record_connection(fixture.key_host, 1_800_000_000)
        .expect("recorded");
    let merged = merge_recent_connections(local.hosts.recents(), other.recents());
    let plan = ApplyPlan::with_recents(&local, Platform::Windows, &merged).expect("planned");
    assert_eq!(plan.files().collect::<Vec<_>>(), vec!["hosts.json"]);
    assert!(plan.hosts().is_empty(), "no record changes");
    let applied = plan.apply(&data, "sync-merge").expect("applied");
    assert_eq!(applied.files_written, vec!["hosts.json"]);
    assert_eq!(applied.secrets_saved, 0, "no password is touched");
    let snapshot = applied.snapshot.expect("a snapshot");
    for (path, bytes) in &before {
        let name = path.file_name().expect("a name");
        assert_eq!(&std::fs::read(snapshot.join(name)).expect("kept"), bytes);
    }
    let after = data.load().expect("loads");
    assert_eq!(
        after
            .hosts
            .recent_of(fixture.key_host)
            .map(|recent| recent.last_connected_unix()),
        Some(1_800_000_000)
    );
    assert_eq!(after.hosts.records(), local.hosts.records());
    assert_eq!(after.hosts.favorites(), local.hosts.favorites());
    assert_eq!(after.preferences, local.preferences);
    assert_eq!(after.known_hosts, local.known_hosts);
    // Planned against data that changed meanwhile, it writes nothing.
    let stale = ApplyPlan::with_recents(&local, Platform::Windows, &merged).expect("planned");
    assert_eq!(
        stale.apply(&data, "sync-merge").unwrap_err(),
        PortableError::Changed
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_document_does_not_say_which_passwords_a_machine_remembers() {
    let fixture = windows_machine();
    let secrets = vault_for(&fixture);
    for options in [
        options(Platform::Windows),
        with_passphrase(Platform::Windows),
    ] {
        let document =
            ShelluneData::export_from_vault(&fixture.state, &options, Some(&secrets), None)
                .expect("exported")
                .document;
        assert!(
            document
                .shared()
                .hosts()
                .records()
                .iter()
                .all(|record| !record.remembers_secret()),
            "the shared records carry no remembered flag"
        );
        let text = String::from_utf8(document.to_json().expect("json")).expect("utf-8");
        assert!(!text.contains("remember_secret"), "{text}");
    }
}

#[test]
fn remembering_a_password_follows_this_machine_not_the_document() {
    let fixture = windows_machine();

    // A document that (as another build could write it) says both hosts remember
    // a password, without sealing any.
    let exported =
        ShelluneData::export_from_vault(&fixture.state, &options(Platform::Windows), None, None)
            .expect("exported")
            .document;
    let mut value: serde_json::Value =
        serde_json::from_slice(&exported.to_json().expect("json")).expect("value");
    for host in value["shared"]["hosts"]["hosts"]
        .as_array_mut()
        .expect("hosts")
    {
        host["remember_secret"] = serde_json::Value::Bool(true);
    }
    let claiming =
        ShelluneData::from_json(&serde_json::to_vec(&value).expect("bytes")).expect("read");
    assert!(
        claiming
            .shared()
            .hosts()
            .host(fixture.password_host)
            .expect("there")
            .remembers_secret()
    );

    // A machine that has none of these records saves no password and so marks
    // none as remembered, whether merging or replacing.
    for mode in [ApplyMode::Merge, ApplyMode::Replace] {
        let plan = claiming
            .plan(&LocalState::default(), Platform::Windows, mode, None)
            .expect("planned");
        assert_eq!(plan.secrets_to_import(), 0);
        assert!(plan.secrets_to_forget().is_empty());
        assert!(
            plan.result()
                .hosts
                .records()
                .iter()
                .all(|record| !record.remembers_secret()),
            "{mode:?}"
        );
    }

    // A machine that remembers both passwords keeps remembering them, and keeps
    // the passwords, when a document from a machine without them arrives.
    let root = scratch("remember-kept");
    let data = LocalData::at(&root);
    save_state(&root, &fixture.state);
    let mut vault = vault_for(&fixture);
    for mode in [ApplyMode::Merge, ApplyMode::Replace] {
        let plan = exported
            .plan(&data.load().expect("loads"), Platform::Windows, mode, None)
            .expect("planned");
        assert!(plan.is_empty(), "{mode:?}: {plan:?}");
        assert_eq!(plan.hosts(), Default::default());
        plan.apply_with_vault(&data, &mut vault, DEVICE, "pull", &mut keep)
            .expect("applied");
        let loaded = data.load().expect("loads");
        assert!(
            loaded
                .hosts
                .host(fixture.password_host)
                .expect("kept")
                .remembers_secret()
        );
        assert!(
            loaded
                .hosts
                .host(fixture.key_host)
                .expect("kept")
                .remembers_secret()
        );
    }
    assert_eq!(
        password_in(&vault, fixture.password_host).as_deref(),
        Some(PASSWORD),
        "the saved passwords are still there"
    );
    assert!(password_in(&vault, fixture.key_host).is_some());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn passwords_nobody_reads_any_more_are_forgotten() {
    let fixture = windows_machine();
    let root = scratch("forget");
    let data = LocalData::at(&root);

    // This machine also has a host only it knows, with a remembered password.
    let mut local = fixture.state.clone();
    let mut mine = HostConfig::new("mine", "10.9.9.9", 22, "me").expect("host");
    mine.set_auth(Some(AuthMethodKind::Password));
    mine.set_remember_secret(true);
    let mine_id = mine.id();
    local.hosts.insert(mine).expect("inserted");
    save_state(&root, &local);
    let mut vault = vault_for(&fixture);
    vault.set_host_secret(mine_id, SecretString::new("mine-password"), DEVICE);

    // The document signs in to the web host with the agent now.
    let mut incoming = fixture.state.clone();
    let mut web = incoming
        .hosts
        .host(fixture.password_host)
        .expect("there")
        .clone();
    web.set_auth(Some(AuthMethodKind::Agent));
    incoming
        .hosts
        .update(web, fixture.password_host)
        .expect("updated");
    let document =
        ShelluneData::export_from_vault(&incoming, &options(Platform::Windows), None, None)
            .expect("exported")
            .document;

    // A merge keeps the local host and its password, and forgets the password
    // the web host no longer uses.
    let merge = document
        .plan(
            &data.load().expect("loads"),
            Platform::Windows,
            ApplyMode::Merge,
            None,
        )
        .expect("planned");
    assert_eq!(merge.secrets_to_forget(), &[fixture.password_host]);
    let outcome = merge
        .apply_with_vault(&data, &mut vault, DEVICE, "merge", &mut keep)
        .expect("applied");
    assert_eq!(outcome.secrets_forgotten, 1);
    assert!(outcome.secrets_not_forgotten.is_empty());
    assert_eq!(
        vault.host_secret_state(fixture.password_host),
        HostSecretState::Tombstoned,
        "the deletion is a tombstone that travels with the vault"
    );
    assert!(password_in(&vault, mine_id).is_some());
    let merged = data.load().expect("loads");
    assert!(
        !merged
            .hosts
            .host(fixture.password_host)
            .expect("kept")
            .remembers_secret()
    );
    assert!(merged.hosts.host(mine_id).expect("kept").remembers_secret());
    assert!(
        merged
            .hosts
            .host(fixture.key_host)
            .expect("kept")
            .remembers_secret()
    );

    // A replace removes the local host, so its password goes with it.
    let replace = document
        .plan(&merged, Platform::Windows, ApplyMode::Replace, None)
        .expect("planned");
    assert_eq!(replace.hosts().removed, 1);
    assert_eq!(replace.secrets_to_forget(), &[mine_id]);
    let replaced = replace
        .apply_with_vault(&data, &mut vault, DEVICE, "replace", &mut keep)
        .expect("applied");
    assert_eq!(replaced.secrets_forgotten, 1);
    assert_eq!(
        vault.host_secret_state(mine_id),
        HostSecretState::Tombstoned
    );
    assert!(data.load().expect("loads").hosts.host(mine_id).is_none());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_plan_is_not_applied_over_data_saved_after_it() {
    let fixture = windows_machine();
    let root = scratch("stale");
    let data = LocalData::at(&root);
    save_state(&root, &LocalState::default());
    let document =
        ShelluneData::export_from_vault(&fixture.state, &options(Platform::Windows), None, None)
            .expect("exported")
            .document;
    let plan = document
        .plan(
            &data.load().expect("loads"),
            Platform::Windows,
            ApplyMode::Merge,
            None,
        )
        .expect("planned");

    // Something is saved between the preview and the confirmation.
    let mut later = LocalState::default();
    later
        .snippets
        .create("saved later", "uptime")
        .expect("snippet");
    save_state(&root, &later);
    let before = tree(&root);

    let error = plan.apply(&data, "import").expect_err("the data changed");
    assert_eq!(error, PortableError::Changed);
    assert_eq!(
        tree(&root),
        before,
        "nothing was written, not even a snapshot"
    );

    // Planning again from the current data keeps what was saved.
    let again = document
        .plan(
            &data.load().expect("loads"),
            Platform::Windows,
            ApplyMode::Merge,
            None,
        )
        .expect("planned");
    again.apply(&data, "import").expect("applied");
    assert_eq!(data.load().expect("loads").snippets.len(), 2);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_version_two_document_carries_the_vault_and_not_the_old_secrets() {
    let fixture = windows_machine();
    let sync_passphrase = SecretString::new("the one sync passphrase");

    // A sealed vault with one shared password and the trust records that
    // travel with it from version 2 on.
    let mut vault = VaultContent::new();
    vault
        .insert_credential(VaultCredentialRecord::new(
            shellune_core::CredentialMeta::new(
                shellune_core::CredentialId::new(),
                "Prod login",
                Some("deploy".to_owned()),
                shellune_core::VaultRecordVersion::new(1, "device-a"),
            ),
            SecretString::new("hunter2"),
        ))
        .expect("inserts");
    let mut trust = HostTrust::new();
    trust
        .accept("10.0.0.5", 22, &trust_key(9), 1_700_000_000)
        .expect("trusted");
    vault.set_trust(trust);
    let cipher = VaultCipher::export_copy(&vault, &sync_passphrase, cheap_kdf()).expect("sealed");

    let mut vault_options = options(Platform::Windows).with_vault(cipher);
    let outcome = ShelluneData::export_from_vault(&fixture.state, &vault_options, None, None)
        .expect("exported");
    let document = outcome.document;
    assert_eq!(document.version(), PORTABLE_VAULT_VERSION);
    assert!(
        document.version() > PORTABLE_VERSION,
        "an older build refuses the document by the version check it already has"
    );
    assert!(document.has_vault());
    assert!(
        !document.has_secrets(),
        "a version 2 document writes no version 1 password block"
    );
    assert!(
        document.shared().known_hosts().is_empty(),
        "the accepted identities are the vault's to guard from version 2 on"
    );
    assert_eq!(outcome.secrets_included.len(), 0);

    // The document round-trips as bytes, and the bytes hold no secret.
    let bytes = document.to_json().expect("json");
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("hunter2"), "{text}");
    assert!(!text.contains("webdav"), "{text}");
    let back = ShelluneData::from_json(&bytes).expect("reads");
    let opened = back
        .vault()
        .expect("the vault travels")
        .open(&shellune_storage::KeySource::Passphrase {
            purpose: shellune_storage::PassphrasePurpose::SyncPassphrase,
            value: sync_passphrase.clone(),
        })
        .expect("the sync passphrase opens it");
    assert_eq!(opened.credentials().count(), 1);
    assert_eq!(opened.trust().entries().len(), 1);
    let wrong =
        back.vault()
            .expect("the vault travels")
            .open(&shellune_storage::KeySource::Passphrase {
                purpose: shellune_storage::PassphrasePurpose::SyncPassphrase,
                value: SecretString::new("not the passphrase"),
            });
    assert!(
        matches!(wrong, Err(shellune_storage::VaultStoreError::Sealed)),
        "a wrong passphrase does not open the travelling vault"
    );

    // Neither secrets section may travel with the other's version.
    let mut mixed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    mixed["secrets"] = serde_json::json!({});
    assert!(matches!(
        ShelluneData::from_json(&serde_json::to_vec(&mixed).expect("json")),
        Err(PortableError::Corrupt { .. })
    ));
    let plain =
        ShelluneData::export_from_vault(&fixture.state, &options(Platform::Windows), None, None)
            .expect("exported")
            .document;
    let mut v1_with_vault: serde_json::Value =
        serde_json::from_slice(&plain.to_json().expect("json")).expect("json");
    v1_with_vault["vault"] = serde_json::json!({});
    assert!(matches!(
        ShelluneData::from_json(&serde_json::to_vec(&v1_with_vault).expect("json")),
        Err(PortableError::Corrupt { .. })
    ));

    // A caller that asks for both secrets sections is refused rather than
    // given a document that silently drops one of them.
    let cipher = VaultCipher::export_copy(&vault, &sync_passphrase, cheap_kdf()).expect("sealed");
    vault_options = options(Platform::Windows).with_vault(cipher);
    vault_options.passphrase = Some(SecretString::new(PASSPHRASE));
    assert!(matches!(
        ShelluneData::export_from_vault(&fixture.state, &vault_options, None, None),
        Err(PortableError::Encrypt { .. })
    ));
}
