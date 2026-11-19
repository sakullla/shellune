//! Shellune's own RDP client: negotiation, security, session and channels.
//!
//! This module is the whole RDP backend. It shares the crate's `DesktopClient` API,
//! event stream, cancellation and clipboard rules with the VNC shim, but it never
//! calls FreeRDP and owns its socket and protocol state.
mod clipboard;
mod drive;
mod mcs;
mod security;
mod session;
mod share;
mod update;
mod wire;

pub(crate) use clipboard::{probe_offer, probe_offer_events, probe_receive};
pub(crate) use drive::{ProbeRequest, probe_drive_reply, probe_requests};

use crate::{ClipboardEntry, Event, Failure, State, ffi};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Where an event goes. The crate's mailbox or handler is wrapped once here so
/// the session never has to know which one it is.
pub(super) type Emit = Arc<dyn Fn(Event) + Send + Sync>;

/// What the host asked the session to configure before it connected.
pub(super) struct Config {
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) user: String,
    pub(super) password: String,
    pub(super) domain: String,
    pub(super) width: u16,
    pub(super) height: u16,
    pub(super) clipboard: bool,
    pub(super) drive: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            host: String::new(),
            port: 3389,
            user: String::new(),
            password: String::new(),
            domain: String::new(),
            width: 1024,
            height: 768,
            clipboard: true,
            drive: None,
        }
    }
}

/// Input and clipboard commands from the host thread to the session thread.
pub(super) enum Command {
    Key {
        scancode: u8,
        down: bool,
        extended: bool,
    },
    Pointer {
        buttons: u32,
        x: u16,
        y: u16,
    },
    Clipboard(Vec<ClipboardEntry>),
}

/// Shared between the handle and its worker: cancellation, the certificate
/// question and the state the host reads back synchronously.
pub(super) struct Control {
    cancel: AtomicBool,
    socket: Mutex<Option<TcpStream>>,
    state: AtomicU8,
    failure: Mutex<Failure>,
    certificate: Mutex<Question>,
    next_question: AtomicU32,
}

struct Question {
    id: u32,
    answer: Option<bool>,
}

impl Default for Control {
    fn default() -> Control {
        Control {
            cancel: AtomicBool::new(false),
            socket: Mutex::new(None),
            state: AtomicU8::new(0),
            failure: Mutex::new(Failure::None),
            certificate: Mutex::new(Question {
                id: 0,
                answer: None,
            }),
            next_question: AtomicU32::new(0),
        }
    }
}

impl Control {
    pub(super) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Keeps a second handle to the socket so `cancel` can unblock a read.
    pub(super) fn attach(&self, socket: &TcpStream) {
        if let Ok(mut slot) = self.socket.lock() {
            *slot = socket.try_clone().ok();
        }
    }

    pub(super) fn stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Ok(slot) = self.socket.lock() {
            if let Some(socket) = slot.as_ref() {
                let _ = socket.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    /// Asks the host whether to trust a certificate and waits for its answer. The
    /// wait is bounded: no answer is a refusal, never a silent acceptance.
    pub(super) fn ask(
        &self,
        emit: &Emit,
        host: &str,
        subject: String,
        issuer: String,
        fingerprint: String,
    ) -> bool {
        let id = self.next_question.fetch_add(1, Ordering::SeqCst) + 1;
        match self.certificate.lock() {
            Ok(mut question) => *question = Question { id, answer: None },
            Err(_) => return false,
        }
        emit(Event::Certificate {
            request_id: id,
            host: host.to_owned(),
            subject,
            issuer,
            fingerprint,
            flags: 0,
        });
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if self.cancelled() || Instant::now() > deadline {
                return false;
            }
            match self.certificate.lock() {
                Ok(question) if question.id == id => {
                    if let Some(answer) = question.answer {
                        return answer;
                    }
                }
                Ok(_) => return false,
                Err(_) => return false,
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn answer(&self, request_id: u32, accept: bool) -> bool {
        match self.certificate.lock() {
            Ok(mut question) if question.id == request_id && question.answer.is_none() => {
                question.answer = Some(accept);
                true
            }
            _ => false,
        }
    }

    pub(super) fn state(&self) -> State {
        let failure = self
            .failure
            .lock()
            .map(|failure| *failure)
            .unwrap_or(Failure::Internal);
        decode(self.state.load(Ordering::SeqCst), failure)
    }

    fn record(&self, state: &State, failure: Failure) {
        if let State::Failed(_) = state {
            if let Ok(mut slot) = self.failure.lock() {
                *slot = failure;
            }
        }
        self.state.store(encode(state), Ordering::SeqCst);
    }
}

fn encode(state: &State) -> u8 {
    match state {
        State::Idle => 0,
        State::Connecting => 1,
        State::Connected => 2,
        State::Closing => 3,
        State::Closed => 4,
        State::Failed(_) => 5,
    }
}

fn decode(code: u8, failure: Failure) -> State {
    match code {
        0 => State::Idle,
        1 => State::Connecting,
        2 => State::Connected,
        3 => State::Closing,
        4 => State::Closed,
        5 => State::Failed(failure),
        _ => State::Failed(Failure::Internal),
    }
}

/// Diagnostic trace, off unless `SHELLUNE_RDP_TRACE` is set. It goes to stderr so
/// a probe can report the exact protocol step an attempt stopped at.
pub(super) fn trace(step: &str) {
    if std::env::var_os("SHELLUNE_RDP_TRACE").is_some() {
        eprintln!("rdp trace: {step}");
    }
}

/// Publishes one state change: the host's synchronous reader and the event stream
/// always see the same value.
pub(super) fn publish(
    emit: &Emit,
    control: &Control,
    state: &State,
    failure: Failure,
    reason: &str,
) {
    control.record(state, failure);
    emit(Event::State {
        state: state.clone(),
        failure,
        detail: 0,
        reason: reason.to_owned(),
    });
}

/// The RDP half of `DesktopClient`.
pub(super) struct Client {
    config: Config,
    emit: Emit,
    control: Arc<Control>,
    commands: Option<Sender<Command>>,
    worker: Option<JoinHandle<()>>,
}

impl Client {
    pub(super) fn new(emit: Emit) -> Client {
        Client {
            config: Config::default(),
            emit,
            control: Arc::new(Control::default()),
            commands: None,
            worker: None,
        }
    }

    pub(super) fn set_server(&mut self, host: &str, port: u16) {
        self.config.host = host.to_owned();
        self.config.port = port;
    }

    pub(super) fn set_credentials(
        &mut self,
        user: Option<&str>,
        password: Option<&str>,
        domain: Option<&str>,
    ) {
        self.config.user = user.unwrap_or_default().to_owned();
        self.config.password = password.unwrap_or_default().to_owned();
        self.config.domain = domain.unwrap_or_default().to_owned();
    }

    pub(super) fn set_size(&mut self, width: u32, height: u32) {
        self.config.width = u16::try_from(width).unwrap_or(u16::MAX);
        self.config.height = u16::try_from(height).unwrap_or(u16::MAX);
    }

    pub(super) fn set_clipboard(&mut self, enabled: bool) {
        self.config.clipboard = enabled;
    }

    pub(super) fn set_drive(&mut self, folder: Option<PathBuf>) {
        self.config.drive = folder;
    }

    pub(super) fn state(&self) -> State {
        self.control.state()
    }

    pub(super) fn connect(&mut self) -> Result<(), crate::Error> {
        if self.worker.is_some() {
            return Err(crate::Error::State);
        }
        if self.config.host.is_empty() {
            return Err(crate::Error::Argument);
        }
        let (sender, receiver) = mpsc::channel();
        let emit = Arc::clone(&self.emit);
        let control = Arc::clone(&self.control);
        let config = Config {
            host: self.config.host.clone(),
            port: self.config.port,
            user: self.config.user.clone(),
            password: self.config.password.clone(),
            domain: self.config.domain.clone(),
            width: self.config.width,
            height: self.config.height,
            clipboard: self.config.clipboard,
            drive: self.config.drive.clone(),
        };
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

    /// Waits for the worker to end, at most `millis`; the worker is never detached,
    /// so a later call still reaps it.
    pub(super) fn wait(&mut self, millis: u32) -> Result<(), crate::Error> {
        let deadline = Instant::now() + Duration::from_millis(u64::from(millis));
        loop {
            match self.worker.as_ref() {
                Some(worker) if !worker.is_finished() => {
                    if Instant::now() >= deadline {
                        return Err(crate::Error::Timeout);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Some(_) => {
                    if let Some(worker) = self.worker.take() {
                        let _ = worker.join();
                    }
                    self.commands = None;
                    return Ok(());
                }
                None => return Ok(()),
            }
        }
    }

    pub(super) fn answer_certificate(
        &self,
        request_id: u32,
        accept: bool,
    ) -> Result<(), crate::Error> {
        if self.control.answer(request_id, accept) {
            Ok(())
        } else {
            Err(crate::Error::State)
        }
    }

    fn command(&self, command: Command) -> Result<(), crate::Error> {
        match &self.commands {
            Some(sender) => sender.send(command).map_err(|_| crate::Error::State),
            None => Err(crate::Error::State),
        }
    }

    pub(super) fn key(
        &self,
        scancode: u32,
        down: bool,
        extended: bool,
    ) -> Result<(), crate::Error> {
        self.command(Command::Key {
            scancode: u8::try_from(scancode).map_err(|_| crate::Error::Argument)?,
            down,
            extended,
        })
    }

    pub(super) fn pointer(&self, x: u16, y: u16, buttons: u32) -> Result<(), crate::Error> {
        self.command(Command::Pointer { buttons, x, y })
    }

    /// The settings an attempt would use, for diagnostics: it proves the options
    /// reached this engine without a server.
    pub(super) fn describe(&self) -> String {
        format!(
            "protocol=rdp implementation=shellune clipboard={} drive={} size={}x{}",
            u8::from(self.config.clipboard),
            if self.config.drive.is_some() { 1 } else { 0 },
            self.config.width,
            self.config.height
        )
    }

    pub(super) fn push_clipboard(&self, entries: &[ClipboardEntry]) -> Result<(), crate::Error> {
        if !self.config.clipboard {
            return Err(crate::Error::Unavailable(
                "clipboard sharing is off".to_owned(),
            ));
        }
        self.command(Command::Clipboard(entries.to_vec()))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.control.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_press_and_a_release_both_name_their_button() {
        // Press: move with the button and the down bit.
        assert_eq!(
            pointer_flags(0, ffi::SD_BUTTON_LEFT),
            0x0800 | 0x1000 | 0x8000
        );
        // Release: the same button bit without the down bit, so a server knows
        // which button came up.
        assert_eq!(pointer_flags(ffi::SD_BUTTON_LEFT, 0), 0x0800 | 0x1000);
        assert_eq!(pointer_flags(ffi::SD_BUTTON_RIGHT, 0), 0x0800 | 0x2000);
        assert_eq!(pointer_flags(ffi::SD_BUTTON_MIDDLE, 0), 0x0800 | 0x4000);
        // A plain move carries no button at all.
        assert_eq!(pointer_flags(0, 0), 0x0800);
        // Holding a button while moving does not repeat the press.
        assert_eq!(
            pointer_flags(ffi::SD_BUTTON_LEFT, ffi::SD_BUTTON_LEFT),
            0x0800
        );
    }

    #[test]
    fn a_wheel_step_is_one_event_and_its_release_is_not_one() {
        assert_eq!(pointer_flags(0, ffi::SD_BUTTON_WHEEL_UP), 0x0200 | 0x0078);
        assert_eq!(
            pointer_flags(0, ffi::SD_BUTTON_WHEEL_DOWN),
            0x0200 | 0x0100 | 0x0088
        );
        // The step bit clears again; that clearing is not a second wheel event.
        assert_eq!(pointer_flags(ffi::SD_BUTTON_WHEEL_UP, 0), 0x0800);
    }
}

/// Maps a change of the crate's button state to the protocol's pointer flags.
///
/// The protocol names the button on both edges: a press carries PTR_FLAGS_DOWN
/// with the button bit, a release carries the button bit alone. A wheel step is
/// one event with no edge, so it is emitted once, when its bit appears.
pub(super) fn pointer_flags(previous: u32, buttons: u32) -> u16 {
    const DOWN: u16 = 0x8000;
    const MOVE: u16 = 0x0800;
    if buttons & ffi::SD_BUTTON_WHEEL_UP != 0 && previous & ffi::SD_BUTTON_WHEEL_UP == 0 {
        return 0x0200 | 0x0078;
    }
    if buttons & ffi::SD_BUTTON_WHEEL_DOWN != 0 && previous & ffi::SD_BUTTON_WHEEL_DOWN == 0 {
        return 0x0200 | 0x0100 | 0x0088;
    }
    if buttons & ffi::SD_BUTTON_WHEEL_LEFT != 0 && previous & ffi::SD_BUTTON_WHEEL_LEFT == 0 {
        return 0x0400 | 0x0078;
    }
    if buttons & ffi::SD_BUTTON_WHEEL_RIGHT != 0 && previous & ffi::SD_BUTTON_WHEEL_RIGHT == 0 {
        return 0x0400 | 0x0088;
    }
    let mut flags = MOVE;
    for (mask, flag) in [
        (ffi::SD_BUTTON_LEFT, 0x1000u16),
        (ffi::SD_BUTTON_RIGHT, 0x2000),
        (ffi::SD_BUTTON_MIDDLE, 0x4000),
    ] {
        if buttons & mask != previous & mask {
            flags |= flag;
            if buttons & mask != 0 {
                flags |= DOWN;
            }
        }
    }
    flags
}
