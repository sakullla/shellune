//! Render-independent domain model for Shellune.
//!
//! The crate deliberately has no GPUI, SSH or storage dependency: the application
//! shell, the SSH layer and the storage layer all translate into these types, which
//! is what makes session state, host validation, transfer state and secret handling
//! testable without a window or a network.
//!
//! Two rules hold for every type here:
//!
//! * state is only reported after the transition that justifies it, so a failed
//!   connection can never be rendered as a live session;
//! * user-facing text never contains a secret, which is why secrets use
//!   [`SecretString`] and errors use fixed, structured variants.

mod auth;
mod error;
mod files;
pub mod fingerprint;
mod host;
mod ids;
mod session;
mod shell;
mod task;

pub use auth::{AuthMethod, AuthMethodKind};
pub use error::{AgentKind, SecretString, SecurityError};
pub use files::{
    BrowseFailure, BrowseRequest, BrowseResponse, BrowseState, DirEntry, DirectoryListing,
    FileBrowser, FileKind, MutationAction, MutationOutcome, MutationStep, NameProblem, check_name,
    is_single_name, join_path, normalize_path, parent_path,
};
pub use fingerprint::sha256_fingerprint;
pub use host::{DEFAULT_SSH_PORT, HostConfig, HostValidationError, Port};
pub use ids::{HostId, SessionId, TransferId};
pub use session::{
    EventTag, Session, SessionFailure, SessionKind, SessionState, SessionTransitionError,
};
pub use shell::LocalShell;
pub use task::{
    OverwriteChoice, PARTIAL_PREFIX, Transfer, TransferDirection, TransferEvent, TransferFailure,
    TransferQueue, TransferState, TransferUpdate, TransferUpdateError, partial_path,
};
