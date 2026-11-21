//! Shellune's own VNC client: RFB over its own socket, with no protocol library
//! underneath.
//!
//! The handle keeps the crate's shape: configuration is copied into a worker that
//! owns the session, input and clipboard commands travel to it over a channel, and a
//! cancel shuts the socket down so the worker leaves its read promptly.
mod clipboard;
mod handshake;
mod session;
mod wire;
mod zrle;

pub(crate) use clipboard::{latin1_to_utf8, utf8_to_latin1};

use crate::control::{Control, Emit, publish};
use crate::input::{self, Sender};
use crate::{
    ClipboardEntry, ClipboardKind, Error, Event, Failure, MAX_CLIPBOARD_TEXT_BYTES, Notice, State,
    cut_clipboard_text,
};
use std::io;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A session dropped by the server is retried once before it is reported.
const ATTEMPTS: u8 = 2;

/// How long a drop waits for the worker before leaving it to finish on its own.
///
/// The worker can be parked in a name lookup or a multi-address connect that has
/// no timeout of its own, and the drop runs on the thread that owns the handle
/// (the UI thread). The wait is bounded so that thread is never held longer.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

/// What the host asked the session to configure before it connected.
pub(super) struct Config {
    pub(super) certificate_pin: Option<String>,
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) password: String,
    /// Which security types may be negotiated: the crate's VncSecurity bits.
    pub(super) security: u32,
    pub(super) width: u16,
    pub(super) height: u16,
    pub(super) clipboard: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            certificate_pin: None,
            host: String::new(),
            port: 5900,
            password: String::new(),
            // None and VncAuth: the two types that need no TLS backend.
            security: 0x1 | 0x2,
            width: 1024,
            height: 768,
            clipboard: true,
        }
    }
}

impl Config {
    pub(super) fn snapshot(&self) -> Config {
        Config {
            certificate_pin: self.certificate_pin.clone(),
            host: self.host.clone(),
            port: self.port,
            password: self.password.clone(),
            security: self.security,
            width: self.width,
            height: self.height,
            clipboard: self.clipboard,
        }
    }
}

/// Input and clipboard commands from the host thread to the session thread.
pub(super) enum Command {
    Key { keysym: u32, down: bool },
    Pointer { x: u16, y: u16, buttons: u32 },
    Resize(u16, u16),
    Clipboard(Vec<ClipboardEntry>),
}

/// Diagnostic trace, off unless `SHELLUNE_VNC_TRACE` is set. It goes to stderr so a
/// probe can report the exact protocol step an attempt stopped at.
pub(super) fn trace(step: &str) {
    if std::env::var_os("SHELLUNE_VNC_TRACE").is_some() {
        eprintln!("vnc trace: {step}");
    }
}

/// Why one attempt ended: the business failure and the text that explains it.
pub(super) struct Ended {
    pub(super) failure: Failure,
    pub(super) reason: String,
    reconnect: bool,
}

impl Ended {
    pub(super) fn new(failure: Failure, reason: impl Into<String>) -> Ended {
        Ended {
            failure,
            reason: reason.into(),
            reconnect: failure == Failure::RemoteClosed,
        }
    }

    pub(super) fn io(error: io::Error, during: Failure) -> Ended {
        let failure = match error.kind() {
            io::ErrorKind::PermissionDenied => Failure::Authentication,
            io::ErrorKind::Unsupported => Failure::UnsupportedSecurity,
            io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset => Failure::RemoteClosed,
            _ if during != Failure::None => during,
            _ => Failure::Io,
        };
        let mut ended = Ended::new(failure, error.to_string());
        ended.reconnect |= matches!(
            error.kind(),
            io::ErrorKind::ConnectionAborted | io::ErrorKind::BrokenPipe
        );
        // A pending Winsock operation gives us no completed byte count. Start
        // a fresh session rather than repeating a possibly partial write.
        #[cfg(windows)]
        {
            ended.reconnect |= error.raw_os_error()
                == Some(windows_sys::Win32::Networking::WinSock::WSA_IO_PENDING);
        }
        ended
    }
}

impl From<io::Error> for Ended {
    fn from(error: io::Error) -> Ended {
        Ended::io(error, Failure::None)
    }
}

/// The VNC half of `DesktopClient`.
pub(super) struct Client {
    config: Config,
    emit: Emit,
    control: Arc<Control>,
    commands: Option<Sender<Command>>,
    awaiting_reconnect_frame: Option<Arc<std::sync::atomic::AtomicBool>>,
    worker: Option<JoinHandle<()>>,
}

impl Client {
    pub(super) fn new(
        emit: Emit,
        awaiting_reconnect_frame: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Client {
        Client {
            config: Config::default(),
            emit,
            control: Arc::new(Control::default()),
            commands: None,
            awaiting_reconnect_frame,
            worker: None,
        }
    }

    pub(super) fn set_certificate_pin(&mut self, pin: Option<&str>) {
        self.config.certificate_pin = pin.map(str::to_owned);
    }

    pub(super) fn set_server(&mut self, host: &str, port: u16) {
        self.config.host = host.to_owned();
        self.config.port = if port == 0 { 5900 } else { port };
    }

    /// RFB has one secret: the session password. A user name or domain has no
    /// place in it, so only the password is kept.
    pub(super) fn set_password(&mut self, password: Option<&str>) {
        self.config.password = password.unwrap_or_default().to_owned();
    }

    pub(super) fn set_size(&mut self, width: u32, height: u32) {
        // The server decides the desktop size; this is what an attempt asks for
        // when it is still free to.
        self.config.width = u16::try_from(width).unwrap_or(u16::MAX);
        self.config.height = u16::try_from(height).unwrap_or(u16::MAX);
    }

    pub(super) fn set_security(&mut self, mask: u32) {
        self.config.security = mask;
    }

    /// Frames are always 32bpp. Any other depth is refused rather than configured
    /// and then dropped.
    pub(super) fn set_depth(&self, bytes_per_pixel: u32) -> Result<(), Error> {
        if bytes_per_pixel == 4 {
            Ok(())
        } else {
            Err(Error::Argument)
        }
    }

    pub(super) fn set_clipboard(&mut self, enabled: bool) {
        self.config.clipboard = enabled;
    }

    pub(super) fn state(&self) -> State {
        self.control.state()
    }

    pub(super) fn connect(&mut self) -> Result<(), Error> {
        if self.worker.is_some() {
            return Err(Error::State);
        }
        if self.config.host.is_empty() {
            return Err(Error::Argument);
        }
        let (sender, receiver) = input::channel(self.awaiting_reconnect_frame.clone());
        let emit = Arc::clone(&self.emit);
        let control = Arc::clone(&self.control);
        let config = self.config.snapshot();
        publish(&emit, &control, &State::Connecting, Failure::None, "");
        self.commands = Some(sender);
        self.worker = Some(std::thread::spawn(move || {
            session::run(&config, &emit, &control, receiver);
        }));
        Ok(())
    }

    pub(super) fn cancel(&mut self) {
        self.control.stop();
    }

    /// Answers a certificate question the worker asked; the id must be echoed.
    pub(super) fn answer_certificate(&self, request_id: u32, accept: bool) -> Result<(), Error> {
        if self.control.answer(request_id, accept) {
            Ok(())
        } else {
            Err(Error::State)
        }
    }

    /// Waits for the worker to end, at most `millis`; a worker that outlives the
    /// wait is still reaped by a later call.
    pub(super) fn wait(&mut self, millis: u32) -> Result<(), Error> {
        if self.join_worker(Duration::from_millis(u64::from(millis))) {
            Ok(())
        } else {
            Err(Error::Timeout)
        }
    }

    /// Joins the worker if it finishes within `timeout`.
    fn join_worker(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            match self.worker.as_ref() {
                Some(worker) if !worker.is_finished() => {
                    if Instant::now() >= deadline {
                        return false;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Some(_) => {
                    if let Some(worker) = self.worker.take() {
                        let _ = worker.join();
                    }
                    self.commands = None;
                    return true;
                }
                None => return true,
            }
        }
    }

    fn command(&self, command: Command) -> Result<(), Error> {
        let buttons = match &command {
            Command::Pointer { buttons, .. } => Some(*buttons),
            _ => None,
        };
        self.commands
            .as_ref()
            .ok_or(crate::Error::State)?
            .send(command, buttons)
    }

    pub(super) fn key(&self, keysym: u32, down: bool) -> Result<(), Error> {
        if keysym == 0 {
            return Ok(());
        }
        self.command(Command::Key { keysym, down })
    }

    pub(super) fn resize(&self, w: u16, h: u16) -> Result<(), Error> {
        self.command(Command::Resize(w, h))
    }

    pub(super) fn pointer(&self, x: u16, y: u16, buttons: u32) -> Result<(), Error> {
        self.command(Command::Pointer { x, y, buttons })
    }

    /// Offers the text of a group. Text over the bound is cut at the last whole
    /// character and reported once, like every other backend; images are not part of
    /// RFB and are not forwarded.
    pub(super) fn push_clipboard(&self, entries: &[ClipboardEntry]) -> Result<(), Error> {
        if !self.config.clipboard {
            return Err(Error::Unavailable(
                "clipboard sharing is turned off for this connection".to_owned(),
            ));
        }
        let Some(text) = entries
            .iter()
            .find(|entry| entry.kind == ClipboardKind::Text)
        else {
            return Ok(());
        };
        let len = cut_clipboard_text(&text.bytes, MAX_CLIPBOARD_TEXT_BYTES);
        let cut = len < text.bytes.len();
        let entry = ClipboardEntry {
            kind: ClipboardKind::Text,
            bytes: text.bytes[..len].to_vec(),
        };
        self.command(Command::Clipboard(vec![entry]))?;
        if cut {
            (self.emit)(Event::Notice {
                notice: Notice::ClipboardTooLarge(ClipboardKind::Text),
            });
        }
        Ok(())
    }

    /// The settings an attempt would use, for diagnostics without a server.
    pub(super) fn describe(&self) -> String {
        format!(
            "protocol=vnc implementation=shellune clipboard={} security={:#x} size={}x{}",
            u8::from(self.config.clipboard),
            self.config.security,
            self.config.width,
            self.config.height
        )
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.control.stop();
        if !self.join_worker(SHUTDOWN_TIMEOUT) {
            // The worker is parked in something that cannot be interrupted (a name
            // lookup, a connect to an address that never answers). It is left to
            // finish on its own; the sink it feeds stays alive through the emitter.
            crate::count_leaked_handle();
        }
    }
}
