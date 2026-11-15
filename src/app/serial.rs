//! Serial sessions in the shell: what a saved device becomes when a pane opens it
//! (R17, R18, R21).
//!
//! The backend is `shellune_serial`: it lists ports, validates the line settings,
//! opens the device and runs the reader and writer threads. This module is the layer
//! between it and the window, and it holds only the decisions a window makes:
//!
//! * [`SerialTarget`] - a saved [`ConnectionRecord`] turned into the settings a device
//!   is really opened with, so the form, the record and the port never disagree;
//! * [`NewlineFormat`] - what the Return key sends. It changes the bytes the user
//!   sends and nothing else: what the device returns is never rewritten;
//! * [`SerialLink`] - one open device as a terminal pane drives it, including the
//!   local echo, the send queue refusing bytes and the loss a driver reports;
//! * [`PortScan`] and [`DeviceChoice`] - the port list, its two empty situations (the
//!   list could not be read, and the machine has no ports) and the exact-name rule a
//!   saved device is matched by on a reconnect.
//!
//! Two rules run through all of it. A serial session is a character terminal and
//! nothing else - no file view, no batch, no forwarding, no X11 - and the capability
//! set in `shellune_core` is what enforces that, not a hidden button. And nothing here
//! presents a device it did not open: [`SerialTarget`] is a description, and a pane is
//! live only while a [`SerialLink`] holds an open port.

use shellune_core::{ConnectionRecord, SerialFlow, SerialParity, SerialStopBits};
use shellune_serial::{
    CloseOutcome, DataBits, FlowControl, Parity, PortInfo, SerialCause, SerialConfig, SerialError,
    SerialEvent, SerialSession, StopBits, list_ports,
};

/// A saved serial connection, as a device is opened from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialTarget {
    device: String,
    config: SerialConfig,
    local_echo: bool,
}

impl SerialTarget {
    /// Reads a saved record into the settings a port is opened with (R21).
    ///
    /// Returns `None` for a record of another protocol: a serial session cannot be
    /// started from an SSH, RDP or VNC record, and answering with a made-up target
    /// would open a device the user never chose.
    #[must_use]
    pub fn of_record(record: &ConnectionRecord) -> Option<Self> {
        let ConnectionRecord::Serial(saved) = record else {
            return None;
        };
        Some(Self {
            device: saved.device().to_owned(),
            config: config_of(saved),
            local_echo: saved.local_echo(),
        })
    }

    /// The device exactly as the platform spells it, which is what is opened.
    #[must_use]
    pub fn device(&self) -> &str {
        &self.device
    }

    /// The line settings the port is opened with.
    #[must_use]
    pub fn config(&self) -> &SerialConfig {
        &self.config
    }

    /// Whether what the user types is echoed locally as well as sent.
    #[must_use]
    pub fn local_echo(&self) -> bool {
        self.local_echo
    }
}

/// Maps a saved record frame onto the backend own types.
///
/// The two enums are deliberately separate: `shellune-core` stores what a user chose
/// and must keep loading files a future build wrote, while the backend names what a
/// driver is asked for. The mapping is total because the record validates its own
/// ranges - a data width outside 5..=8 cannot be saved - and the fallback below only
/// keeps this function total; no record that was written reaches it.
fn config_of(saved: &shellune_core::SerialConfig) -> SerialConfig {
    let data_bits = match saved.data_bits() {
        5 => DataBits::Five,
        6 => DataBits::Six,
        7 => DataBits::Seven,
        _ => DataBits::Eight,
    };
    let parity = match saved.parity() {
        SerialParity::None => Parity::None,
        SerialParity::Even => Parity::Even,
        SerialParity::Odd => Parity::Odd,
        SerialParity::Mark => Parity::Mark,
        SerialParity::Space => Parity::Space,
    };
    let stop_bits = match saved.stop_bits() {
        SerialStopBits::One => StopBits::One,
        SerialStopBits::Two => StopBits::Two,
    };
    let flow = match saved.flow() {
        SerialFlow::None => FlowControl::None,
        SerialFlow::Software => FlowControl::Software,
        SerialFlow::Hardware => FlowControl::Hardware,
    };
    SerialConfig::new(saved.baud(), data_bits, parity, stop_bits, flow)
}

/// What the Return key sends (R21).
///
/// A serial device is not a terminal emulator: nothing on its side turns a line feed
/// into a carriage return, and different equipment expects different pairs. The choice
/// is therefore a send habit, and it is applied to the bytes the *user* sends - typed
/// text, a paste, a pressed Return - and never to what the device returns. Rewriting
/// received bytes would corrupt a binary protocol whose 0x0d is a header, and would
/// make the transcript disagree with the wire.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NewlineFormat {
    /// Send exactly what was typed: no line break is rewritten.
    #[default]
    AsTyped,
    /// Every line break becomes a carriage return, which is what a terminal sends.
    CarriageReturn,
    /// Every line break becomes a line feed.
    LineFeed,
    /// Every line break becomes a carriage return and a line feed.
    CarriageReturnLineFeed,
}

impl NewlineFormat {
    /// Every choice, in the order the menu offers them.
    pub const ALL: [Self; 4] = [
        Self::AsTyped,
        Self::CarriageReturn,
        Self::LineFeed,
        Self::CarriageReturnLineFeed,
    ];

    /// The short label the control shows.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::AsTyped => "as typed",
            Self::CarriageReturn => "CR",
            Self::LineFeed => "LF",
            Self::CarriageReturnLineFeed => "CRLF",
        }
    }

    /// The bytes this format sends for one line break.
    #[must_use]
    pub fn ending(self) -> &'static [u8] {
        match self {
            Self::AsTyped => b"",
            Self::CarriageReturn => b"\r",
            Self::LineFeed => b"\n",
            Self::CarriageReturnLineFeed => b"\r\n",
        }
    }

    /// What is handed to the device for the bytes the user produced.
    ///
    /// A pair CR LF is one line break and is rewritten once, not twice, so a paste of
    /// a Windows text file does not become two breaks per line on the wire.
    #[must_use]
    pub fn apply(self, bytes: &[u8]) -> Vec<u8> {
        if self == Self::AsTyped {
            return bytes.to_vec();
        }
        let ending = self.ending();
        let mut sent = Vec::with_capacity(bytes.len() + 8);
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            if byte == b'\r' || byte == b'\n' {
                sent.extend_from_slice(ending);
                // The second half of a CR LF pair belongs to the break just written.
                if byte == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                    index += 1;
                }
            } else {
                sent.push(byte);
            }
            index += 1;
        }
        sent
    }

    /// What the local echo shows for one send.
    ///
    /// The echo is what a person reads, not what the device sees: whatever ending was
    /// chosen, a line break has to move the caret to the next row, and the grid does
    /// that for CR LF alone. The bytes on the wire are unchanged - this only decides
    /// how the echoed copy of them is drawn.
    #[must_use]
    pub fn echo(self, bytes: &[u8]) -> Vec<u8> {
        let mut shown = Vec::with_capacity(bytes.len() + 8);
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            if byte == b'\r' || byte == b'\n' {
                shown.extend_from_slice(b"\r\n");
                if byte == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                    index += 1;
                }
            } else {
                shown.push(byte);
            }
            index += 1;
        }
        shown
    }
}
/// One open device, as a terminal pane drives it (R17, R21).
///
/// The reader and writer threads belong to the session inside; the pane sees only
/// this. Three things are kept beside it, and each is a fact a person needs:
///
/// * a send the queue refused - a bound is not a lost connection, so it is stated and
///   the session stays live;
/// * a frame the driver took in pieces - its remainder is not replayed, which the
///   backend reports and this passes on rather than hiding;
/// * a receive loss the driver reported - bytes that were on the wire and never
///   arrived, which no call can fail for.
pub struct SerialLink {
    session: SerialSession,
    /// Why the last send could not be queued, kept until a send succeeds.
    send_problem: Option<String>,
    /// The frame the driver did not finish, kept until the pane shows it.
    incomplete: Option<String>,
    /// The receive loss the driver reported, kept until the pane shows it.
    loss: Option<String>,
}

impl SerialLink {
    /// Opens the device a target names and starts its session.
    ///
    /// # Errors
    ///
    /// The reason the backend allowed, already phrased for a person: which port,
    /// which cause the platform could separate, and what to do about it.
    pub fn open(target: &SerialTarget) -> Result<Self, String> {
        SerialSession::open(target.device(), target.config())
            .map(Self::holding)
            .map_err(|error| describe(&error))
    }

    /// Wraps an already open session.
    ///
    /// This is how the tests drive every rule a pane has with a real session behind
    /// it - reader, writer, bounded queue, close - over an in-memory port. Nothing in
    /// the application opens a port this way: a real device always goes through
    /// [`SerialLink::open`], which is the only call that reaches the system API.
    pub fn holding(session: SerialSession) -> Self {
        Self {
            session,
            send_problem: None,
            incomplete: None,
            loss: None,
        }
    }

    /// Queues bytes for the device.
    ///
    /// Returns `Ok` when the bytes were queued. A full queue is **not** an error of the
    /// session: the bytes were refused, the session is still open, and the pane says so
    /// while keeping the terminal live. Any other cause really did end the session and
    /// is reported as one.
    ///
    /// # Errors
    ///
    /// The reason the session is no longer open, when it is not.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        match self.session.send(bytes) {
            Ok(_) => {
                self.send_problem = None;
                Ok(())
            }
            // A refusal is a pause, not an end: the queue is full and the same bytes
            // can be typed again. Ending the session here would kill a device that is
            // merely slower than the typist.
            Err(error) if error.is(SerialCause::QueueFull) => {
                self.send_problem = Some(describe(&error));
                Ok(())
            }
            Err(error) => Err(describe(&error)),
        }
    }

    /// Everything the session reported since the last call, in order.
    ///
    /// The events are passed up unchanged except for the two the pane states itself:
    /// a driver receive loss and an unfinished frame are kept here for the bar,
    /// because they describe this line rather than a change of state.
    pub fn poll(&mut self) -> Vec<SerialEvent> {
        let events = self.session.poll();
        for event in &events {
            match event {
                SerialEvent::SendIncomplete {
                    sent, requested, ..
                } => {
                    self.incomplete = Some(format!(
                        "the device accepted {sent} of {requested} bytes; the rest was not sent again"
                    ));
                }
                SerialEvent::ReceiveOverflow { notice } => {
                    self.loss = Some(match notice.lost {
                        Some(lost) => format!("the device lost {lost} bytes: {}", notice.text),
                        None => format!("the device lost bytes: {}", notice.text),
                    });
                }
                SerialEvent::Received(_)
                | SerialEvent::Sent { .. }
                | SerialEvent::Disconnected { .. }
                | SerialEvent::Closed => {}
            }
        }
        events
    }

    /// Takes the reason the last send could not be queued, if there is one.
    pub fn take_send_problem(&mut self) -> Option<String> {
        self.send_problem.take()
    }

    /// Takes the unfinished frame the driver reported, if it reported one.
    pub fn take_incomplete(&mut self) -> Option<String> {
        self.incomplete.take()
    }

    /// Takes the receive loss the driver reported, if it reported one.
    pub fn take_loss(&mut self) -> Option<String> {
        self.loss.take()
    }

    /// Stops the session and releases the port, reporting what could not be sent.
    ///
    /// The order is the backend one: in-flight I/O is cancelled, both threads are
    /// joined, and only then is the handle dropped. A close the driver never answers
    /// keeps the port in Closing and says so instead of reporting a release that did
    /// not happen, and the pane states that as the reason it is not finished.
    pub fn close(&mut self) -> CloseReport {
        match self.session.close() {
            CloseOutcome::Closed {
                discarded_frames,
                discarded_bytes,
            } => CloseReport {
                released: true,
                note: (discarded_frames > 0).then(|| {
                    format!(
                        "{discarded_frames} queued frames ({discarded_bytes} bytes) were never sent"
                    )
                }),
            },
            CloseOutcome::AlreadyClosed => CloseReport {
                released: true,
                note: None,
            },
            CloseOutcome::StillClosing { waited } => CloseReport {
                released: false,
                note: Some(format!(
                    "the driver did not stop within {} ms, so the port is still held",
                    waited.as_millis()
                )),
            },
        }
    }
}

/// What closing a serial session achieved (R17).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CloseReport {
    /// Whether the port was really released.
    pub released: bool,
    /// What a person has to know about what remained, when anything did.
    pub note: Option<String>,
}
/// The reason a device could not be opened or kept open, phrased for a person.
///
/// The cause the platform allowed comes first, the causes it could not separate
/// follow, then the system own detail, and the next step that cause points at closes
/// the sentence - because "port is in use" without "close the other program" leaves
/// the reader where they were. Every cause the backend names has a next step, so this
/// never returns a diagnostic without one.
#[must_use]
pub fn describe(error: &SerialError) -> String {
    let mut text = String::new();
    if let Some(port) = error.port() {
        text.push_str(port);
        text.push_str(": ");
    }
    text.push_str(error.cause().label());
    let also: Vec<&'static str> = error
        .causes()
        .iter()
        .filter(|cause| **cause != error.cause())
        .map(|cause| cause.label())
        .collect();
    if !also.is_empty() {
        text.push_str(" (or: ");
        text.push_str(&also.join(", "));
        text.push(')');
    }
    if let Some(detail) = error.detail() {
        text.push_str(" - ");
        text.push_str(detail);
    }
    text.push_str(". ");
    text.push_str(error.cause().next_step());
    if let Some(raw) = error.raw() {
        text.push_str(&format!(" [{} {}]", raw.kind().label(), raw.code()));
    }
    text
}

/// What a port list showed the last time it was asked (R21).
///
/// R21 asks that a list which could not be read and a machine with no ports be told
/// apart: they need different sentences and different next steps, and drawing both as
/// "no ports" would send a user to plug in a cable when the device service is down.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PortScan {
    /// The list has not been asked for yet.
    #[default]
    NotAsked,
    /// The system answered. An empty list is a real answer and is not an error.
    Ready(Vec<PortInfo>),
    /// The list could not be read, with the reason and its next step.
    Failed(String),
}

impl PortScan {
    /// Asks the system which ports exist right now.
    #[must_use]
    pub fn scan() -> Self {
        match list_ports() {
            Ok(ports) => Self::Ready(ports),
            Err(error) => Self::Failed(describe(&error)),
        }
    }

    /// The ports the system reported, newest answer only.
    #[must_use]
    pub fn ports(&self) -> &[PortInfo] {
        match self {
            Self::Ready(ports) => ports,
            Self::NotAsked | Self::Failed(_) => &[],
        }
    }

    /// Whether the system answered with no ports at all.
    ///
    /// Kept apart from "not asked" and from "could not be asked": only an answer that
    /// really listed nothing may say this machine has no ports.
    #[must_use]
    pub fn is_empty_machine(&self) -> bool {
        matches!(self, Self::Ready(ports) if ports.is_empty())
    }

    /// Whether a saved device is in the list, by the exact-name rule below.
    #[must_use]
    pub fn holds(&self, device: &str) -> bool {
        self.ports().iter().any(|port| same_device(port, device))
    }

    /// The ports as the labels a chooser shows.
    #[must_use]
    pub fn labels(&self) -> Vec<String> {
        self.ports()
            .iter()
            .map(|port| match port.description() {
                Some(description) => format!("{} - {description}", port.name()),
                None => port.name().to_owned(),
            })
            .collect()
    }
}

/// Whether a listed port is the device a saved record names.
///
/// The comparison is exact: the same name, or the same path, ignoring only the case
/// Windows writes a device map key in. It is deliberately not a prefix or similarity
/// match - a saved `COM3` is not satisfied by a `COM30` that happens to be plugged in,
/// because opening a different device than the one the user saved would be a session
/// that looks right and talks to the wrong thing. A reconnect to a device that is gone
/// therefore fails with the missing-port reason instead of silently moving.
#[must_use]
pub fn same_device(port: &PortInfo, device: &str) -> bool {
    let matches = |candidate: &str| candidate == device || candidate.eq_ignore_ascii_case(device);
    matches(port.name()) || matches(port.path())
}

/// What a saved device did when the list was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceChoice {
    /// The saved device is attached exactly as it was saved.
    Attached,
    /// It is not attached. Nothing is substituted for it.
    Missing,
}

impl DeviceChoice {
    /// Decides from a scan and the device a record names.
    #[must_use]
    pub fn of(scan: &PortScan, device: &str) -> Self {
        if scan.holds(device) {
            Self::Attached
        } else {
            Self::Missing
        }
    }
}
/// What one serial pane remembers and draws (R21).
///
/// The session itself is in [`SerialLink`] and exists only while the port is open.
/// This outlives it: the device the pane is about, the settings it was opened with,
/// the two send habits, the port list and the one line under the grid. That split is
/// what lets a pane whose port could not be opened still say which device, why, what
/// else is attached and what to do next - the case R21 asks not to draw as a terminal.
pub struct SerialPane {
    device: String,
    settings: String,
    format: NewlineFormat,
    echo: bool,
    ports: PortScan,
    note: Option<String>,
}

impl SerialPane {
    /// The state of a pane for a saved device.
    ///
    /// The local echo starts from the record, because that is the choice the user made
    /// when they saved the connection; the newline format starts as typed, which is
    /// what the bytes on the wire really were before a choice is made.
    #[must_use]
    pub fn new(device: &str, settings: impl Into<String>, local_echo: bool) -> Self {
        Self {
            device: device.to_owned(),
            settings: settings.into(),
            format: NewlineFormat::AsTyped,
            echo: local_echo,
            ports: PortScan::NotAsked,
            note: None,
        }
    }

    /// The device the pane is about.
    #[must_use]
    pub fn device(&self) -> &str {
        &self.device
    }

    /// The settings the port was opened with.
    #[must_use]
    pub fn settings(&self) -> &str {
        &self.settings
    }

    /// What the Return key sends.
    #[must_use]
    pub fn format(&self) -> NewlineFormat {
        self.format
    }

    /// Chooses what the Return key sends. It applies to the next send.
    pub fn set_format(&mut self, format: NewlineFormat) {
        self.format = format;
    }

    /// Whether typed text is echoed locally.
    #[must_use]
    pub fn echo(&self) -> bool {
        self.echo
    }

    /// Turns the local echo on or off.
    pub fn set_echo(&mut self, echo: bool) {
        self.echo = echo;
    }

    /// The port list as it was last answered.
    #[must_use]
    pub fn ports(&self) -> &PortScan {
        &self.ports
    }

    /// Asks the system again, which is what the refresh control does.
    ///
    /// It replaces the answer whether the list was read or not, so a service that came
    /// back is reflected without reopening the pane.
    pub fn refresh_ports(&mut self) {
        self.ports = PortScan::scan();
    }

    /// Puts a port list the pane was handed, without asking the system.
    ///
    /// The tests drive the two empty situations the system cannot be made to produce on
    /// demand with it, and so does the capture probe; the control above is what the
    /// application uses.
    #[cfg(any(test, feature = "ui-capture"))]
    pub fn show_ports(&mut self, ports: PortScan) {
        self.ports = ports;
    }

    /// The one line the pane says about its line, when it has one.
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Replaces the line the pane says, or clears it.
    pub fn set_note(&mut self, note: Option<String>) {
        self.note = note;
    }

    /// Keeps the newest of two things worth saying about the line.
    ///
    /// Only one line is drawn, so the newest fact replaces the older one: a refused
    /// send that was followed by a lost byte should not hide the loss behind it.
    pub fn say(&mut self, note: impl Into<String>) {
        self.note = Some(note.into());
    }
}
