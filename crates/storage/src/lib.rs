//! Versioned, atomic storage for saved hosts (R3).
//!
//! The crate owns exactly three things:
//!
//! * `HostBook` — the records a user created: hosts, their group labels and the
//!   recent-connection log. One book, so search, grouping and the recent view can
//!   never disagree about what a host is called or which address it uses.
//! * `HostStore` — the file those records live in. Loading is strict (a damaged
//!   or newer file is reported, never guessed at), saving is atomic (a temporary
//!   file in the same directory is flushed and then renamed over the target), and
//!   every failure keeps the previous file intact.
//! * `StoreEvent` — the closed set of log lines the store emits, so the logging
//!   path can be tested for the absence of secrets instead of being trusted.
//! * `ImportPreview` — what one SSH configuration file would contribute, read
//!   without ever writing it, with every limitation named before anything is saved.
//!
//! Secrets are deliberately absent from the model: a `HostConfig` stores a name,
//! an address, a port, a user and an optional group, and nothing else. Both the
//! document and the per-host record are deserialized with `deny_unknown_fields`,
//! so a file that carries a password field is rejected rather than loaded and
//! ignored.
//!
//! [`HostConfig`]: shellune_core::HostConfig

mod audit;
mod book;
mod error;
mod ssh_config;
mod store;
mod trust;

pub use audit::{CONFIG_KEYS, LOG_FIELDS, StoreEvent};
pub use book::{
    FORMAT_VERSION, Group, HostBook, HostEdit, RecentConnection, RecentHost, host_matches,
};
pub use error::{BookError, StorageError};
pub use ssh_config::{
    DEFAULT_CONFIG_TEXT, ImportError, ImportLimits, ImportNote, ImportOutcome, ImportPlan,
    ImportPreview, ImportedHost, NoteKind, SkippedHost, default_config_path, home_directory,
    resolve_path,
};
pub use store::{FILE_NAME, HostStore};
pub use trust::{
    HostKey, HostTrust, TRUST_FILE_NAME, TRUST_FORMAT_VERSION, TrustEntry, TrustError, TrustStore,
    TrustVerdict, is_sha256_fingerprint,
};
