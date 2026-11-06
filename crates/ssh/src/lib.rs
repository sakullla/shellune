//! SSH transport for Shellune: host trust, authentication and a remote PTY (R3, R4, R6, R8).
//!
//! The crate is render-independent and storage-independent on purpose. It executes
//! one connection attempt, reports what happened as events a window can poll, and
//! takes its trust decision from the caller, so the same code runs inside the
//! application, inside an integration test against a real OpenSSH server and inside
//! the unit tests of this crate.
//!
//! * [hostkey] turns a presented key into the algorithm and SHA-256 fingerprint the
//!   user compares;
//! * [auth] reads a private key file, decrypting it when a passphrase was given;
//! * [agent] reaches SSH_AUTH_SOCK, the Windows OpenSSH pipe or Pageant;
//! * [session] runs the attempt and the remote shell;
//! * [sftp] browses directories over the connection the shell already authenticated;
//! * [transfer] moves file contents over that same connection, under a temporary name;
//! * [error] keeps every failure free of credentials.

pub mod agent;
pub mod auth;
pub mod error;
pub mod hostkey;
pub mod session;
pub mod sftp;
pub mod transfer;

/// A disposable OpenSSH server, for the tests that need a real one.
#[cfg(any(test, feature = "test-support"))]
pub mod testserver;

/// A session whose events come from a test rather than from a server.
#[cfg(feature = "test-support")]
pub use session::ScriptedSession;

pub use error::ConnectError;
pub use hostkey::{HostKeyPrompt, KnownHostKey, PresentedKey};
pub use session::{
    CONNECT_TIMEOUT, ConnectRequest, PtySize, RemoteEvent, RemoteSession, TERM_VALUE, TrustAnswer,
};
pub use sftp::FileLink;
pub use transfer::{TransferCommand, TransferLink, TransferRequest};
