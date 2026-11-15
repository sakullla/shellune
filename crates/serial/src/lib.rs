//! Serial ports for Shellune, straight from the system (R17, R21).
//!
//! R21 asks for a real bidirectional serial session inside an application tab: discover
//! the ports, choose baud rate, data bits, parity, stop bits and flow control, send and
//! receive bytes, and be told why a port will not open. R17 asks that a session have one
//! real state, that a failure name the object it affected, and that closing or losing a
//! device release what it held without disturbing anything else.
//!
//! This crate is that backend and nothing else - no window, no theme, no terminal grid:
//!
//! * [`list_ports`] asks the system which ports exist;
//! * [`SerialConfig`] is the setting a user chooses, validated before a device is touched;
//! * [`SerialSession`] is one open port: a reader thread, a writer thread, a bounded send
//!   queue and a close that stops in-flight I/O before it releases the handle;
//! * [`SerialError`] names the cause the system allowed - a missing port, a port in use, a
//!   missing permission, settings the driver rejects, a device that went away - and keeps
//!   the causes it could not separate and the raw system code beside them;
//! * [`SerialEvent`] is what the session reports: received bytes, sent pieces, a frame
//!   that stopped part way, a disconnect, and the final release.
//!
//! Two rules hold throughout. Bytes are bytes: what the device sends is delivered
//! unchanged, including `0x00` and sequences that are not valid UTF-8, so a terminal above
//! this crate decides what text is. And a bound is a bound: a full queue refuses the
//! bytes instead of dropping them, a driver that accepts half a frame is reported half way
//! instead of being replayed, and a close that cannot stop the driver keeps the port open
//! and says so rather than reporting a release that did not happen.
//!
//! # Platforms
//!
//! * **Windows** uses the system's own serial API: `CreateFileW` on the device, the `DCB`
//!   for the line settings, overlapped reads and writes with a completion event each,
//!   `CancelIoEx` to stop an in-flight operation, and `HARDWARE\DEVICEMAP\SERIALCOMM` for
//!   the port list.
//! * **Unix** uses raw termios on the device, non-blocking reads and writes, `TIOCEXCL` so
//!   a second open reports "in use" instead of silently sharing the port, and a self-pipe
//!   polled beside the device so a close wakes a blocked read at once.
//!
//! The unsafe calls - the two platform backends and nothing else - never reach the caller:
//! every public type and function here is safe, and no handle, file descriptor or
//! `OVERLAPPED` structure appears in the API.
//!
//! # What is not verified here
//!
//! R21's real-device checks - hardware flow control against a second device, DTR/RTS
//! behaviour, a physical unplug, and reopening after a replug - need hardware and a
//! virtual null-modem pair. The tests in this repository drive [`testing`] instead: the
//! production path is the system API (which the port list and the missing-port test
//! exercise for real), and the byte, queue, partial-write and close cases are proven
//! against an in-memory port that follows the same [`PortIo`] contract. A test labelled
//! with a fake port is never a claim about a device.

#![deny(unsafe_op_in_unsafe_fn)]

mod cause;
pub mod config;
pub mod error;
pub mod io;
mod platform;
pub mod port;
pub mod session;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use config::{
    COMMON_BAUD_RATES, DataBits, FlowControl, MAX_BAUD_RATE, Parity, SerialConfig, StopBits,
};
pub use error::{RawCodeKind, RawError, SerialCause, SerialError};
pub use io::{PortFault, PortIo};
pub use port::{PortInfo, list_ports};
pub use session::{
    CloseOutcome, DEFAULT_CLOSE_TIMEOUT, DEFAULT_PENDING_RX_BYTES, DEFAULT_QUEUED_BYTES,
    DEFAULT_QUEUED_FRAMES, SerialEvent, SerialSession, SessionLimits, SessionState,
};
