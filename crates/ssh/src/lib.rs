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
//! * [session] runs one direct attempt and the remote shell;
//! * [chain] runs an ordered jump chain, one trust decision and one confirmed
//!   ProxyCommand per hop, and cleans up from the far end when a hop fails;
//! * [transport] is the byte pipe a direct socket, a jump channel and a
//!   ProxyCommand all become after the chain has decided to open them;
//! * [sftp] browses directories over the connection the shell already authenticated;
//! * [transfer] moves file contents over that same connection, under a temporary name;
//! * [scp] copies a file or one chosen directory with the traditional SCP protocol, for
//!   a server that offers no SFTP subsystem;
//! * [forwarding] binds local and dynamic listeners and asks the server for remote
//!   ones, on the connection that is already authenticated;
//! * [error] keeps every failure free of credentials.

pub mod agent;
pub mod auth;
mod chain;
pub mod error;
pub mod forwarding;
pub mod hostkey;
mod scp;
pub mod session;
pub mod sftp;
pub mod transfer;
mod transport;

/// A disposable OpenSSH server, for the tests that need a real one.
#[cfg(any(test, feature = "test-support"))]
pub mod testserver;

/// A session whose events come from a test rather than from a server.
#[cfg(feature = "test-support")]
pub use session::ScriptedSession;

pub use chain::{ChainEvent, ChainRequest, ChainSession, HopAuth};
pub use error::{ChainFailure, ConnectError, HopLocation};
pub use forwarding::{ForwardCommand, ForwardEvent, ForwardFailure, ForwardLink, ForwardSpec};
pub use hostkey::{HostKeyPrompt, KnownHostKey, PresentedKey};
pub use session::{
    CONNECT_TIMEOUT, ConnectRequest, PtySize, RemoteEvent, RemoteSession, TERM_VALUE, TrustAnswer,
};
pub use sftp::FileLink;
pub use transfer::{TransferCommand, TransferLink, TransferRequest};
