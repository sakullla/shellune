//! The one operation set a session needs from a port, and the boundary of the unsafe
//! code (R21).
//!
//! A [`SerialSession`](crate::SerialSession) never sees a Win32 handle, an
//! `OVERLAPPED` structure or a file descriptor. It sees this trait, whose four
//! operations are all safe to call: the system calls, the memory the driver writes
//! into and the cancellation of an in-flight operation stay inside them, and
//! the two platform backends, and the application layer above this crate only ever
//! holds safe types.
//!
//! The trait is public so a test can supply a port instead of a device - the crate's
//! own virtual pair is one - but nothing about it exposes a raw handle, so a fake
//! cannot make the production path bypass the system API: opening a real port always
//! goes through [`SerialSession::open`](crate::SerialSession::open) and the platform
//! backend.

use crate::error::SerialError;

/// Why a read or a write stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortFault {
    /// The operation was cancelled by [`PortIo::interrupt`]; the handle is still open.
    Interrupted,
    /// The port failed, and the session must end.
    Failed(SerialError),
}

/// One opened serial port, as a session drives it.
///
/// # Contracts
///
/// * [`PortIo::read`] returns as soon as at least one byte arrived, and reports an
///   interrupted wait as [`PortFault::Interrupted`] - it never returns `Ok(0)` for
///   "no data yet", so a session can tell an empty read from a wake-up.
/// * [`PortIo::write`] may accept fewer bytes than it was given (a driver with a full
///   transmit buffer does), and `Ok(0)` means "nothing was accepted this time". A
///   session keeps offering the remainder of the same frame, and never re-offers bytes
///   the port already accepted.
/// * [`PortIo::interrupt`] is callable from any thread, is idempotent, and wakes a
///   blocked read or write with [`PortFault::Interrupted`]. A driver that never
///   completes is a case the session reports instead of hiding, so an implementation
///   that cannot wake is allowed - the session then keeps the port in
///   [`SessionState::Closing`](crate::SessionState::Closing) instead of pretending it
///   was released.
/// * [`PortIo::release`] is called once, after both session threads have finished, so
///   an implementation can note that the handle is free.
pub trait PortIo: Send + Sync + 'static {
    /// The port name the user knows, such as `COM3` or `/dev/ttyUSB0`.
    fn name(&self) -> &str;

    /// Reads whatever arrived, waiting until at least one byte is there.
    ///
    /// # Errors
    ///
    /// [`PortFault::Interrupted`] when [`PortIo::interrupt`] cancelled the wait, or
    /// [`PortFault::Failed`] when the port reported a failure.
    fn read(&self, buf: &mut [u8]) -> Result<usize, PortFault>;

    /// Offers bytes to the port and returns how many it accepted.
    ///
    /// # Errors
    ///
    /// The same two outcomes as [`PortIo::read`].
    fn write(&self, buf: &[u8]) -> Result<usize, PortFault>;

    /// Stops any in-flight read or write without releasing the handle.
    fn interrupt(&self);

    /// Notes that the session has finished with this port and will not call it again.
    fn release(&self) {}
}
