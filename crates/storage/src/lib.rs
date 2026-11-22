//! Versioned, atomic storage for saved hosts, their groups, forwarding rules and
//! workspace snapshots (R3, R4, R10, R11, R18).
//!
//! The crate owns the user-data files:
//!
//! * `HostBook` / `HostStore` — hosts, their group labels, the recent-connection
//!   log and the favourite marks. One book, so search, grouping and the recent view can never disagree
//!   about what a host is called or which address it uses. A host may also name a
//!   jump chain and a ProxyCommand; those are references and command text. The
//!   same book holds the connections whose protocol is not SSH, each carrying its
//!   protocol explicitly: a record of a protocol this build does not know is
//!   reported instead of loaded, and a book without such records is written exactly
//!   as an older build wrote it.
//! * `GroupStore` / `GroupService` — the group tree: which paths exist, empty ones
//!   included, and the changes that keep it and the hosts agreeing when a group is
//!   moved, renamed or deleted.
//! * `ForwardStore` — saved local, remote and dynamic forwarding rules.
//! * `WorkspaceStore` — the workspace snapshot: tabs, pane tree and panel.
//! * `SnippetStore` — the saved commands: a name and a command body each, in their
//!   own file, with no field that could hold a credential.
//! * `PreferencesStore` — the interface choices and the custom key bindings, per
//!   platform, with a reporting reader so a damaged file is never mistaken for a
//!   default the user chose.
//! * `StoreEvent` — the closed set of log lines the host store emits, so the
//!   logging path can be tested for the absence of secrets instead of being trusted.
//! * `ImportPreview` — what one SSH configuration file would contribute, read
//!   without ever writing it, with every limitation named before anything is saved.
//! * `ShelluneData` — the portable data file a backup writes and sync uploads: the
//!   shared records, one section per platform for what differs between machines,
//!   and optionally the saved passwords sealed under a passphrase. Applying one is
//!   previewed as an `ApplyPlan`, snapshots the data files first and puts them back
//!   when a write fails. A version 2 document may instead carry the encrypted
//!   vault (see `VaultStore`), which replaces the version 1 password block and
//!   moves the accepted server identities into the ciphertext.
//! * `VaultStore` — the encrypted vault (`vault.json`): managed key bodies,
//!   shared passwords, their public metadata, the host references that use them
//!   and the accepted server identities, sealed as one AES-256-GCM ciphertext
//!   under a random data key that every unseal secret — this machine's own, the
//!   WebDAV password, the sync passphrase — can unwrap (R2, R9).
//!
//! Loading is strict (a damaged or newer file is reported, never guessed at) and
//! saving is atomic (a temporary file in the same directory is flushed and then
//! renamed over the target). Every failure keeps the previous file intact.
//! Reading a snapshot or a rule file does not start a process, a connection or a
//! listener.
//!
//! Secrets are deliberately absent from the host model. A password field is
//! rejected rather than loaded, and its value is not copied into the error. The
//! one place secrets live at rest is the encrypted vault and the sealed block
//! of a portable document, both of which hold them only as ciphertext.
//!
//! [`HostConfig`]: shellune_core::HostConfig

mod audit;
mod batch;
mod book;
mod credentials;
mod error;
mod forwarding;
mod group;
mod platforms;
mod portable;
mod preferences;
mod snippets;
mod ssh_config;
mod store;
mod trust;
mod vault;
mod workspace;

pub use audit::{CONFIG_KEYS, LOG_FIELDS, StoreEvent};
pub use batch::{
    BatchStore, DIRECTORY as BATCH_DIRECTORY, RUNS_DIRECTORY as BATCH_RUNS_DIRECTORY, RunLog,
    TASKS_FILE_NAME as BATCH_TASKS_FILE_NAME,
};
pub use book::{
    FORMAT_VERSION, Group, HostBook, HostEdit, LegacyGroupLabel, RecentConnection, RecentHost,
    UnreadableConnection, host_matches,
};
pub use credentials::{AppSecret, MemorySecrets, SERVICE, SecretError, SecretStore, SystemSecrets};
pub use error::{BookError, PersistError, StorageError};
pub use forwarding::{FILE_NAME as FORWARD_FILE_NAME, ForwardStore};
pub use group::{
    FILE_NAME as GROUP_FILE_NAME, GroupDeletion, GroupError, GroupService, GroupStore, GroupUpdate,
};
pub use platforms::{FILE_NAME as PLATFORMS_FILE_NAME, PlatformStore};
pub use portable::{
    ApplyMode, ApplyOutcome, ApplyPlan, Change, ExportOptions, ExportOutcome, HostOverlay,
    KdfParams, LocalData, LocalState, PORTABLE_FORMAT, PORTABLE_VAULT_VERSION, PORTABLE_VERSION,
    Platform, PlatformData, PlatformFallback, PlatformField, PortableError, Rebased,
    SNAPSHOT_DIRECTORY, SNAPSHOT_LIMIT, SecretBundle, SharedData, SharedPreferences, ShelluneData,
    Snapshot, SnapshotStore, Touched, TrustConflict, merge_recent_connections, rebase_state,
};
pub use preferences::{
    FILE_NAME as PREFERENCES_FILE_NAME, KeymapEntry, KeymapPreferences, Preferences,
    PreferencesProblem, PreferencesStore,
};
pub use snippets::{FILE_NAME as SNIPPETS_FILE_NAME, SnippetStore};
pub use ssh_config::{
    DEFAULT_CONFIG_TEXT, ImportError, ImportLimits, ImportNote, ImportOutcome, ImportPlan,
    ImportPreview, ImportedForward, ImportedHost, JumpSpec, NoteKind, PlannedForward, SkippedHost,
    default_config_path, home_directory, resolve_path,
};
pub use store::{FILE_NAME, HostStore};
pub use trust::{
    HostKey, HostTrust, TRUST_FILE_NAME, TRUST_FORMAT_VERSION, TrustEntry, TrustError, TrustStore,
    TrustVerdict, is_sha256_fingerprint,
};
pub use vault::{
    HostReference, KeySource, MigrationReport, PassphrasePurpose, UnsealSource, VAULT_FILE_NAME,
    VAULT_FORMAT, VAULT_FORMAT_VERSION, VaultCipher, VaultContent, VaultCredentialRecord,
    VaultKeyRecord, VaultStore, VaultStoreError, WrapRecord,
};
pub use workspace::{FILE_NAME as WORKSPACE_FILE_NAME, WorkspaceStore};
