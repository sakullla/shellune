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
pub mod fingerprint;
mod host;
mod ids;
mod session;
mod shell;
mod task;

pub use auth::{AuthMethod, AuthMethodKind};
pub use error::{AgentKind, SecretString, SecurityError};
pub use fingerprint::sha256_fingerprint;
pub use host::{DEFAULT_SSH_PORT, HostConfig, HostValidationError, Port};
pub use ids::{HostId, SessionId, TransferId};
pub use session::{Session, SessionFailure, SessionKind, SessionState, SessionTransitionError};
pub use shell::LocalShell;
pub use task::{
    Transfer, TransferDirection, TransferEvent, TransferFailure, TransferState, TransferUpdate,
    TransferUpdateError,
};
