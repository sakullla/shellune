//! The Windows port: the system's own serial API (R21).
//!
//! The recipe names the pieces, and each is used the way the system documents it:
//!
//! * the device is opened with `CreateFileW` on `\\.\COMn` with no sharing, so a second
//!   open fails instead of two sessions stealing each other's bytes, and with
//!   `FILE_FLAG_OVERLAPPED` so no read or write can block the thread that has to notice
//!   a close;
//! * the line settings are the system's own `DCB` structure: baud rate, byte size,
//!   parity, stop bits, and the flow-control and DTR/RTS bits inside its bitfield. A
//!   driver that refuses them is reported as an unsupported setting, with its code;
//! * every read and write is an overlapped operation with its own completion event, so
//!   a wait is `WaitForMultipleObjects` over "the operation finished" and "the session
//!   gave up" - never a sleep that would make a close take as long as a timeout;
//! * `CancelIoEx` is what stops an in-flight operation. After cancelling, the completion
//!   is reaped with `GetOverlappedResult` before the buffer leaves scope, so the driver
//!   can never write into memory this session no longer owns;
//! * the port list is `HARDWARE\DEVICEMAP\SERIALCOMM`, the device map Windows keeps for
//!   exactly this question.
//!
//! What this file cannot do is prove a device accepts these settings: that needs a real
//! adapter, and it is recorded as unverified rather than assumed.

use std::ptr;
use std::sync::Arc;

use windows_sys::Win32::Devices::Communication::{
    CE_OVERRUN, CE_RXOVER, COMMTIMEOUTS, COMSTAT, ClearCommError, DCB, EVENPARITY, GetCommState,
    MARKPARITY, NOPARITY, ODDPARITY, ONESTOPBIT, PURGE_RXCLEAR, PURGE_TXCLEAR, PurgeComm,
    SPACEPARITY, SetCommState, SetCommTimeouts, SetupComm, TWOSTOPBITS,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_DEV_NOT_EXIST, ERROR_DEVICE_NOT_CONNECTED,
    ERROR_FILE_NOT_FOUND, ERROR_GEN_FAILURE, ERROR_INVALID_HANDLE, ERROR_INVALID_NAME,
    ERROR_INVALID_PARAMETER, ERROR_IO_PENDING, ERROR_NO_MORE_ITEMS, ERROR_OPERATION_ABORTED,
    ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION, GENERIC_READ, GENERIC_WRITE, GetLastError,
    HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegEnumValueW, RegOpenKeyExW, RegQueryInfoKeyW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects,
};

use crate::cause::{Stage, SysCode, classify};
use crate::config::{DataBits, FlowControl, Parity, SerialConfig, StopBits};
use crate::error::{RawCodeKind, RawError, SerialCause, SerialError};
use crate::io::{PortFault, PortIo};
use crate::port::PortInfo;

/// The bits inside `DCB::_bitfield` that this crate sets.
///
/// `DCB` is a bitfield structure in the C header; Windows Sys hands it over as a `u32`,
/// so the positions are named here. They are the documented ones: bit 0 `fBinary`, 1
/// `fParity`, 2 `fOutxCtsFlow`, 4-5 `fDtrControl`, 8 `fOutX`, 9 `fInX`, 12-13
/// `fRtsControl`; the two multi-bit fields are only ever given a documented value.
mod bits {
    /// Binary mode, which is the only mode a modern driver supports.
    pub(super) const FBINARY: u32 = 0x0000_0001;
    /// Report parity errors on the input.
    pub(super) const FPARITY: u32 = 0x0000_0002;
    /// Hold transmission while CTS is low.
    pub(super) const FOUTXCTSFLOW: u32 = 0x0000_0004;
    /// `DTR_CONTROL_ENABLE`: DTR stays asserted.
    pub(super) const DTR_CONTROL_ENABLE: u32 = 0x0000_0010;
    /// XON/XOFF on output.
    pub(super) const FOUTX: u32 = 0x0000_0100;
    /// XON/XOFF on input.
    pub(super) const FINX: u32 = 0x0000_0200;
    /// `RTS_CONTROL_ENABLE`: RTS stays asserted.
    pub(super) const RTS_CONTROL_ENABLE: u32 = 0x0000_1000;
    /// `RTS_CONTROL_HANDSHAKE`: the driver drives RTS with the receive buffer.
    pub(super) const RTS_CONTROL_HANDSHAKE: u32 = 0x0000_2000;
}

/// A Win32 handle this value owns.
struct Handle(HANDLE);

// SAFETY: a handle is a reference to a kernel object that belongs to the process, and
// the operations this crate issues on it are documented as callable from any thread.
// The one structure that must not be shared - an OVERLAPPED - is created inside each
// call, and each direction has its own completion event.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this value and is closed exactly once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// An opened serial device and the three events its session waits on.
pub(crate) struct WindowsPort {
    handle: Handle,
    read_event: Handle,
    write_event: Handle,
    cancel_event: Handle,
    name: String,
    /// The receive loss the driver reported and the session has not been told yet.
    ///
    /// A serial driver drops bytes silently when its buffer overruns; the only place
    /// it says so is `ClearCommError`, whose error mask is cleared by the very call
    /// that reads it. The mask is therefore taken right after every read and kept
    /// until the session asks, so a loss is neither repeated nor lost.
    overrun: std::sync::Mutex<Option<crate::io::PortNotice>>,
}

impl WindowsPort {
    /// The failure a system code means at this stage, with its code and message kept.
    fn failure(&self, stage: Stage, code: u32) -> SerialError {
        let (cause, also) = classify(stage, sys_code(code));
        let mut error = SerialError::new(cause).at(self.name.clone());
        for extra in also {
            error = error.also(*extra);
        }
        error.with_raw(RawError::new(RawCodeKind::Win32, code as i32))
    }
}

impl PortIo for WindowsPort {
    fn name(&self) -> &str {
        &self.name
    }

    fn read(&self, buf: &mut [u8]) -> Result<usize, PortFault> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut overlapped = OVERLAPPED {
            hEvent: self.read_event.0,
            ..OVERLAPPED::default()
        };
        // SAFETY: the event is owned by this value and resetting it before an operation
        // is what keeps the completion event from being read as a stale signal.
        unsafe {
            ResetEvent(self.read_event.0);
        }
        let mut transferred = 0u32;
        // SAFETY: buf is a valid buffer that outlives the call, and the operation either
        // completes inside this call or is waited for below before buf leaves scope.
        let started = unsafe {
            ReadFile(
                self.handle.0,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut transferred,
                &mut overlapped,
            )
        };
        if started != 0 {
            // A driver that completes an operation immediately can still have dropped
            // bytes while the last read was being processed, so the error mask is
            // asked for on every completed read - not only on the waiting path.
            self.note_driver_errors();
            return Ok(transferred as usize);
        }
        let code = last_error();
        if code != ERROR_IO_PENDING {
            return Err(self.fault(code));
        }
        let read = self.wait(&overlapped, self.read_event.0, &mut transferred);
        self.note_driver_errors();
        read
    }

    fn write(&self, buf: &[u8]) -> Result<usize, PortFault> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut overlapped = OVERLAPPED {
            hEvent: self.write_event.0,
            ..OVERLAPPED::default()
        };
        // SAFETY: as in read: the event is owned here and reset before the operation.
        unsafe {
            ResetEvent(self.write_event.0);
        }
        let mut transferred = 0u32;
        // SAFETY: buf outlives this call or the operation is waited for before returning.
        let started = unsafe {
            WriteFile(
                self.handle.0,
                buf.as_ptr(),
                buf.len() as u32,
                &mut transferred,
                &mut overlapped,
            )
        };
        if started != 0 {
            return Ok(transferred as usize);
        }
        let code = last_error();
        if code != ERROR_IO_PENDING {
            return Err(self.fault(code));
        }
        self.wait(&overlapped, self.write_event.0, &mut transferred)
    }

    fn interrupt(&self) {
        // Cancelling every operation of this handle is what stops a read that is waiting
        // for a byte that will never come; the cancel event then makes the wait return
        // at once, and the completion is reaped inside wait.
        // SAFETY: the handle is owned by this value; a null OVERLAPPED means "all
        // operations this process started on the handle".
        unsafe {
            CancelIoEx(self.handle.0, ptr::null());
            SetEvent(self.cancel_event.0);
        }
    }

    /// The receive loss the driver reported, for the session to state.
    fn take_notice(&self) -> Option<crate::io::PortNotice> {
        self.overrun
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

impl WindowsPort {
    /// Waits for the completion event or for the cancel event, then reaps the operation.
    fn wait(
        &self,
        overlapped: &OVERLAPPED,
        completion: HANDLE,
        transferred: &mut u32,
    ) -> Result<usize, PortFault> {
        let handles = [completion, self.cancel_event.0];
        // SAFETY: both handles are owned by this value and the array outlives the call.
        let index = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
        if index == WAIT_OBJECT_0 {
            // SAFETY: the operation this waits for was started on this handle and is
            // complete or failed, so the result can be read without waiting.
            let finished =
                unsafe { GetOverlappedResult(self.handle.0, overlapped, transferred, 0) };
            if finished != 0 {
                return Ok(*transferred as usize);
            }
            return Err(self.fault(last_error()));
        }
        // One branch for every outcome that is not "the operation completed": the
        // cancel event, and a wait that failed outright. Both leave the operation in
        // an unknown state, and in both the buffer the caller handed in is still the
        // one the driver may write into. It is therefore cancelled and reaped before
        // this function returns, so the caller's buffer cannot leave scope while the
        // driver still owns the transfer - the same order the closing path uses.
        let code = last_error();
        // SAFETY: the overlapped structure belongs to this call and the handle is
        // owned by this value.
        unsafe {
            CancelIoEx(self.handle.0, overlapped);
            let mut reaped = 0u32;
            GetOverlappedResult(self.handle.0, overlapped, &mut reaped, 1);
        }
        if index == WAIT_OBJECT_0 + 1 {
            return Err(PortFault::Interrupted);
        }
        Err(PortFault::Failed(
            self.failure(Stage::Io, code)
                .with_detail("waiting for the device did not succeed"),
        ))
    }

    /// Takes whatever `ClearCommError` says the driver dropped since the last read.
    ///
    /// The call is the only way to learn about a receive overrun, and it clears the
    /// mask as it reports it, which is exactly the once-only delivery the session
    /// wants. Its own failure is ignored: a port that cannot answer this question is
    /// still a working port, and inventing a loss would be worse than not knowing.
    fn note_driver_errors(&self) {
        let mut errors = 0u32;
        let mut status: COMSTAT = unsafe { std::mem::zeroed() };
        // SAFETY: the handle is owned by this value, and both out-parameters are valid
        // destinations of their own type for the duration of the call.
        let asked = unsafe { ClearCommError(self.handle.0, &mut errors, &mut status) };
        if asked == 0 {
            return;
        }
        // Only the receive side is reported: a frame or parity error is the line
        // condition of a byte that did arrive, which the bytes themselves already
        // show, while these two mean bytes that arrived and were then dropped.
        let overrun = errors & (CE_RXOVER | CE_OVERRUN);
        if overrun == 0 {
            return;
        }
        let notice = crate::io::PortNotice::overrun(
            Some(overrun as i32),
            None,
            "the driver's receive buffer overran and dropped bytes",
        );
        let mut held = self
            .overrun
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = Some(notice);
    }

    /// Turns a system code into the fault a session understands.
    fn fault(&self, code: u32) -> PortFault {
        // ERROR_OPERATION_ABORTED is what a cancelled operation reports; it is a
        // cancellation, not a failure of the device.
        if code == ERROR_OPERATION_ABORTED {
            return PortFault::Interrupted;
        }
        PortFault::Failed(self.failure(Stage::Io, code))
    }
}

/// Opens a port with the given settings.
pub(crate) fn open(port: &str, config: &SerialConfig) -> Result<Arc<dyn PortIo>, SerialError> {
    config.validate()?;
    let path = device_path(port);
    let wide = wide(&path);
    // SAFETY: wide is a NUL terminated UTF-16 path that outlives the call; no security
    // attributes are given and no template file is used.
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(failure_for(port, Stage::Open, last_error()));
    }
    let handle = Handle(raw);

    // A larger buffer than the default keeps a burst from a fast device from being
    // reported as an overrun before the reader thread collects it. Failure is not fatal:
    // the driver then keeps its own size, which is still a working port.
    // SAFETY: the handle is owned by this value and the two sizes are plain integers.
    unsafe {
        SetupComm(handle.0, 64 * 1024, 64 * 1024);
        PurgeComm(handle.0, PURGE_RXCLEAR | PURGE_TXCLEAR);
    }

    apply_config(&handle, port, config)?;
    set_timeouts(&handle, port)?;

    let read_event = Handle(create_event(port)?);
    let write_event = Handle(create_event(port)?);
    let cancel_event = Handle(create_event(port)?);

    Ok(Arc::new(WindowsPort {
        handle,
        read_event,
        write_event,
        cancel_event,
        name: port.to_string(),
        overrun: std::sync::Mutex::new(None),
    }))
}

fn apply_config(handle: &Handle, port: &str, config: &SerialConfig) -> Result<(), SerialError> {
    // SAFETY: dcb is plain data the call fills in for the handle this value owns.
    let mut dcb: DCB = unsafe { std::mem::zeroed() };
    dcb.DCBlength = std::mem::size_of::<DCB>() as u32;
    // SAFETY: the handle is owned here and dcb is a valid DCB for the call to fill.
    if unsafe { GetCommState(handle.0, &mut dcb) } == 0 {
        return Err(failure_for(port, Stage::Configure, last_error()));
    }

    dcb.BaudRate = config.baud();
    dcb.ByteSize = match config.data_bits() {
        DataBits::Five => 5,
        DataBits::Six => 6,
        DataBits::Seven => 7,
        DataBits::Eight => 8,
    };
    dcb.Parity = match config.parity() {
        Parity::None => NOPARITY,
        Parity::Odd => ODDPARITY,
        Parity::Even => EVENPARITY,
        Parity::Mark => MARKPARITY,
        Parity::Space => SPACEPARITY,
    };
    dcb.StopBits = match config.stop_bits() {
        StopBits::One => ONESTOPBIT,
        StopBits::Two => TWOSTOPBITS,
    };

    let mut flags = bits::FBINARY | bits::DTR_CONTROL_ENABLE;
    if config.parity() != Parity::None {
        flags |= bits::FPARITY;
    }
    match config.flow() {
        FlowControl::None => {
            flags |= bits::RTS_CONTROL_ENABLE;
        }
        FlowControl::Software => {
            flags |= bits::FOUTX | bits::FINX | bits::RTS_CONTROL_ENABLE;
            dcb.XonChar = 0x11;
            dcb.XoffChar = 0x13;
            dcb.XonLim = 2048;
            dcb.XoffLim = 2048;
        }
        FlowControl::Hardware => {
            flags |= bits::FOUTXCTSFLOW | bits::RTS_CONTROL_HANDSHAKE;
        }
    }
    dcb._bitfield = flags;

    // SAFETY: the handle is owned here and dcb is a fully filled DCB.
    if unsafe { SetCommState(handle.0, &dcb) } == 0 {
        let code = last_error();
        return Err(failure_for(port, Stage::Configure, code)
            .with_detail(format!("the driver refused {}", config.summary())));
    }
    Ok(())
}

fn set_timeouts(handle: &Handle, port: &str) -> Result<(), SerialError> {
    let timeouts = COMMTIMEOUTS {
        // "Return at once if bytes are waiting, otherwise wait a short moment for the
        // first one": `ReadIntervalTimeout = MAXDWORD` with a multiplier of zero is the
        // driver's own way of saying that, and the constant is what keeps the reader from
        // asking the driver a thousand times a second while a device is idle. The wait is
        // still cancellable, because `CancelIoEx` ends it immediately.
        ReadIntervalTimeout: u32::MAX,
        ReadTotalTimeoutMultiplier: 0,
        ReadTotalTimeoutConstant: 100,
        // A write has no driver-side timeout: the queue is the bound, and a close cancels
        // a write that cannot proceed.
        WriteTotalTimeoutMultiplier: 0,
        WriteTotalTimeoutConstant: 0,
    };
    // SAFETY: the handle is owned here and the timeouts structure is fully initialised.
    if unsafe { SetCommTimeouts(handle.0, &timeouts) } == 0 {
        return Err(failure_for(port, Stage::Configure, last_error()));
    }
    Ok(())
}

fn create_event(port: &str) -> Result<HANDLE, SerialError> {
    // SAFETY: no attributes, manual reset off (a completion event is consumed by the
    // wait that observes it), initially unsignalled, unnamed.
    let event = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };
    if event.is_null() {
        return Err(failure_for(port, Stage::Open, last_error())
            .with_detail("a completion event could not be created"));
    }
    Ok(event)
}

/// Lists the ports in the device map Windows keeps for serial ports.
pub(crate) fn list() -> Result<Vec<PortInfo>, SerialError> {
    let subkey = wide("HARDWARE\\DEVICEMAP\\SERIALCOMM");
    let mut key: HKEY = ptr::null_mut();
    // SAFETY: subkey is a NUL terminated UTF-16 string and key is a valid destination.
    let status =
        unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, subkey.as_ptr(), 0, KEY_READ, &mut key) };
    if status == ERROR_FILE_NOT_FOUND {
        // No serial ports at all: the key is only created when one exists.
        return Ok(Vec::new());
    }
    if status != 0 {
        return Err(enumerate_failure(status));
    }
    let key = Key(key);

    let mut values = 0u32;
    let mut longest_name = 0u32;
    let mut longest_data = 0u32;
    // SAFETY: every pointer is either a valid destination of its own type or null,
    // which the call accepts for the parts that are not needed.
    let status = unsafe {
        RegQueryInfoKeyW(
            key.0,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut values,
            &mut longest_name,
            &mut longest_data,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if status != 0 {
        return Err(enumerate_failure(status));
    }

    let mut ports = Vec::new();
    for index in 0..values {
        let mut name = vec![0u16; longest_name as usize + 1];
        let mut name_len = name.len() as u32;
        let mut data = vec![0u8; longest_data as usize + 2];
        let mut data_len = data.len() as u32;
        // SAFETY: both buffers are as long as the lengths passed with them, and the
        // index is below the value count the call just reported.
        let status = unsafe {
            RegEnumValueW(
                key.0,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                ptr::null(),
                ptr::null_mut(),
                data.as_mut_ptr(),
                &mut data_len,
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status != 0 {
            return Err(enumerate_failure(status));
        }
        // The data of a SERIALCOMM value is a NUL terminated UTF-16 device name.
        let utf16: Vec<u16> = data[..data_len as usize]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        let name = String::from_utf16_lossy(&utf16);
        let name = name.trim_end_matches('\0').to_string();
        if name.is_empty() {
            continue;
        }
        let path = device_path(&name);
        ports.push(PortInfo::new(name, path, None));
    }
    ports.sort_by(|left, right| {
        port_number(left.name())
            .cmp(&port_number(right.name()))
            .then_with(|| left.name().cmp(right.name()))
    });
    Ok(ports)
}

/// The registry key this function owns.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key is owned by this value and is closed exactly once.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

/// The number inside a `COMn` name, for sorting.
fn port_number(name: &str) -> u32 {
    name.trim_start_matches("COM")
        .parse::<u32>()
        .unwrap_or(u32::MAX)
}

/// The path a port name opens by.
///
/// `CreateFileW` resolves `COM10` and above only through the `\\.\` prefix, and the
/// prefix is harmless for a single digit port, so it is always used.
fn device_path(port: &str) -> String {
    if port.starts_with("\\\\.\\") || port.starts_with("\\\\?\\") {
        port.to_string()
    } else {
        format!("\\\\.\\{port}")
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error() -> u32 {
    // SAFETY: GetLastError takes no arguments and only reads thread state.
    unsafe { GetLastError() }
}

fn enumerate_failure(code: u32) -> SerialError {
    SerialError::new(SerialCause::Enumerate)
        .with_raw(RawError::new(RawCodeKind::Win32, code as i32))
}

fn failure_for(port: &str, stage: Stage, code: u32) -> SerialError {
    let (cause, also) = classify(stage, sys_code(code));
    let mut error = SerialError::new(cause)
        .at(port)
        .with_raw(RawError::new(RawCodeKind::Win32, code as i32));
    for extra in also {
        error = error.also(*extra);
    }
    error
}

/// The shared meaning of a Windows error code.
fn sys_code(code: u32) -> SysCode {
    match code {
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND | ERROR_INVALID_NAME => SysCode::NoSuchDevice,
        ERROR_ACCESS_DENIED => SysCode::AccessDenied,
        ERROR_SHARING_VIOLATION => SysCode::Busy,
        ERROR_INVALID_PARAMETER => SysCode::InvalidParameter,
        ERROR_DEVICE_NOT_CONNECTED | ERROR_DEV_NOT_EXIST | ERROR_INVALID_HANDLE => {
            SysCode::DeviceGone
        }
        ERROR_GEN_FAILURE => SysCode::NotFunctioning,
        ERROR_OPERATION_ABORTED => SysCode::Aborted,
        _ => SysCode::Other,
    }
}
