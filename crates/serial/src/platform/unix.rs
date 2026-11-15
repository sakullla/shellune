//! The Unix port: raw termios on the device, non-blocking I/O, and a wake channel
//! (R21).
//!
//! What the recipe asks for is what this file does, and each part is here for a
//! reason:
//!
//! * the device is opened with `O_RDWR | O_NOCTTY | O_NONBLOCK`: the port is not this
//!   process's controlling terminal, and no read or write can block a thread that also
//!   has to notice a close;
//! * the line discipline is turned raw with termios directly - every `ICANON`/`ECHO`/
//!   `ISIG`/`OPOST` transformation off - so a received `0x0d`, `0x00` or a byte that
//!   is not valid UTF-8 reaches the terminal as the byte the device sent;
//! * `TIOCEXCL` claims the device for this process. Without it a second open would
//!   silently succeed and two sessions would take bytes from each other; with it the
//!   second open fails with `EBUSY`, which is how "in use" becomes a reason the user
//!   can be given instead of an empty terminal;
//! * a self-pipe is polled beside the port, so [`PortIo::interrupt`] wakes a blocked
//!   read immediately and without a signal handler or a timeout that would make a
//!   close take a second;
//! * every system call's failure goes through [`crate::cause::classify`], so the only
//!   thing this file decides is which meaning a code has.

use std::ffi::CString;
use std::os::fd::RawFd;
use std::sync::Arc;

use crate::cause::{Stage, SysCode, classify};
use crate::config::{FlowControl, SerialConfig};
use crate::error::{RawCodeKind, RawError, SerialCause, SerialError};
use crate::io::{PortFault, PortIo};
use crate::port::PortInfo;

/// An opened serial device.
pub(crate) struct UnixPort {
    fd: RawFd,
    wake_read: RawFd,
    wake_write: RawFd,
    name: String,
}

// SAFETY: the three descriptors are owned by this value alone, and the implementations
// below use them only through system calls that are safe to issue from several threads
// on a descriptor: a read and a write on a terminal are independent, and the wake pipe
// is written from one thread and drained from another.
unsafe impl Send for UnixPort {}
unsafe impl Sync for UnixPort {}

impl UnixPort {
    fn failure(&self, stage: Stage, errno: i32) -> SerialError {
        let (cause, also) = classify(stage, sys_code(errno));
        let mut error = SerialError::new(cause).at(self.name.clone());
        for extra in also {
            error = error.also(*extra);
        }
        error.with_raw(RawError::new(RawCodeKind::Errno, errno))
    }
}

impl UnixPort {
    /// The failure for a device that hung up or was removed while it was being read.
    fn hung_up(&self) -> SerialError {
        let (cause, also) = classify(Stage::Io, SysCode::DeviceGone);
        let mut error = SerialError::new(cause).at(self.name.clone());
        for extra in also {
            error = error.also(*extra);
        }
        error.with_detail("the device hung up")
    }
}

impl PortIo for UnixPort {
    fn name(&self) -> &str {
        &self.name
    }

    fn read(&self, buf: &mut [u8]) -> Result<usize, PortFault> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let mut fds = [
                libc::pollfd {
                    fd: self.fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.wake_read,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: fds points at two initialised pollfd values of the stated length,
            // and poll only writes into their revents field.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
            if ready < 0 {
                let errno = errno();
                if errno == libc::EINTR {
                    return Err(PortFault::Interrupted);
                }
                return Err(PortFault::Failed(self.failure(Stage::Io, errno)));
            }
            if fds[1].revents != 0 {
                drain(self.wake_read);
                return Err(PortFault::Interrupted);
            }
            let revents = fds[0].revents;
            if revents & libc::POLLNVAL != 0 {
                return Err(PortFault::Failed(
                    SerialError::new(SerialCause::NotFound)
                        .at(self.name.clone())
                        .with_detail("the descriptor is no longer valid"),
                ));
            }
            if revents & libc::POLLIN == 0 {
                if revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                    return Err(PortFault::Failed(self.hung_up()));
                }
                continue;
            }
            // SAFETY: buf is a valid mutable slice and the descriptor was opened by this
            // type; read writes at most buf.len() bytes into it.
            let read = unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
            if read > 0 {
                return Ok(read.unsigned_abs());
            }
            if read == 0 {
                // With VMIN=0 a read can report zero bytes even when poll reported input,
                // so a hang-up is only concluded when poll also says the device is gone.
                if revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                    return Err(PortFault::Failed(self.hung_up()));
                }
                continue;
            }
            let errno = errno();
            if errno == libc::EAGAIN || errno == libc::EWOULDBLOCK || errno == libc::EINTR {
                continue;
            }
            return Err(PortFault::Failed(self.failure(Stage::Io, errno)));
        }
    }

    fn write(&self, buf: &[u8]) -> Result<usize, PortFault> {
        if buf.is_empty() {
            return Ok(0);
        }
        // SAFETY: buf is a valid slice; write reads at most buf.len() bytes from it.
        let written = unsafe { libc::write(self.fd, buf.as_ptr().cast(), buf.len()) };
        if written >= 0 {
            return Ok(written.unsigned_abs());
        }
        let errno = errno();
        if errno == libc::EAGAIN || errno == libc::EWOULDBLOCK {
            return Ok(0);
        }
        if errno == libc::EINTR {
            return Err(PortFault::Interrupted);
        }
        Err(PortFault::Failed(self.failure(Stage::Io, errno)))
    }

    fn interrupt(&self) {
        // A wake byte that cannot be queued is not a failure: the pipe is only a hint,
        // and a cancellation that was already delivered is enough.
        let byte = [1u8];
        // SAFETY: the descriptor is owned by this value and the buffer has one byte.
        unsafe {
            libc::write(self.wake_write, byte.as_ptr().cast(), 1);
        }
    }
}

impl Drop for UnixPort {
    fn drop(&mut self) {
        // SAFETY: each descriptor is owned by this value and closed exactly once here.
        unsafe {
            libc::close(self.fd);
            libc::close(self.wake_read);
            libc::close(self.wake_write);
        }
    }
}

/// Opens a device with the given settings and claims it exclusively.
pub(crate) fn open(port: &str, config: &SerialConfig) -> Result<Arc<dyn PortIo>, SerialError> {
    config.validate()?;
    let path = device_path(port);
    let path_c = CString::new(path.clone()).map_err(|_| {
        SerialError::new(SerialCause::NotFound)
            .at(port)
            .with_detail("the port name contains a NUL byte")
    })?;
    // SAFETY: path_c is a valid NUL terminated string for the duration of the call.
    let fd = unsafe {
        libc::open(
            path_c.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(failure_for(port, Stage::Open, errno()));
    }
    // SAFETY: fd was just returned by open and is owned from here on.
    let device = Device { fd };

    // SAFETY: ioctl requests a claim on the descriptor this value owns, and the
    // TIOCEXCL request takes no further argument.
    let claimed = unsafe { libc::ioctl(device.fd, libc::TIOCEXCL as _) };
    if claimed < 0 {
        let errno = errno();
        // A second open of a device another process claimed fails here, which is the one
        // place Unix can say "in use" instead of sharing the port silently.
        if errno == libc::EBUSY {
            return Err(SerialError::new(SerialCause::InUse)
                .at(port)
                .with_detail("another program has claimed this port")
                .with_raw(RawError::new(RawCodeKind::Errno, errno)));
        }
        return Err(failure_for(port, Stage::Open, errno)
            .with_detail("the port could not be claimed exclusively"));
    }

    let (wake_read, wake_write) = make_pipe(port)?;
    let port = UnixPort {
        fd: device.fd,
        wake_read,
        wake_write,
        name: port.to_string(),
    };
    std::mem::forget(device);
    configure(&port, config)?;
    Ok(Arc::new(port))
}

/// A file descriptor this function owns until the caller takes it.
struct Device {
    fd: RawFd,
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the descriptor is owned by this value and closed exactly once.
        unsafe {
            libc::close(self.fd);
        }
    }
}

fn configure(port: &UnixPort, config: &SerialConfig) -> Result<(), SerialError> {
    // SAFETY: termios is plain data and the descriptor is owned by the port.
    let mut settings: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: settings is a valid termios for tcgetattr to write into.
    if unsafe { libc::tcgetattr(port.fd, &mut settings) } < 0 {
        let errno = errno();
        let error = port.failure(Stage::Configure, errno);
        // A path that is not a character device at all is reported as what it is: not a
        // serial port, rather than a setting problem.
        if errno == libc::ENOTTY {
            return Err(SerialError::new(SerialCause::NotFound)
                .at(port.name())
                .with_detail("the path is not a terminal device")
                .with_raw(RawError::new(RawCodeKind::Errno, errno)));
        }
        return Err(error);
    }

    let speed = speed_constant(config.baud()).ok_or_else(|| {
        SerialError::new(SerialCause::UnsupportedConfig)
            .at(port.name())
            .with_detail(format!(
                "termios has no constant for {} baud on this platform",
                config.baud()
            ))
    })?;

    settings.c_cflag &= !(libc::CSIZE | libc::PARENB | libc::PARODD | libc::CSTOPB);
    settings.c_cflag |= libc::CLOCAL | libc::CREAD;
    settings.c_cflag |= match config.data_bits() {
        crate::config::DataBits::Five => libc::CS5,
        crate::config::DataBits::Six => libc::CS6,
        crate::config::DataBits::Seven => libc::CS7,
        crate::config::DataBits::Eight => libc::CS8,
    };
    match config.parity() {
        crate::config::Parity::None => {}
        crate::config::Parity::Odd => settings.c_cflag |= libc::PARENB | libc::PARODD,
        crate::config::Parity::Even => settings.c_cflag |= libc::PARENB,
        crate::config::Parity::Mark | crate::config::Parity::Space => {
            return Err(SerialError::new(SerialCause::UnsupportedConfig)
                .at(port.name())
                .with_detail("termios on this platform cannot generate mark or space parity"));
        }
    }
    if config.stop_bits() == crate::config::StopBits::Two {
        settings.c_cflag |= libc::CSTOPB;
    }
    apply_flow(&mut settings, config.flow(), port.name())?;

    // Raw in both directions: no line editing, no echo, no signal keys, no CR/LF
    // translation, no character stripping and no output post-processing.
    settings.c_lflag &= !(libc::ICANON
        | libc::ECHO
        | libc::ECHOE
        | libc::ECHOK
        | libc::ECHONL
        | libc::ISIG
        | libc::IEXTEN);
    settings.c_iflag &= !(libc::INLCR
        | libc::IGNCR
        | libc::ICRNL
        | libc::ISTRIP
        | libc::BRKINT
        | libc::PARMRK
        | libc::INPCK
        | libc::IGNBRK);
    settings.c_oflag &= !libc::OPOST;
    if config.flow() == FlowControl::Software {
        settings.c_iflag |= libc::IXON | libc::IXOFF;
    }
    settings.c_cc[libc::VMIN] = 0;
    settings.c_cc[libc::VTIME] = 0;

    // SAFETY: settings is a fully initialised termios for the descriptor this owns.
    if unsafe { libc::cfsetispeed(&mut settings, speed) } < 0
        || unsafe { libc::cfsetospeed(&mut settings, speed) } < 0
    {
        return Err(port.failure(Stage::Configure, errno()));
    }
    // SAFETY: same descriptor, same initialised termios, applied immediately.
    if unsafe { libc::tcsetattr(port.fd, libc::TCSANOW, &settings) } < 0 {
        return Err(port.failure(Stage::Configure, errno()));
    }
    // Bytes that arrived before the settings were applied are not this session's:
    // discarding them keeps a reopen from showing output the user already saw.
    // SAFETY: the descriptor is owned by the port and TCIOFLUSH takes no buffer.
    unsafe {
        libc::tcflush(port.fd, libc::TCIOFLUSH);
    }
    Ok(())
}

fn apply_flow(
    settings: &mut libc::termios,
    flow: FlowControl,
    port: &str,
) -> Result<(), SerialError> {
    settings.c_iflag &= !(libc::IXON | libc::IXOFF | libc::IXANY);
    match hardware_flow_bits() {
        Some(bits) => settings.c_cflag &= !bits,
        None if flow == FlowControl::Hardware => {
            return Err(SerialError::new(SerialCause::UnsupportedConfig)
                .at(port)
                .with_detail("this platform cannot request RTS/CTS flow control"));
        }
        None => {}
    }
    match flow {
        FlowControl::None => {}
        FlowControl::Software => settings.c_iflag |= libc::IXON | libc::IXOFF,
        FlowControl::Hardware => {
            if let Some(bits) = hardware_flow_bits() {
                settings.c_cflag |= bits;
            }
        }
    }
    Ok(())
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
fn hardware_flow_bits() -> Option<libc::tcflag_t> {
    Some(libc::CRTSCTS)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
)))]
fn hardware_flow_bits() -> Option<libc::tcflag_t> {
    None
}

/// The termios constant for a baud rate, or `None` when this platform has none.
fn speed_constant(baud: u32) -> Option<libc::speed_t> {
    let speed = match baud {
        50 => libc::B50,
        75 => libc::B75,
        110 => libc::B110,
        134 => libc::B134,
        150 => libc::B150,
        200 => libc::B200,
        300 => libc::B300,
        600 => libc::B600,
        1200 => libc::B1200,
        1800 => libc::B1800,
        2400 => libc::B2400,
        4800 => libc::B4800,
        9600 => libc::B9600,
        19_200 => libc::B19200,
        38_400 => libc::B38400,
        57_600 => libc::B57600,
        115_200 => libc::B115200,
        230_400 => libc::B230400,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        460_800 => libc::B460800,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        500_000 => libc::B500000,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        576_000 => libc::B576000,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        921_600 => libc::B921600,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        1_000_000 => libc::B1000000,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        1_152_000 => libc::B1152000,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        1_500_000 => libc::B1500000,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        2_000_000 => libc::B2000000,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        2_500_000 => libc::B2500000,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        3_000_000 => libc::B3000000,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        3_500_000 => libc::B3500000,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        4_000_000 => libc::B4000000,
        _ => return None,
    };
    Some(speed)
}

/// Lists the serial devices the system describes under `/dev` and `/sys`.
pub(crate) fn list() -> Result<Vec<PortInfo>, SerialError> {
    let entries = std::fs::read_dir("/dev").map_err(|error| {
        SerialError::new(SerialCause::Enumerate)
            .at("/dev")
            .with_detail(error.to_string())
            .with_raw(RawError::new(
                RawCodeKind::Errno,
                error.raw_os_error().unwrap_or(0),
            ))
    })?;
    let mut ports = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !looks_like_serial_port(&name) {
            continue;
        }
        let path = entry.path().to_string_lossy().into_owned();
        ports.push(PortInfo::new(name.clone(), path, sys_description(&name)));
    }
    ports.sort_by(|left, right| left.name().cmp(right.name()));
    Ok(ports)
}

/// Whether a `/dev` entry is the sort of name a serial adapter has.
///
/// The names come from the drivers that create them: `ttyS` for a built-in UART,
/// `ttyUSB`/`ttyACM`/`ttyAMA` for adapters, `cu.*` for the call-out devices on BSD and
/// macOS, and `rfcomm` for Bluetooth serial. The list is a name filter, not a claim that
/// the device works: opening it is what decides that.
fn looks_like_serial_port(name: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "ttyS", "ttyUSB", "ttyACM", "ttyAMA", "ttyXRUSB", "ttyAP", "ttyGS", "rfcomm", "cu.",
    ];
    PREFIXES.iter().any(|prefix| name.starts_with(prefix))
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
}

/// The driver name `/sys` records for a device, when it records one.
fn sys_description(name: &str) -> Option<String> {
    let driver = std::fs::read_link(format!("/sys/class/tty/{name}/device/driver")).ok()?;
    driver
        .file_name()
        .map(|part| part.to_string_lossy().into_owned())
}

/// The path a port name opens by.
fn device_path(port: &str) -> String {
    if port.starts_with('/') {
        port.to_string()
    } else {
        format!("/dev/{port}")
    }
}

fn failure_for(port: &str, stage: Stage, errno: i32) -> SerialError {
    let (cause, also) = classify(stage, sys_code(errno));
    let mut error = SerialError::new(cause)
        .at(port)
        .with_raw(RawError::new(RawCodeKind::Errno, errno));
    for extra in also {
        error = error.also(*extra);
    }
    error
}

/// The shared meaning of an `errno` value.
fn sys_code(errno: i32) -> SysCode {
    match errno {
        libc::ENOENT | libc::ENOTDIR | libc::ENOTTY => SysCode::NoSuchDevice,
        libc::EACCES | libc::EPERM => SysCode::AccessDenied,
        libc::EBUSY => SysCode::Busy,
        libc::EINVAL => SysCode::InvalidParameter,
        libc::ENXIO | libc::ENODEV => SysCode::DeviceGone,
        libc::EIO => SysCode::NotFunctioning,
        libc::EINTR => SysCode::Aborted,
        _ => SysCode::Other,
    }
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// A pipe whose read end never blocks the poll that watches it.
fn make_pipe(port: &str) -> Result<(RawFd, RawFd), SerialError> {
    let mut fds = [0 as RawFd; 2];
    #[cfg(any(target_os = "linux", target_os = "android"))]
    // SAFETY: fds is space for the two descriptors pipe2 writes.
    let created = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    // SAFETY: fds is space for the two descriptors pipe writes; the flags are then set
    // with fcntl because this system has no pipe2.
    let created = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if created < 0 {
        return Err(failure_for(port, Stage::Open, errno())
            .with_detail("the wake channel every session needs could not be created"));
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    for fd in fds {
        // SAFETY: both descriptors were just created by pipe and are owned here.
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
        }
    }
    Ok((fds[0], fds[1]))
}

/// Empties the wake pipe so the next wait starts from a quiet channel.
fn drain(fd: RawFd) {
    let mut buffer = [0u8; 64];
    loop {
        // SAFETY: buffer is a valid destination for up to its own length.
        let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if read <= 0 {
            break;
        }
    }
}
