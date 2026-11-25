//! WebDAV sync for Shellune (R7, R8, R10).
//!
//! One file in one WebDAV folder, [`REMOTE_FILE_NAME`], holds the portable data
//! document ([`shellune_storage::ShelluneData`]). Every machine that syncs reads
//! it, applies what changed elsewhere, and writes its own changes back, keeping
//! the sections the other platforms wrote.
//!
//! * [`WebDavClient`] — the requests: a connection test that tells a refused
//!   sign-in, an unreachable server and a folder that cannot be written apart;
//!   the file's version, the file, and a write that only happens when the file is
//!   still the version that was read.
//! * [`SyncConfig`] / [`SyncConfigStore`] — `sync.json`: server, account, folder
//!   and automatic sync (on by default). The encrypted vault, remembered host
//!   passwords included, travels with every sync; there is no switch for it
//!   (R7). The WebDAV password and the sync passphrase are in the system
//!   credential store ([`shellune_storage::AppSecret`]).
//! * [`SyncState`] / [`SyncStateStore`] — `sync-state.json`: what both sides held
//!   after the last successful sync, and how the last attempt ended.
//! * [`SyncService`] — one sync: compare, then upload, pull, do nothing or report
//!   a [`ConflictReport`]; and the three answers to a conflict ([`Resolution`]).
//!
//! Nothing here starts a thread or a timer: the application decides when to sync
//! and runs the blocking calls in the background.

mod config;
mod engine;
mod webdav;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use config::{
    BASE_FILE_NAME, CONFIG_FILE_NAME, ConfigError, DEFAULT_REMOTE_DIR, LastResult, STATE_FILE_NAME,
    SyncConfig, SyncConfigStore, SyncState, SyncStateStore, VaultBaseline, VaultRecordBase,
};
pub use engine::{
    ConflictReport, PullReport, Resolution, SyncError, SyncOutcome, SyncService, TrustIdentity,
    UploadReport, VaultChoice, VaultChoiceKey, VaultChoiceSide, VaultConflict, VaultRecordKind,
    VaultRecordSide, VaultRestored,
};
pub use webdav::{
    Download, Precondition, REMOTE_FILE_NAME, RemoteFile, WebDavClient, WebDavError,
    is_strong_etag, normalize_etag,
};
