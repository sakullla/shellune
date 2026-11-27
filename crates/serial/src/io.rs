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

/// Something the driver reported about bytes that never reached the session (R21).
///
/// A receive overrun is the one loss a serial port can suffer without any call
/// failing: the device kept sending while the driver's buffer was full, so bytes
/// that were on the wire were dropped before this crate could see them. Windows
/// reports it through `ClearCommError`, and the backend hands it over here so the
/// session can say so instead of presenting a gap as a quiet line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortNotice {
    /// Bytes the driver dropped, when the platform counts them.
    pub lost: Option<u64>,
    /// The platform code that reported it, kept so a diagnostic can be exact.
    pub code: Option<i32>,
    /// The platform's own words, when it has any.
    pub text: String,
}

impl PortNotice {
    /// A notice that the driver dropped bytes, with the code that said so.
    #[must_use]
    pub fn overrun(code: Option<i32>, lost: Option<u64>, text: impl Into<String>) -> Self {
        Self {
            lost,
            code,
            text: text.into(),
        }
    }
}

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
///
/// # Threading, and why `&self` is enough
///
/// The trait is `Send + Sync` because `interrupt` and `release` are called from the
/// session's own thread while a read or a write may be in flight in another - not
/// because two calls of the same operation may run at once. The session starts one
/// reader thread and one writer thread, and each of them is the only caller of its
/// own operation:
///
/// * [`PortIo::read`] is called by the reader thread only;
/// * [`PortIo::write`] is called by the writer thread only.
///
/// A backend may therefore keep **one completion event per direction** and reset it
/// at the start of each call, which is what the Windows backend does. That is only
/// sound while the rule above holds: two concurrent reads would share the one event
/// of the read direction, each would reset the other's signal, and a completed
/// operation would then be waited for a second time. An implementation that cannot
/// keep the rule - or that wants to be callable from several threads - must give
/// every operation its own event instead of reusing the direction's.
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

    /// Sends an RS-232 break signal on the open line.
    ///
    /// # Errors
    ///
    /// [`PortFault::Interrupted`] when cancelled, or [`PortFault::Failed`] when the
    /// port could not emit the break signal.
    fn send_break(&self) -> Result<(), PortFault> {
        Err(PortFault::Failed(
            SerialError::new(crate::error::SerialCause::UnsupportedConfig)
                .at(self.name())
                .with_detail("this port backend does not support sending a break signal"),
        ))
    }

    /// Notes that the session has finished with this port and will not call it again.
    fn release(&self) {}

    /// Takes the loss the driver reported since the last call, if it reported one.
    ///
    /// This is the receive overrun, and the default is "this platform does not say".
    /// A port that cannot tell must return [`None`] rather than a placeholder: an
    /// invented overrun would make the session print a loss that never happened, and
    /// staying silent about a real one is the lesser lie of the two. Unix keeps no
    /// equivalent counter in termios, so its backend takes the default.
    fn take_notice(&self) -> Option<PortNotice> {
        None
    }
}
