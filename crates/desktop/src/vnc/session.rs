//! One VNC attempt: the handshake, the request that produces the first frame, the
//! update loop, input and both clipboard forms.
//!
//! Short waits for incoming bytes let the worker service input and cancellation
//! while the desktop is idle. Incomplete messages stay buffered between reads.
use super::clipboard::{self, EXTENDED_CLIPBOARD, Extended};
use super::handshake::{self, Opened, Transport};
use super::wire::{Incoming, Message, Rect};
use super::{Command, Config, Ended};
use crate::control::{Control, Emit, publish};
use crate::input::Receiver;
use crate::{ClipboardKind, Event, Failure, MAX_FRAME_BYTES, Notice, State};
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::Duration;

/// One read wait once the desktop is up: the loop returns to its cancellation
/// check after each, and a partial message stays buffered.
const POLL: Duration = Duration::from_millis(32);
/// The decoder retains an incomplete tile or pixel band across reads.
const CHUNK: usize = 32 * 1024;

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
                        if attempt < super::ATTEMPTS && ended.reconnect {
                            commands.disconnect();
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
        let bytes = u64::from(width) * u64::from(height) * 4;
        if width == 0 || height == 0 || bytes > MAX_FRAME_BYTES as u64 {
            return Err(format!(
                "the server resized the desktop to {width}x{height}, past the {MAX_FRAME_BYTES} byte bound"
            ));
        }
        self.width = width;
        self.height = height;
        self.pixels = vec![0; bytes as usize];
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
                Ok(Some(self.frame(x, y, width, height, pixels)))
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
                Ok(Some(self.frame(x, y, width, height, patch)))
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

    fn frame(&self, x: u16, y: u16, width: u16, height: u16, pixels: Vec<u8>) -> Event {
        Event::Frame {
            x: i32::from(x),
            y: i32::from(y),
            width: i32::from(width),
            height: i32::from(height),
            desktop_width: u32::from(self.width),
            desktop_height: u32::from(self.height),
            stride: u32::from(width) * 4,
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
    clipboard_flags: u32,
    auto_text_limit: u32,
    local_text: Option<Vec<u8>>,
    /// None until the server acknowledges the extension, then the subscribed area.
    continuous: Option<(u16, u16)>,
    continuous_announced: bool,
    screen: Option<(u32, u32)>,
    desired_size: Option<(u16, u16)>,
    resize_after: std::time::Instant,
    resizing: bool,
    full_refresh: bool,
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
            clipboard_flags: 0,
            auto_text_limit: 0,
            local_text: None,
            continuous: None,
            continuous_announced: false,
            screen: None,
            desired_size: Some((config.width, config.height)),
            resize_after: std::time::Instant::now(),
            resizing: false,
            full_refresh: false,
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
        self.transport.poll_reads_every(POLL)?;
        self.configure()?;
        publish(&self.emit, control, &State::Connected, Failure::None, "");
        let mut chunk = vec![0u8; CHUNK];
        loop {
            if control.cancelled() {
                return Err(Ended::new(Failure::Cancelled, "cancelled"));
            }
            self.drain(commands)?;
            self.resize_if_ready()?;
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
                    self.messages(control, commands)?;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
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

        // Only encodings this client decodes are requested. ZRLE compresses tiles across updates;
        // peers without it can select Hextile. Raw stays in the list so a
        // server that supports nothing else still has a way to answer.
        let mut encodings: Vec<i32> = vec![
            16,
            5,
            0,
            1,
            -308,
            -223,
            -224,
            super::wire::RICH_CURSOR,
            -312,
            -313,
        ];
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
        self.request_region(3, incremental)
    }

    fn request_region(&mut self, kind: u8, enabled: bool) -> Result<(), Ended> {
        let mut message = vec![kind, u8::from(enabled)];
        message.extend_from_slice(&[0, 0, 0, 0]);
        message.extend_from_slice(&self.desktop.width.to_be_bytes());
        message.extend_from_slice(&self.desktop.height.to_be_bytes());
        self.send(&message)
    }

    /// Applies every queued input or clipboard command before the next read.
    fn drain(&mut self, commands: &Receiver<Command>) -> Result<(), Ended> {
        for _ in 0..64 {
            match commands.pop() {
                Some(Command::Key { keysym, down }) => self.key(keysym, down)?,
                Some(Command::Pointer { x, y, buttons }) => self.pointer(x, y, buttons)?,
                Some(Command::Resize(w, h)) => {
                    self.desired_size = Some((w, h));
                    self.resize_after = std::time::Instant::now() + Duration::from_millis(200);
                }
                Some(Command::Clipboard(entries)) => self.offer(&entries)?,
                None => break,
            }
        }
        Ok(())
    }

    /// Publishes available pixel bands and requests the next update at the end.
    fn messages(&mut self, control: &Control, commands: &Receiver<Command>) -> Result<(), Ended> {
        let mut serviced = std::time::Instant::now();
        loop {
            // A single compressed read may contain a whole desktop's worth of tiles.
            if serviced.elapsed() >= Duration::from_millis(4) {
                if control.cancelled() {
                    return Err(Ended::new(Failure::Cancelled, "cancelled"));
                }
                self.drain(commands)?;
                serviced = std::time::Instant::now();
            }
            let message = self
                .incoming
                .take()
                .map_err(|reason| Ended::new(Failure::ProtocolFailure, reason))?;
            let Some(message) = message else {
                return Ok(());
            };
            if !matches!(message, Message::Rectangle(_)) {
                self.stats.messages += 1;
            }
            match message {
                Message::Layout {
                    width,
                    height,
                    reason,
                    result,
                    screen,
                } => {
                    if reason == 1 {
                        self.resizing = false;
                    }
                    if reason != 1 || result == 0 {
                        self.screen = screen;
                        if (width, height) != (self.desktop.width, self.desktop.height) {
                            self.desktop
                                .resize(width, height)
                                .map_err(|e| Ended::new(Failure::ProtocolFailure, e))?;
                            self.full_refresh = true;
                        }
                    }
                }
                Message::Rectangle(rect) => {
                    if matches!(rect, Rect::Resize { .. }) {
                        self.full_refresh = true;
                    }
                    if let Some(event) = self
                        .desktop
                        .apply(rect)
                        .map_err(|reason| Ended::new(Failure::ProtocolFailure, reason))?
                    {
                        if self.stats.frames == 0 {
                            commands.resume();
                        }
                        self.stats.frames += 1;
                        (self.emit)(event);
                    }
                }
                Message::Pointer(shape) => (self.emit)(Event::Pointer(shape)),
                Message::UpdateEnd => {
                    let size = (self.desktop.width, self.desktop.height);
                    let full = std::mem::take(&mut self.full_refresh);
                    match self.continuous {
                        None => self.request(!full)?,
                        Some(region) if region != size => {
                            self.request_region(150, true)?;
                            self.continuous = Some(size);
                            self.request(false)?;
                        }
                        Some(_) if full => self.request(false)?,
                        _ => {}
                    }
                }
                Message::EndContinuous if !self.continuous_announced => {
                    self.continuous_announced = true;
                    self.request_region(150, true)?;
                    self.continuous = Some((self.desktop.width, self.desktop.height));
                }
                Message::EndContinuous => {
                    self.continuous = None;
                    self.request(true)?;
                }
                Message::Fence { flags, payload } => {
                    if flags & 0x8000_0000 != 0 {
                        // The single decoder processes messages in wire order,
                        // so BlockBefore and BlockAfter are already satisfied.
                        let mut reply = vec![248, 0, 0, 0];
                        reply.extend_from_slice(&(flags & 3).to_be_bytes());
                        reply.push(payload.len() as u8);
                        reply.extend_from_slice(&payload);
                        self.send(&reply)?;
                    }
                }
                Message::CutText(bytes) => {
                    super::trace(&format!("classic cut text of {} bytes", bytes.len()));
                    if self.clipboard {
                        self.local_text = None;
                        clipboard::deliver(&self.emit, clipboard::classic(&bytes));
                    }
                }
                Message::ExtendedCutText(payload) => {
                    super::trace(&format!("extended cut text of {} bytes", payload.len()));
                    if !self.clipboard {
                        continue;
                    }
                    match clipboard::parse_extended(&payload) {
                        Some(Extended::Caps { flags, text_limit }) => {
                            self.extended = flags & clipboard::EXT_TEXT != 0;
                            self.clipboard_flags = flags;
                            self.auto_text_limit = text_limit;
                            let mut caps = (clipboard::EXT_CAPS
                                | clipboard::EXT_TEXT
                                | clipboard::EXT_NOTIFY
                                | clipboard::EXT_PEEK
                                | clipboard::EXT_REQUEST
                                | clipboard::EXT_PROVIDE)
                                .to_be_bytes()
                                .to_vec();
                            caps.extend_from_slice(
                                &((crate::MAX_CLIPBOARD_TEXT_BYTES + 1) as u32).to_be_bytes(),
                            );
                            self.cut_text_extended(&caps)?;
                        }
                        Some(Extended::Notify(has_text)) => {
                            self.local_text = None;
                            if has_text && self.clipboard_flags & clipboard::EXT_REQUEST != 0 {
                                self.cut_text_extended(
                                    &(clipboard::EXT_REQUEST | clipboard::EXT_TEXT).to_be_bytes(),
                                )?;
                            }
                        }
                        Some(Extended::Request) => self.provide_local_text()?,
                        Some(Extended::Peek)
                            if self.clipboard_flags & clipboard::EXT_NOTIFY != 0 =>
                        {
                            let format = if self.local_text.is_some() {
                                clipboard::EXT_TEXT
                            } else {
                                0
                            };
                            self.cut_text_extended(
                                &(clipboard::EXT_NOTIFY | format).to_be_bytes(),
                            )?;
                        }
                        Some(Extended::Text(text)) => {
                            self.local_text = None;
                            self.extended = true;
                            clipboard::deliver(&self.emit, text);
                        }
                        Some(Extended::Other | Extended::Peek) => {}
                        None => super::trace("an extended cut text did not decode"),
                    }
                }
                Message::Bell | Message::Ignored => {}
            }
        }
    }

    fn resize_if_ready(&mut self) -> Result<(), Ended> {
        let (Some((id, flags)), Some((w, h))) = (self.screen, self.desired_size) else {
            return Ok(());
        };
        if self.resizing || std::time::Instant::now() < self.resize_after {
            return Ok(());
        }
        self.desired_size = None;
        if (self.desktop.width, self.desktop.height) == (w, h) {
            return Ok(());
        }
        let mut packet = vec![251, 0];
        packet.extend_from_slice(&w.to_be_bytes());
        packet.extend_from_slice(&h.to_be_bytes());
        packet.extend_from_slice(&[1, 0]);
        packet.extend_from_slice(&id.to_be_bytes());
        packet.extend_from_slice(&[0; 4]);
        packet.extend_from_slice(&w.to_be_bytes());
        packet.extend_from_slice(&h.to_be_bytes());
        packet.extend_from_slice(&flags.to_be_bytes());
        self.send(&packet)?;
        self.resizing = true;
        Ok(())
    }

    fn key(&mut self, keysym: u32, down: bool) -> Result<(), Ended> {
        if keysym == 0 {
            return Ok(());
        }
        let mut message = vec![4u8, u8::from(down), 0, 0];
        message.extend_from_slice(&keysym.to_be_bytes());
        self.send(&message)
    }

    /// RFB buttons 4–7 are momentary wheel presses. Write each press and
    /// release together so one wheel step needs only one transport flush.
    fn pointer(&mut self, x: u16, y: u16, buttons: u32) -> Result<(), Ended> {
        let held = (buttons & 7) as u8;
        let [xh, xl] = x.to_be_bytes();
        let [yh, yl] = y.to_be_bytes();
        let event = |mask| [5, mask, xh, xl, yh, yl];
        if buttons & 0x78 == 0 {
            return self.send(&event(held));
        }
        let mut messages = Vec::with_capacity(12);
        for bit in [8, 16, 32, 64] {
            if buttons & u32::from(bit) != 0 {
                messages.extend_from_slice(&event(held | bit));
                messages.extend_from_slice(&event(held));
            }
        }
        self.send(&messages)
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
            self.local_text = Some(entry.bytes.clone());
            if text.len() < self.auto_text_limit as usize
                && self.clipboard_flags & clipboard::EXT_PROVIDE != 0
            {
                self.provide_local_text()?;
            } else if self.clipboard_flags & clipboard::EXT_NOTIFY != 0 {
                self.cut_text_extended(
                    &(clipboard::EXT_NOTIFY | clipboard::EXT_TEXT).to_be_bytes(),
                )?;
            } else if self.clipboard_flags == 0 {
                // Older peers may send Provide without sending capabilities first.
                self.provide_local_text()?;
            }
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

    fn provide_local_text(&mut self) -> Result<(), Ended> {
        if let Some(text) = self.local_text.as_deref() {
            let payload = clipboard::provide(text).map_err(|e| Ended::io(e, Failure::Io))?;
            self.cut_text_extended(&payload)?;
        }
        Ok(())
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
            eprintln!(
                "vnc trace: send type={} bytes={}",
                message[0],
                message.len()
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
