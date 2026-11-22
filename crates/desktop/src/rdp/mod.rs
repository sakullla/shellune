//! Shellune's own RDP client: negotiation, security, session and channels.
//!
//! This module is the whole RDP backend. It shares the crate's `DesktopClient` API,
//! event stream, cancellation and clipboard rules with the VNC engine, but it never
//! calls FreeRDP and owns its socket and protocol state.
mod clipboard;
mod display;
mod drive;
mod mcs;
mod pointer;
mod security;
mod session;
mod share;
mod update;
mod wire;

pub(crate) use clipboard::{
    cf_unicode_text, probe_offer, probe_offer_events, probe_receive, text_from_cf_unicode,
};
pub(crate) use drive::{ProbeRequest, probe_drive_reply, probe_replies, probe_requests};

use crate::input::{self, Sender};
use crate::{
    ClipboardEntry, Failure, SD_BUTTON_LEFT, SD_BUTTON_MIDDLE, SD_BUTTON_RIGHT,
    SD_BUTTON_WHEEL_DOWN, SD_BUTTON_WHEEL_LEFT, SD_BUTTON_WHEEL_RIGHT, SD_BUTTON_WHEEL_UP, State,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub(super) use crate::control::{Control, Emit, publish};

/// What the host asked the session to configure before it connected.
pub(super) struct Config {
    pub(super) certificate_pin: Option<String>,
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) user: String,
    pub(super) password: String,
    pub(super) domain: String,
    pub(super) scale_percent: u32,
    pub(super) width: u16,
    pub(super) height: u16,
    pub(super) clipboard: bool,
    pub(super) drive: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            certificate_pin: None,
            host: String::new(),
            port: 3389,
            user: String::new(),
            password: String::new(),
            domain: String::new(),
            scale_percent: 100,
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
    Resize(u16, u16, u32),
    Clipboard(Vec<ClipboardEntry>),
}

/// Diagnostic trace, off unless `SHELLUNE_RDP_TRACE` is set. It goes to stderr so
/// a probe can report the exact protocol step an attempt stopped at.
pub(super) fn trace(step: &str) {
    if trace_enabled() {
        thread_local! {static START: std::time::Instant=std::time::Instant::now();}
        START.with(|start| eprintln!("rdp trace: +{} ms {step}", start.elapsed().as_millis()));
    }
}

pub(super) fn trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("SHELLUNE_RDP_TRACE").is_some())
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

    pub(super) fn set_certificate_pin(&mut self, pin: Option<&str>) {
        self.config.certificate_pin = pin.map(str::to_owned);
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
        let (sender, receiver) = input::channel(None);
        let emit = Arc::clone(&self.emit);
        let control = Arc::clone(&self.control);
        let config = Config {
            certificate_pin: self.config.certificate_pin.clone(),
            host: self.config.host.clone(),
            port: self.config.port,
            user: self.config.user.clone(),
            password: self.config.password.clone(),
            domain: self.config.domain.clone(),
            scale_percent: self.config.scale_percent,
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
        let buttons = match &command {
            Command::Pointer { buttons, .. } => Some(*buttons),
            _ => None,
        };
        self.commands
            .as_ref()
            .ok_or(crate::Error::State)?
            .send(command, buttons)
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

    pub(super) fn set_scale(&mut self, percent: u32) {
        self.config.scale_percent = percent;
    }

    pub(super) fn resize(&self, width: u16, height: u16, percent: u32) -> Result<(), crate::Error> {
        self.command(Command::Resize(width, height, percent))
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

/// Maps a change of the crate's button state to the protocol's pointer flags.
///
/// The protocol names the button on both edges: a press carries PTR_FLAGS_DOWN
/// with the button bit, a release carries the button bit alone. A wheel step is
/// one event with no edge, so it is emitted once, when its bit appears.
pub(super) fn pointer_flags(previous: u32, buttons: u32) -> u16 {
    const DOWN: u16 = 0x8000;
    const MOVE: u16 = 0x0800;
    if buttons & SD_BUTTON_WHEEL_UP != 0 && previous & SD_BUTTON_WHEEL_UP == 0 {
        return 0x0200 | 0x0078;
    }
    if buttons & SD_BUTTON_WHEEL_DOWN != 0 && previous & SD_BUTTON_WHEEL_DOWN == 0 {
        return 0x0200 | 0x0100 | 0x0088;
    }
    if buttons & SD_BUTTON_WHEEL_LEFT != 0 && previous & SD_BUTTON_WHEEL_LEFT == 0 {
        return 0x0400 | 0x0188;
    }
    if buttons & SD_BUTTON_WHEEL_RIGHT != 0 && previous & SD_BUTTON_WHEEL_RIGHT == 0 {
        return 0x0400 | 0x0078;
    }
    let mut flags = 0;
    for (mask, flag) in [
        (SD_BUTTON_LEFT, 0x1000u16),
        (SD_BUTTON_RIGHT, 0x2000),
        (SD_BUTTON_MIDDLE, 0x4000),
    ] {
        if buttons & mask != previous & mask {
            flags |= flag;
            if buttons & mask != 0 {
                flags |= DOWN;
            }
        }
    }
    if flags == 0 { MOVE } else { flags }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn a_press_and_a_release_both_name_their_button() {
        // A button edge and a movement are distinct RDP mouse events.
        assert_eq!(pointer_flags(0, SD_BUTTON_LEFT), 0x1000 | 0x8000);
        // Release: the same button bit without the down bit, so a server knows
        // which button came up.
        assert_eq!(pointer_flags(SD_BUTTON_LEFT, 0), 0x1000);
        assert_eq!(pointer_flags(SD_BUTTON_RIGHT, 0), 0x2000);
        assert_eq!(pointer_flags(SD_BUTTON_MIDDLE, 0), 0x4000);
        // A plain move carries no button at all.
        assert_eq!(pointer_flags(0, 0), 0x0800);
        // Holding a button while moving does not repeat the press.
        assert_eq!(pointer_flags(SD_BUTTON_LEFT, SD_BUTTON_LEFT), 0x0800);
    }

    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn a_wheel_step_is_one_event_and_its_release_is_not_one() {
        assert_eq!(pointer_flags(0, SD_BUTTON_WHEEL_UP), 0x0200 | 0x0078);
        assert_eq!(
            pointer_flags(0, SD_BUTTON_WHEEL_DOWN),
            0x0200 | 0x0100 | 0x0088
        );
        assert_eq!(pointer_flags(0, SD_BUTTON_WHEEL_LEFT), 0x0400 | 0x0188);
        assert_eq!(pointer_flags(0, SD_BUTTON_WHEEL_RIGHT), 0x0400 | 0x0078);
        // Clearing the step bit does not generate a second wheel event.
        assert_eq!(pointer_flags(SD_BUTTON_WHEEL_UP, 0), 0x0800);
    }
}
