//! One VNC attempt: the handshake, the request that produces the first frame, the
//! update loop, input and both clipboard forms.
//!
//! The socket is read with a short timeout so a cancel is noticed without a second
//! reader, and bytes of a message that has not fully arrived stay buffered.
use super::clipboard::{self, EXTENDED_CLIPBOARD, Extended};
use super::handshake::{self, Opened, Transport};
use super::wire::{Incoming, Message, Rect};
use super::{Command, Config, Ended};
use crate::control::{Control, Emit, publish};
use crate::{ClipboardKind, Event, Failure, MAX_FRAME_BYTES, Notice, State};
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

/// One read wait once the desktop is up: the loop returns to its cancellation
/// check after each, and a partial message stays buffered.
const POLL: Duration = Duration::from_millis(200);
/// One read. A message is re-parsed from its start until it is complete, so a
/// large buffer keeps a screenful of hextile to a couple of passes instead of a
/// dozen; the read itself still returns as soon as any bytes are available.
const CHUNK: usize = 256 * 1024;

/// Runs one attempt, retrying once if the server drops an established session.
pub(super) fn run(
    config: &Config,
    emit: &Emit,
    control: &Arc<Control>,
    commands: Receiver<Command>,
) {
    for attempt in 1..=super::ATTEMPTS {
        if control.cancelled() {
            // A cancel that arrives before the worker starts still ends the attempt,
            // and the host must see that it did.
            publish(emit, control, &State::Closed, Failure::None, "cancelled");
            return;
        }
        match handshake::open(config, emit, control) {
            Ok(opened) => {
                let mut session = Session::new(opened, config, emit);
                match session.run(control, &commands) {
                    Ok(()) => {
                        publish(emit, control, &State::Closed, Failure::None, "");
                        return;
                    }
                    Err(ended) => {
                        if control.cancelled() {
                            publish(emit, control, &State::Closed, Failure::None, "");
                            return;
                        }
                        if attempt < super::ATTEMPTS && ended.failure == Failure::RemoteClosed {
                            publish(
                                emit,
                                control,
                                &State::Connecting,
                                Failure::None,
                                "the session dropped; reconnecting",
                            );
                            continue;
                        }
                        publish(
                            emit,
                            control,
                            &State::Failed(ended.failure),
                            ended.failure,
                            &ended.reason,
                        );
                        return;
                    }
                }
            }
            Err(ended) => {
                super::trace(&format!(
                    "attempt failed: {:?} {}",
                    ended.failure, ended.reason
                ));
                if control.cancelled() {
                    publish(emit, control, &State::Closed, Failure::None, "");
                    return;
                }
                publish(
                    emit,
                    control,
                    &State::Failed(ended.failure),
                    ended.failure,
                    &ended.reason,
                );
                return;
            }
        }
    }
}

/// The pixels the server has described, kept so a CopyRect can copy from them.
struct Desktop {
    width: u16,
    height: u16,
    pixels: Vec<u8>,
}

impl Desktop {
    fn new(width: u16, height: u16) -> Desktop {
        Desktop {
            width,
            height,
            pixels: vec![0; usize::from(width) * usize::from(height) * 4],
        }
    }

    fn resize(&mut self, width: u16, height: u16) -> Result<(), String> {
        let bytes = usize::from(width) * usize::from(height) * 4;
        if width == 0 || height == 0 || bytes > MAX_FRAME_BYTES {
            return Err(format!(
                "the server resized the desktop to {width}x{height}, past the {MAX_FRAME_BYTES} byte bound"
            ));
        }
        self.width = width;
        self.height = height;
        self.pixels = vec![0; bytes];
        Ok(())
    }

    fn stride(&self) -> usize {
        usize::from(self.width) * 4
    }

    /// Applies one rectangle and returns the frame event it produced, if any.
    fn apply(&mut self, rect: Rect) -> Result<Option<Event>, String> {
        match rect {
            Rect::Resize { width, height } => {
                self.resize(width, height)?;
                Ok(None)
            }
            Rect::Raw {
                x,
                y,
                width,
                height,
                pixels,
            } => {
                let (x, y, width, height) = self.place(x, y, width, height)?;
                let stride = self.stride();
                let row = usize::from(width) * 4;
                for line in 0..usize::from(height) {
                    let at = (usize::from(y) + line) * stride + usize::from(x) * 4;
                    let from = line * row;
                    self.pixels[at..at + row].copy_from_slice(&pixels[from..from + row]);
                }
                Ok(Some(self.region(x, y, width, height)))
            }
            Rect::Copy {
                x,
                y,
                width,
                height,
                source_x,
                source_y,
            } => {
                let (x, y, width, height) = self.place(x, y, width, height)?;
                let (source_x, source_y, _, _) = self.place(source_x, source_y, width, height)?;
                let stride = self.stride();
                let row = usize::from(width) * 4;
                // The source is read whole first: the regions may overlap.
                let mut patch = Vec::with_capacity(row * usize::from(height));
                for line in 0..usize::from(height) {
                    let at = (usize::from(source_y) + line) * stride + usize::from(source_x) * 4;
                    patch.extend_from_slice(&self.pixels[at..at + row]);
                }
                for line in 0..usize::from(height) {
                    let at = (usize::from(y) + line) * stride + usize::from(x) * 4;
                    let from = line * row;
                    self.pixels[at..at + row].copy_from_slice(&patch[from..from + row]);
                }
                Ok(Some(self.region(x, y, width, height)))
            }
        }
    }

    /// Refuses a rectangle the server placed outside the desktop it described.
    fn place(
        &self,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    ) -> Result<(u16, u16, u16, u16), String> {
        let right = u32::from(x) + u32::from(width);
        let bottom = u32::from(y) + u32::from(height);
        if width == 0
            || height == 0
            || right > u32::from(self.width)
            || bottom > u32::from(self.height)
        {
            return Err(format!(
                "the server sent a {width}x{height} rectangle at {x},{y} outside its {}x{} desktop",
                self.width, self.height
            ));
        }
        Ok((x, y, width, height))
    }

    fn region(&self, x: u16, y: u16, width: u16, height: u16) -> Event {
        let stride = self.stride();
        let row = usize::from(width) * 4;
        let mut pixels = Vec::with_capacity(row * usize::from(height));
        for line in 0..usize::from(height) {
            let at = (usize::from(y) + line) * stride + usize::from(x) * 4;
            pixels.extend_from_slice(&self.pixels[at..at + row]);
        }
        Event::Frame {
            x: i32::from(x),
            y: i32::from(y),
            width: i32::from(width),
            height: i32::from(height),
            desktop_width: u32::from(self.width),
            desktop_height: u32::from(self.height),
            stride: row as u32,
            pixels,
        }
    }
}

/// What one session did, for the trace line: how many bytes it read, how many
/// server messages it decoded and how many frame events it emitted.
#[derive(Default)]
struct Stats {
    bytes: u64,
    messages: u64,
    frames: u64,
    started: Option<std::time::Instant>,
}

struct Session {
    transport: Transport,
    incoming: Incoming,
    desktop: Desktop,
    emit: Emit,
    clipboard: bool,
    /// Whether the server speaks the extended clipboard and can carry text.
    extended: bool,
    /// The Latin-1 notice is said once per session.
    latin1_reported: bool,
    stats: Stats,
}

impl Session {
    fn new(opened: Opened, config: &Config, emit: &Emit) -> Session {
        Session {
            transport: opened.transport,
            incoming: Incoming::default(),
            desktop: Desktop::new(opened.width, opened.height),
            emit: Arc::clone(emit),
            clipboard: config.clipboard,
            extended: false,
            latin1_reported: false,
            stats: Stats::default(),
        }
    }

    fn run(&mut self, control: &Arc<Control>, commands: &Receiver<Command>) -> Result<(), Ended> {
        self.stats.started = Some(std::time::Instant::now());
        let outcome = self.pump(control, commands);
        if let Some(started) = self.stats.started {
            super::trace(&format!(
                "session stats bytes={} messages={} frames={} elapsed_ms={}",
                self.stats.bytes,
                self.stats.messages,
                self.stats.frames,
                started.elapsed().as_millis()
            ));
        }
        outcome
    }

    fn pump(&mut self, control: &Arc<Control>, commands: &Receiver<Command>) -> Result<(), Ended> {
        self.transport.set_read_timeout(Some(POLL));
        self.configure()?;
        publish(&self.emit, control, &State::Connected, Failure::None, "");
        let mut chunk = vec![0u8; CHUNK];
        loop {
            if control.cancelled() {
                return Err(Ended::new(Failure::Cancelled, "cancelled"));
            }
            self.drain(commands)?;
            match self.transport.read(&mut chunk) {
                Ok(0) => {
                    super::trace("server closed the connection");
                    return Err(Ended::new(
                        Failure::RemoteClosed,
                        "the VNC server closed the session",
                    ));
                }
                Ok(read) => {
                    self.stats.bytes += read as u64;
                    self.incoming.push(&chunk[..read]);
                    self.messages()?;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(Ended::io(error, Failure::Io)),
            }
        }
    }

    /// The pixel format, the encodings and the first full request. A server sends
    /// nothing until it is asked, so this is what makes a frame arrive.
    fn configure(&mut self) -> Result<(), Ended> {
        // 32bpp, depth 24, little endian, true colour, red at 16, green at 8,
        // blue at 0: the BGRA bytes the pane draws and every other backend emits.
        let mut format = vec![0u8, 0, 0, 0];
        format.extend_from_slice(&[32, 24, 0, 1]);
        for maximum in [255u16; 3] {
            format.extend_from_slice(&maximum.to_be_bytes());
        }
        for shift in [16u8, 8, 0] {
            format.push(shift);
        }
        format.extend_from_slice(&[0, 0, 0]);
        self.send(&format)?;

        // Only encodings this client decodes are requested. Hextile is offered
        // first: it carries a screen of text and UI in a fraction of raw's bytes,
        // and TigerVNC and libvncserver both speak it. Raw stays in the list so a
        // server that supports nothing else still has a way to answer.
        let mut encodings: Vec<i32> = vec![5, 0, 1, -223, -224];
        if self.clipboard {
            encodings.push(EXTENDED_CLIPBOARD);
        }
        let mut request = vec![2u8, 0];
        request.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
        for encoding in encodings {
            request.extend_from_slice(&encoding.to_be_bytes());
        }
        self.send(&request)?;
        self.request(false)
    }

    fn request(&mut self, incremental: bool) -> Result<(), Ended> {
        // Type, incremental, then the region: x, y, width, height.
        let mut message = vec![3u8, u8::from(incremental)];
        message.extend_from_slice(&[0, 0, 0, 0]);
        message.extend_from_slice(&self.desktop.width.to_be_bytes());
        message.extend_from_slice(&self.desktop.height.to_be_bytes());
        self.send(&message)
    }

    /// Applies every queued input or clipboard command before the next read.
    fn drain(&mut self, commands: &Receiver<Command>) -> Result<(), Ended> {
        loop {
            match commands.try_recv() {
                Ok(Command::Key { keysym, down }) => self.key(keysym, down)?,
                Ok(Command::Pointer { x, y, buttons }) => self.pointer(x, y, buttons)?,
                Ok(Command::Clipboard(entries)) => self.offer(&entries)?,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
    }

    /// Consumes every whole message that arrived.
    fn messages(&mut self) -> Result<(), Ended> {
        loop {
            let message = self
                .incoming
                .take()
                .map_err(|reason| Ended::new(Failure::ProtocolFailure, reason))?;
            let Some(message) = message else {
                return Ok(());
            };
            self.stats.messages += 1;
            match message {
                Message::Update(rects) => {
                    super::trace(&format!("update of {} rectangles", rects.len()));
                    for rect in rects {
                        if let Some(event) = self
                            .desktop
                            .apply(rect)
                            .map_err(|reason| Ended::new(Failure::ProtocolFailure, reason))?
                        {
                            self.stats.frames += 1;
                            (self.emit)(event);
                        }
                    }
                    self.request(true)?;
                }
                Message::CutText(bytes) => {
                    super::trace(&format!("classic cut text of {} bytes", bytes.len()));
                    if self.clipboard {
                        clipboard::deliver(&self.emit, clipboard::classic(&bytes));
                    }
                }
                Message::ExtendedCutText(payload) => {
                    if std::env::var_os("SHELLUNE_VNC_TRACE").is_some() {
                        let head: Vec<String> =
                            payload.iter().map(|byte| format!("{byte:02x}")).collect();
                        eprintln!(
                            "vnc trace: extended cut text of {} bytes [{}]",
                            payload.len(),
                            head.join(" ")
                        );
                    }
                    if !self.clipboard {
                        continue;
                    }
                    match clipboard::parse_extended(&payload) {
                        Some(Extended::Caps { text }) => {
                            if text {
                                self.extended = true;
                            }
                        }
                        Some(Extended::Text(text)) => {
                            // A provided text proves the server can carry one.
                            self.extended = true;
                            clipboard::deliver(&self.emit, text);
                        }
                        Some(Extended::Other) => super::trace("extended request/peek"),
                        None => super::trace("an extended cut text did not decode"),
                    }
                }
                Message::Bell | Message::Ignored => {}
            }
        }
    }

    fn key(&mut self, keysym: u32, down: bool) -> Result<(), Ended> {
        if keysym == 0 {
            return Ok(());
        }
        let mut message = vec![4u8, u8::from(down), 0, 0];
        message.extend_from_slice(&keysym.to_be_bytes());
        self.send(&message)
    }

    /// Pointer buttons use the RFB mask. A wheel step is one press and its release,
    /// which is how a server sees a click of wheel button 4 or 5.
    fn pointer(&mut self, x: u16, y: u16, buttons: u32) -> Result<(), Ended> {
        let mask = (buttons & 0x7) as u8;
        self.send_pointer(x, y, mask)?;
        if buttons & 0x8 != 0 {
            self.send_pointer(x, y, mask | 0x8)?;
            self.send_pointer(x, y, mask)?;
        }
        if buttons & 0x10 != 0 {
            self.send_pointer(x, y, mask | 0x10)?;
            self.send_pointer(x, y, mask)?;
        }
        Ok(())
    }

    fn send_pointer(&mut self, x: u16, y: u16, mask: u8) -> Result<(), Ended> {
        let mut message = vec![5u8, mask];
        message.extend_from_slice(&x.to_be_bytes());
        message.extend_from_slice(&y.to_be_bytes());
        self.send(&message)
    }

    fn offer(&mut self, entries: &[crate::ClipboardEntry]) -> Result<(), Ended> {
        let Some(entry) = entries
            .iter()
            .find(|entry| entry.kind == ClipboardKind::Text)
        else {
            return Ok(());
        };
        let text = String::from_utf8_lossy(&entry.bytes).into_owned();
        if self.extended {
            let (notify, provide) = clipboard::provide(&text);
            self.cut_text_extended(&notify)?;
            self.cut_text_extended(&provide)?;
            return Ok(());
        }
        // Plain cut text is Latin-1. Text that does not fit is not sent at all: a
        // server would otherwise paste mojibake.
        match clipboard::utf8_to_latin1(text.as_bytes()) {
            Some(bytes) => self.cut_text(&bytes),
            None => {
                if !self.latin1_reported {
                    self.latin1_reported = true;
                    (self.emit)(Event::Notice {
                        notice: Notice::ClipboardLatin1Only,
                    });
                }
                Ok(())
            }
        }
    }

    fn cut_text(&mut self, bytes: &[u8]) -> Result<(), Ended> {
        let mut message = vec![6u8, 0, 0, 0];
        message.extend_from_slice(&(bytes.len() as i32).to_be_bytes());
        message.extend_from_slice(bytes);
        self.send(&message)
    }

    fn cut_text_extended(&mut self, payload: &[u8]) -> Result<(), Ended> {
        let mut message = vec![6u8, 0, 0, 0];
        message.extend_from_slice(&(-(payload.len() as i32)).to_be_bytes());
        message.extend_from_slice(payload);
        self.send(&message)
    }

    fn send(&mut self, message: &[u8]) -> Result<(), Ended> {
        if std::env::var_os("SHELLUNE_VNC_TRACE").is_some() {
            let head: Vec<String> = message
                .iter()
                .take(20)
                .map(|byte| format!("{byte:02x}"))
                .collect();
            eprintln!(
                "vnc trace: send {} bytes [{}]",
                message.len(),
                head.join(" ")
            );
        }
        self.transport
            .write_all(message)
            .and_then(|()| self.transport.flush())
            .map_err(|error| Ended::io(error, Failure::Io))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desktop() -> Desktop {
        Desktop::new(4, 3)
    }

    #[test]
    fn a_raw_rectangle_is_copied_into_the_desktop_and_emitted() {
        let mut desktop = desktop();
        let pixels = vec![7u8; 2 * 4];
        let event = desktop
            .apply(Rect::Raw {
                x: 1,
                y: 2,
                width: 2,
                height: 1,
                pixels,
            })
            .expect("applied")
            .expect("a frame");
        let Event::Frame {
            x,
            y,
            width,
            height,
            stride,
            pixels,
            desktop_width,
            ..
        } = event
        else {
            panic!("a frame");
        };
        assert_eq!((x, y, width, height), (1, 2, 2, 1));
        assert_eq!(stride, 8);
        assert_eq!(desktop_width, 4);
        assert_eq!(pixels, vec![7u8; 8]);
    }

    #[test]
    fn a_copy_rectangle_reads_before_it_writes() {
        let mut desktop = desktop();
        let mut pixels = vec![0u8; 2 * 4];
        pixels[..4].copy_from_slice(&[1, 2, 3, 4]);
        pixels[4..].copy_from_slice(&[5, 6, 7, 8]);
        desktop
            .apply(Rect::Raw {
                x: 0,
                y: 0,
                width: 2,
                height: 1,
                pixels,
            })
            .expect("applied");
        desktop
            .apply(Rect::Copy {
                x: 1,
                y: 0,
                width: 2,
                height: 1,
                source_x: 0,
                source_y: 0,
            })
            .expect("applied");
        assert_eq!(&desktop.pixels[4..12], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn a_rectangle_outside_the_desktop_is_refused() {
        let mut desktop = desktop();
        let error = desktop
            .apply(Rect::Raw {
                x: 3,
                y: 0,
                width: 2,
                height: 1,
                pixels: vec![0; 8],
            })
            .expect_err("outside the desktop");
        assert!(error.contains("outside"), "{error}");
    }

    #[test]
    fn a_resize_replaces_the_buffer_and_is_bounded() {
        let mut desktop = desktop();
        desktop
            .apply(Rect::Resize {
                width: 8,
                height: 6,
            })
            .expect("resized");
        assert_eq!(desktop.pixels.len(), 8 * 6 * 4);
        assert!(desktop.resize(0, 6).is_err());
    }
}
