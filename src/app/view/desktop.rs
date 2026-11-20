//! The desktop pane: the window's side of an RDP or VNC session (R8, R10,
//! R17-R20, R23).
//!
//! The pane owns no protocol logic. It draws one BGRA canvas with the fit GPUI
//! itself uses, maps the pointer through the inverse of that same fit, queues what
//! the user does and hands the queue to the link in order. Everything with a rule
//! worth testing lives in [crate::app::desktop]; what lives here is the window:
//! layout, focus, key and pointer events, the status card and the certificate
//! question.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    Bounds, Context, FocusHandle, KeyDownEvent, KeyUpEvent, Modifiers, ModifiersChangedEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, RenderImage,
    ScrollDelta, ScrollWheelEvent, SharedString, Subscription, Window, div, img, px,
};
use shellune_core::SessionFailure;

use crate::app::components::{self, Variant};
use crate::app::copy::{Locale, Strings};
use crate::app::desktop::{
    ALT_LEFT, Availability, Buttons, CONTROL_LEFT, ClipboardEntry, ClipboardKind,
    DESKTOP_POLL_INTERVAL, DesktopLink, DesktopSurface, DesktopTarget, DesktopZoom, DriveProblem,
    InputQueue, KeyCodes, LinkEvent, LinkFailure, LinkState, NativeLink, Notice, SHIFT_LEFT,
    availability, clamp_pan, content_rect, encryption_note, key_codes, to_remote_in,
};
use crate::app::desktop_clipboard;
use crate::app::icons::{self, Icon};
use crate::app::theme::{self, Elevated, Metrics, Tone, palette, spacing};

/// The size a session asks for before the window has measured anything.
///
/// A remote desktop negotiates its size at connect time, so the first ask is the
/// size of the window this build opens with; the pane renegotiates as soon as it
/// knows the space it really has.
pub const FIRST_DESKTOP_SIZE: (u32, u32) = (1280, 720);

/// The widest the notice strip over a picture grows, before text scale.
const NOTICE_WIDTH: f32 = 420.0;

/// The room the zoom step's words take between the two zoom buttons, before text
/// scale: "100%" and "Fit" in either language, so the buttons do not move.
const ZOOM_LABEL_WIDTH: f32 = 44.0;

/// Whether a certificate question is still waiting for an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CertificateQuestion {
    request_id: u32,
    host: String,
    subject: String,
    issuer: String,
    fingerprint: String,
}

impl CertificateQuestion {
    /// The one line the card says about what was presented.
    fn summary(&self) -> String {
        let subject = if self.subject.trim().is_empty() {
            self.host.clone()
        } else {
            self.subject.clone()
        };
        if self.issuer.trim().is_empty() {
            subject
        } else {
            format!("{subject} - {}", self.issuer)
        }
    }
}

/// Where a desktop pane is in its own terms (R17).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesktopPhase {
    /// The pane has not been started.
    NotStarted,
    /// The link is dialling, authenticating or waiting for a decision.
    Connecting,
    /// Frames and input flow.
    Live,
    /// The session ended, with the reason it states.
    Ended(SessionFailure),
}

impl DesktopPhase {
    /// Whether the pane still sends what the user does to the remote side.
    #[must_use]
    pub fn accepts_input(&self) -> bool {
        matches!(self, Self::Live) || matches!(self, Self::Connecting)
    }

    /// The failure the pane states, when it ended.
    #[must_use]
    pub fn failure(&self) -> Option<SessionFailure> {
        match self {
            Self::Ended(failure) => Some(failure.clone()),
            _ => None,
        }
    }
}

/// How far the connection has come, in the words the status card uses (R19).
///
/// R19 asks for the steps between opening a socket and showing a picture to be
/// visible, not folded into one "connecting" word. They are derived from what the
/// link actually reports: the dialling state, the certificate question the server
/// asks, the answer that starts the sign-in, and the moment the session says it is
/// connected while no picture has arrived yet. A step a server never takes is never
/// claimed - a VNC server that asks for neither a certificate nor a password has no
/// such step to show - and a failure stays at the step it happened on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopStage {
    /// Nothing has been started.
    NotStarted,
    /// The socket is being opened and the protocols are being greeted.
    Connecting,
    /// The server asked about its certificate and is waiting for the answer.
    VerifyingCertificate,
    /// The credentials went out and the server is deciding.
    Authenticating,
    /// The server accepted the session and the desktop is being built.
    Establishing,
}

impl DesktopStage {
    /// The one line the status card says while the pane is on this step.
    #[must_use]
    pub fn headline(&self, strings: &crate::app::copy::Strings) -> &'static str {
        match self {
            Self::NotStarted | Self::Connecting => strings.desktop_stage_connecting,
            Self::VerifyingCertificate => strings.desktop_stage_certificate,
            Self::Authenticating => strings.desktop_stage_signing_in,
            Self::Establishing => strings.desktop_stage_establishing,
        }
    }
}

/// Something the pane tells the user about a session beside its state (R1-R5).
///
/// Each is shown once per session: a server that refuses the clipboard refuses it on
/// every paste, and saying so every time would bury the picture under one sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopNotice {
    /// The server does not allow the clipboard.
    ClipboardRefused,
    /// The VNC server's clipboard takes only Latin-1 text.
    ClipboardLatin1,
    /// Clipboard text over the bound was cut.
    TextTooLarge,
    /// A clipboard image over the bound was left out.
    ImageTooLarge,
    /// The mapped folder could not be offered.
    DriveUnavailable,
    /// The server did not take the mapped folder.
    DriveRefused,
}

impl DesktopNotice {
    /// The notice a backend report is, or `None` for one the pane does not show.
    fn of(notice: Notice) -> Option<Self> {
        Some(match notice {
            Notice::ClipboardRefused => Self::ClipboardRefused,
            Notice::ClipboardLatin1Only => Self::ClipboardLatin1,
            Notice::ClipboardTooLarge(ClipboardKind::Text) => Self::TextTooLarge,
            Notice::ClipboardTooLarge(ClipboardKind::Png | ClipboardKind::Dib) => {
                Self::ImageTooLarge
            }
            Notice::DriveUnavailable(_) => Self::DriveUnavailable,
            Notice::DriveRefused(_) => Self::DriveRefused,
            Notice::ClipboardReady | Notice::Unknown(_) => return None,
        })
    }

    /// The selector of the line that says it, for the tests.
    #[must_use]
    pub fn selector(self) -> &'static str {
        match self {
            Self::ClipboardRefused => "desktop.notice.clipboard_refused",
            Self::ClipboardLatin1 => "desktop.notice.clipboard_latin1",
            Self::TextTooLarge => "desktop.notice.text_too_large",
            Self::ImageTooLarge => "desktop.notice.image_too_large",
            Self::DriveUnavailable => "desktop.notice.drive_unavailable",
            Self::DriveRefused => "desktop.notice.drive_refused",
        }
    }
}

/// The sentence a notice is shown with.
fn notice_text(notice: Notice, strings: &Strings) -> String {
    match notice {
        Notice::ClipboardRefused => strings.desktop_notice_clipboard_refused.to_owned(),
        Notice::ClipboardLatin1Only => strings.desktop_notice_clipboard_latin1.to_owned(),
        Notice::ClipboardTooLarge(ClipboardKind::Text) => {
            strings.desktop_notice_text_too_large.to_owned()
        }
        Notice::ClipboardTooLarge(_) => strings.desktop_notice_image_too_large.to_owned(),
        Notice::DriveUnavailable(problem) => {
            let reason = match problem {
                DriveProblem::Missing => strings.desktop_drive_missing,
                DriveProblem::NotAFolder => strings.desktop_drive_not_a_folder,
                DriveProblem::NotAbsolute => strings.desktop_drive_not_absolute,
                DriveProblem::TooLong => strings.desktop_drive_too_long,
                DriveProblem::NoAccess => strings.desktop_drive_no_access,
                DriveProblem::NotAdded => strings.desktop_drive_not_added,
                DriveProblem::Other(_) => strings.desktop_drive_other,
            };
            strings
                .desktop_notice_drive_unavailable
                .replace("{reason}", reason)
        }
        Notice::DriveRefused(_) => strings.desktop_notice_drive_refused.to_owned(),
        Notice::ClipboardReady | Notice::Unknown(_) => String::new(),
    }
}

/// The modifiers the remote side holds for a key pressed with these held here.
///
/// On macOS Command is where Control is elsewhere: Cmd+V and Cmd+C mean to a
/// Windows or Linux desktop what Ctrl+V and Ctrl+C mean, so they reach it as those.
/// The Windows key is not forwarded: a press the remote side reads alone opens its
/// start menu.
fn remote_modifiers(modifiers: &Modifiers) -> [(KeyCodes, bool); 3] {
    let control = shortcut_modifier_held(modifiers);
    [
        (CONTROL_LEFT, control),
        (ALT_LEFT, modifiers.alt),
        (SHIFT_LEFT, modifiers.shift),
    ]
}

/// Whether the modifier of copy and paste shortcuts is held: Control, and on macOS
/// Command as well.
fn shortcut_modifier_held(modifiers: &Modifiers) -> bool {
    modifiers.control || (cfg!(target_os = "macos") && modifiers.platform)
}

/// A number the system moves on whenever anything is put on the clipboard, or
/// `None` where there is none to ask.
///
/// Reading the clipboard is not free: a Windows screenshot is a bitmap that is
/// encoded to PNG on every read, on the window's thread. The pane asks this first
/// and reads only when something was copied since its last read.
#[cfg(all(windows, not(test)))]
fn clipboard_generation() -> Option<u32> {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetClipboardSequenceNumber() -> u32;
    }
    // SAFETY: the call takes no arguments and touches no memory of this process.
    let number = unsafe { GetClipboardSequenceNumber() };
    // Zero is the answer of a session that may not see the clipboard's number.
    (number != 0).then_some(number)
}

/// The tests write a clipboard of their own that has no such number, and other
/// systems keep none GPUI exposes.
#[cfg(not(all(windows, not(test))))]
fn clipboard_generation() -> Option<u32> {
    None
}

/// One remote desktop on screen.
pub struct DesktopPane {
    target: DesktopTarget,
    availability: Availability,
    focus: FocusHandle,
    link: Option<Box<dyn DesktopLink>>,
    surface: DesktopSurface,
    input: InputQueue,
    phase: DesktopPhase,
    /// The step of the connection the status card names (R19).
    stage: DesktopStage,
    /// When this attempt started, while it is still connecting.
    ///
    /// `None` on a pane that was handed a link (tests, the capture) and on every
    /// pane that is not connecting, so the status card is always drawn there. A real
    /// attempt records the moment it starts, and a link that comes up in under
    /// [`crate::app::CONNECT_STATE_FLOOR`] never draws the card at all.
    connecting_since: Option<std::time::Instant>,
    /// The step the session stopped on, kept so the reason can name it.
    failed_stage: Option<DesktopStage>,
    /// How the picture is scaled into the pane (R18).
    zoom: DesktopZoom,
    /// How far a picture larger than the pane is panned, in window pixels.
    pan: (f32, f32),
    /// Whether this pane asked the window to fill the screen (R18).
    fullscreen: bool,
    /// The space the picture is drawn in, measured from the last painted frame.
    viewport: Option<Bounds<Pixels>>,
    /// The size last asked of the protocol, so a resize is sent once.
    asked_size: (u32, u32),
    /// The last remote pixel a motion reported, so movement inside one pixel is not
    /// sent twice.
    last_sent: Option<(u16, u16)>,
    certificate: Option<CertificateQuestion>,
    /// What the protocol layer said about why it ended, kept verbatim.
    layer_reason: Option<String>,
    /// What the last start was handed to sign in with, for the tests that prove the
    /// password reaches the link. `None` until a start; `Some(None)` is "no password".
    #[cfg(test)]
    started_password: Option<Option<String>>,
    /// How many times this pane was asked to start, for the tests that prove one
    /// connection is one attempt. A guard keeps a second start from dialling, and
    /// the count says whether one was even asked for.
    #[cfg(test)]
    starts: usize,
    /// Why the last send failed, when it did, shown once beside the surface.
    send_problem: Option<String>,
    polling: bool,
    watching_focus: bool,
    /// Whether the window was the active one at the last draw, so the release on
    /// losing activation happens once instead of on every frame of an inactive
    /// window.
    was_active: bool,
    /// The picture the window draws, rebuilt once per batch of frames.
    image: Option<Arc<RenderImage>>,
    image_size: (u32, u32),
    locale: Locale,
    /// The parts of the clipboard content last offered to the remote side or written
    /// here from it, so the same content is not offered again (R1-R3).
    shared: Vec<u64>,
    /// The clipboard's number at the pane's last read of it, so an unchanged
    /// clipboard is not read again (see [clipboard_generation]).
    read_generation: Option<u32>,
    /// Whether this session has yet to be offered the clipboard since it went live.
    /// A pane that keeps the keyboard through a reconnect sees no focus event, so
    /// the first frame drawn live and focused offers it instead (R1-R4).
    offer_when_live: bool,
    /// The notices on screen, with their sentences, in the order they arrived.
    notices: Vec<(DesktopNotice, String)>,
    /// Every notice this session has shown, so each is shown once.
    noticed: Vec<DesktopNotice>,
    /// The focus and activation observers, alive as long as the pane.
    subscriptions: Vec<Subscription>,
    /// The first control of the banner the window draws above an ended session, which
    /// Tab reaches from a pane that no longer sends keys (ADR-6, ADR-7). The window
    /// hands it in on every draw, and `None` when no banner is shown.
    banner_focus: std::cell::RefCell<Option<FocusHandle>>,
}

impl DesktopPane {
    /// A pane for a target that has not been started.
    pub fn new(target: DesktopTarget, locale: Locale, cx: &mut Context<Self>) -> Self {
        Self {
            availability: availability(target.protocol()),
            target,
            focus: cx.focus_handle(),
            link: None,
            surface: DesktopSurface::new(),
            input: InputQueue::new(),
            phase: DesktopPhase::NotStarted,
            stage: DesktopStage::NotStarted,
            connecting_since: None,
            failed_stage: None,
            zoom: DesktopZoom::Fit,
            pan: (0.0, 0.0),
            fullscreen: false,
            viewport: None,
            asked_size: FIRST_DESKTOP_SIZE,
            last_sent: None,
            certificate: None,
            layer_reason: None,
            #[cfg(test)]
            started_password: None,
            #[cfg(test)]
            starts: 0,
            send_problem: None,
            polling: false,
            watching_focus: false,
            was_active: false,
            image: None,
            image_size: (0, 0),
            locale,
            shared: Vec::new(),
            read_generation: None,
            offer_when_live: true,
            notices: Vec::new(),
            noticed: Vec::new(),
            subscriptions: Vec::new(),
            banner_focus: std::cell::RefCell::new(None),
        }
    }

    /// Hands the pane the first control of the banner the window draws above it for
    /// the next draw, or `None` when no banner is shown (ADR-6).
    pub fn set_banner_focus(&self, focus: Option<FocusHandle>) {
        *self.banner_focus.borrow_mut() = focus;
    }

    /// A pane already holding a link, which is how the tests drive every rule the
    /// window has without a server, and how the capture photographs the chrome of a
    /// session no server on this machine can produce.
    #[cfg(any(test, feature = "ui-capture"))]
    pub fn with_link(
        target: DesktopTarget,
        link: Box<dyn DesktopLink>,
        locale: Locale,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut pane = Self::new(target, locale, cx);
        pane.link = Some(link);
        pane.phase = DesktopPhase::Connecting;
        pane.stage = DesktopStage::Connecting;
        pane
    }

    /// Where the pane is.
    #[must_use]
    pub fn phase(&self) -> &DesktopPhase {
        &self.phase
    }

    /// The step of the connection the pane is on (R19).
    #[must_use]
    pub fn stage(&self) -> DesktopStage {
        self.stage
    }

    /// The step the session stopped on, when it ended after it started.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn failed_stage(&self) -> Option<DesktopStage> {
        self.failed_stage
    }

    /// How the picture is scaled into the pane (R18).
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn zoom(&self) -> DesktopZoom {
        self.zoom
    }

    /// How far a zoomed picture is panned.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn pan(&self) -> (f32, f32) {
        self.pan
    }

    /// Whether this pane asked the window to fill the screen (R18).
    #[must_use]
    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    /// Takes the pane into the whole window, or gives it back (R18).
    ///
    /// The connection is not touched: only the window is asked for the screen and
    /// the pane keeps its link, its canvas and its place in the tab. Going back is
    /// the same call, which is what makes the round trip exact.
    pub fn set_fullscreen(
        &mut self,
        fullscreen: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.fullscreen == fullscreen {
            return;
        }
        self.fullscreen = fullscreen;
        // The window's own full screen is what hides the task bar and the window
        // frame; the pane's flag is what hides this application's head above it.
        if window.is_fullscreen() != fullscreen {
            window.toggle_fullscreen();
        }
        // A full screen is a bigger space, so the picture is re-fitted as soon as
        // the layout measures it; the size the protocol was asked for does not
        // change, because the pane cannot renegotiate a live session and a
        // remote resize would drop the picture it is showing.
        cx.notify();
    }

    /// Enters or leaves the whole window.
    pub fn toggle_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_fullscreen(!self.fullscreen, window, cx);
    }

    /// One step larger, for the zoom control and its shortcut (R18).
    pub fn zoom_in(&mut self, cx: &mut Context<Self>) {
        let next = self.zoom.zoomed_in();
        if next != self.zoom {
            self.zoom = next;
            self.pan = (0.0, 0.0);
            cx.notify();
        }
    }

    /// One step smaller, back to fitting the window at the bottom of the ladder.
    pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
        let next = self.zoom.zoomed_out();
        if next != self.zoom {
            self.zoom = next;
            self.pan = (0.0, 0.0);
            cx.notify();
        }
    }

    /// Back to the whole desktop in the space it has.
    pub fn zoom_fit(&mut self, cx: &mut Context<Self>) {
        if !self.zoom.is_fit() || self.pan != (0.0, 0.0) {
            self.zoom = DesktopZoom::Fit;
            self.pan = (0.0, 0.0);
            cx.notify();
        }
    }

    /// Moves a zoomed picture by one scroll step, keeping it over the pane.
    fn pan_by(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) {
        if self.zoom.is_fit() {
            return;
        }
        let Some(viewport) = self.viewport else {
            return;
        };
        let (width, height) = self.surface.size();
        let factor = f32::from(self.zoom.percent().unwrap_or(100)) / 100.0;
        let next = clamp_pan(
            viewport,
            (width as f32 * factor, height as f32 * factor),
            (self.pan.0 + dx, self.pan.1 + dy),
        );
        if next != self.pan {
            self.pan = next;
            cx.notify();
        }
    }

    /// The frame buffer, for the tests and the capture probe.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn surface(&self) -> &DesktopSurface {
        &self.surface
    }

    /// The focus handle the window gives the picture.
    #[must_use]
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// The reason the protocol layer itself gave for the last end, when it said
    /// anything.
    ///
    /// Kept beside the projection rather than inside it: the session record states
    /// the fact (a refused credential, a lost connection), and this is the layer's
    /// own sentence, which the status card shows under it.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn layer_reason(&self) -> Option<&str> {
        self.layer_reason
            .as_deref()
            .filter(|reason| !reason.trim().is_empty())
    }

    /// How many starts this pane was asked for, for the tests.
    #[cfg(test)]
    #[must_use]
    pub fn starts(&self) -> usize {
        self.starts
    }

    /// What the last [`Self::start`] was handed to sign in with, for the tests.
    #[cfg(test)]
    #[must_use]
    pub fn started_password(&self) -> Option<Option<&str>> {
        self.started_password
            .as_ref()
            .map(|password| password.as_deref())
    }

    /// Starts the session: dials it and takes the first frames.
    ///
    /// A protocol this build cannot speak ends the session with the reason instead
    /// of leaving a pane that looks like it is connecting.
    pub fn start(&mut self, password: Option<&str>, cx: &mut Context<Self>) {
        // Recorded before the link check: a pane a test mounted on a scripted link is
        // started the way the window starts one, and says what it was handed.
        #[cfg(test)]
        {
            self.starts += 1;
            self.started_password = Some(password.map(str::to_owned));
        }
        if self.link.is_some() {
            return;
        }
        if self.availability.is_built() {
            self.stage = DesktopStage::Connecting;
            self.connecting_since = Some(std::time::Instant::now());
            let (width, height) = self.asked_size;
            match NativeLink::open(&self.target, password, width, height) {
                Ok(link) => {
                    self.attach(Box::new(link), cx);
                }
                Err(error) => self.fail(LinkFailure::Connect, &error.reason(), cx),
            }
            return;
        }
        // A build without the backend is not a connection that failed: another
        // attempt cannot start a library that was not compiled, so the session says
        // so and stops offering one.
        self.phase = DesktopPhase::Ended(SessionFailure::ReconnectUnavailable {
            reason: format!(
                "this build cannot open {}: {}",
                self.protocol_label(),
                self.availability.report()
            ),
        });
        cx.notify();
    }

    fn protocol_label(&self) -> &'static str {
        match self.target.protocol() {
            shellune_core::ProtocolKind::Rdp => "RDP",
            shellune_core::ProtocolKind::Vnc => "VNC",
            _ => "a remote desktop",
        }
    }

    fn attach(&mut self, mut link: Box<dyn DesktopLink>, cx: &mut Context<Self>) {
        if let Err(error) = link.connect() {
            self.fail(LinkFailure::Connect, &error.reason(), cx);
            return;
        }
        self.link = Some(link);
        self.phase = DesktopPhase::Connecting;
        self.stage = DesktopStage::Connecting;
        self.connecting_since = Some(std::time::Instant::now());
        self.failed_stage = None;
        self.surface.reset();
        self.image = None;
        // A new session has said nothing yet and has not seen this clipboard.
        self.shared.clear();
        self.read_generation = None;
        self.offer_when_live = true;
        self.notices.clear();
        self.noticed.clear();
        self.start_polling(cx);
        cx.notify();
    }

    /// Ends the session with a reason, and stops polling.
    fn fail(&mut self, failure: LinkFailure, reason: &str, cx: &mut Context<Self>) {
        if !reason.trim().is_empty() {
            self.layer_reason = Some(reason.to_owned());
        }
        let target = self.target.target();
        let was_live = matches!(self.phase, DesktopPhase::Live);
        // The step it stopped on is part of the reason: "signing in" and "showing
        // the desktop" are different problems, and the card says which one it was.
        if !matches!(self.stage, DesktopStage::NotStarted) {
            self.failed_stage = Some(self.stage);
        }
        self.phase = DesktopPhase::Ended(failure.session_failure(reason, &target, was_live));
        self.connecting_since = None;
        self.polling = false;
        // Nothing is held after a session ended: the remote side would otherwise
        // keep a modifier down for a connection that is gone.
        self.release_input();
        self.flush_input();
        if let Some(mut link) = self.link.take() {
            link.shutdown();
        }
        cx.notify();
    }

    /// Stops the session and releases what it owns. Safe to call twice.
    pub fn stop(&mut self) {
        self.release_input();
        self.flush_input();
        if let Some(mut link) = self.link.take() {
            link.shutdown();
        }
        self.polling = false;
        if !matches!(self.phase, DesktopPhase::Ended(_)) {
            self.phase = DesktopPhase::Ended(SessionFailure::ConnectionLost);
        }
    }

    fn start_polling(&mut self, cx: &mut Context<Self>) {
        if self.polling {
            return;
        }
        self.polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(DESKTOP_POLL_INTERVAL).await;
                let keep_going = this.update(cx, |pane, cx| pane.drain(cx));
                match keep_going {
                    Ok(true) => {}
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// Whether the surface is showing a live picture, for a test or a capture.
    #[cfg(any(test, feature = "ui-capture"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_live(&self) -> bool {
        matches!(self.phase, DesktopPhase::Live)
    }

    /// Applies whatever the link has waiting, for a test or a capture.
    ///
    /// The polling task waits on the window's timer, which a test cannot advance and a
    /// capture has no frame to spend on, so both drive the same call directly -
    /// exactly as the terminal pane does.
    #[cfg(any(test, feature = "ui-capture"))]
    pub fn pump_once(&mut self, cx: &mut Context<Self>) -> bool {
        self.drain(cx)
    }

    /// Applies everything the link reported since the last poll.
    ///
    /// Every event is applied in the order it arrived: the regions of a frame that
    /// is still being decoded are copied into the canvas one after another and are
    /// never dropped in favour of a newer one. What is coalesced is the window's
    /// work - one notification and one picture per batch - because a snapshot that
    /// is half applied is not something the window should draw.
    ///
    /// Returns whether polling should continue.
    pub fn drain(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(link) = self.link.as_mut() else {
            return false;
        };
        let events = link.drain();
        // Input is flushed before the batch is applied so a key pressed during the
        // last interval reaches the peer in the same order the user produced it.
        self.flush_input();
        if events.is_empty() {
            return self.phase.accepts_input() || matches!(self.phase, DesktopPhase::NotStarted);
        }
        let mut keep = true;
        for event in events {
            keep = self.apply(event, cx) && keep;
        }
        cx.notify();
        keep
    }

    fn apply(&mut self, event: LinkEvent, cx: &mut Context<Self>) -> bool {
        match event {
            LinkEvent::Frame(region) => {
                if !self.surface.apply(&region) {
                    // A region that does not fit its own desktop is refused rather
                    // than written past the canvas; the session keeps running.
                    return true;
                }
                if self.phase == DesktopPhase::Connecting {
                    // The first complete region means the session is showing a
                    // picture: that is the moment it is live, not the state event.
                    self.phase = DesktopPhase::Live;
                    self.stage = DesktopStage::Establishing;
                    self.connecting_since = None;
                }
                true
            }
            LinkEvent::State {
                state,
                failure,
                reason,
                ..
            } => self.apply_state(state, failure, &reason, cx),
            LinkEvent::Certificate {
                request_id,
                host,
                subject,
                issuer,
                fingerprint,
                ..
            } => {
                let question = CertificateQuestion {
                    request_id,
                    host,
                    subject,
                    issuer,
                    fingerprint,
                };
                if matches!(
                    self.target.certificate(),
                    shellune_core::RdpCertificatePolicy::Reject
                ) {
                    // The connection said it does not accept an untrusted
                    // certificate: the answer is no, before anything is drawn, and
                    // the reason the session ends is the certificate. The answer
                    // carries the request id from this very event - there is no
                    // question on screen to take it from - so the server is told
                    // at once instead of waiting for a decision that never comes,
                    // and the link is shut down rather than held open behind a
                    // session that already ended.
                    self.stage = DesktopStage::VerifyingCertificate;
                    let answer = self
                        .link
                        .as_ref()
                        .map(|link| link.answer_certificate(request_id, false));
                    match answer {
                        Some(Ok(())) => {}
                        // A link that cannot take the answer is not left running:
                        // the shutdown in fail() releases it either way.
                        Some(Err(error)) => self.layer_reason = Some(error.reason()),
                        None => {}
                    }
                    self.fail(LinkFailure::CertificateRejected, "", cx);
                    return false;
                }
                self.certificate = Some(question);
                self.stage = DesktopStage::VerifyingCertificate;
                cx.notify();
                true
            }
            LinkEvent::Clipboard { entries } => {
                self.receive_clipboard(&entries, cx);
                true
            }
            LinkEvent::Notice(notice) => {
                self.note(notice);
                cx.notify();
                true
            }
        }
    }

    // ------------------------------------------------------------ clipboard

    /// Writes what the remote side copied to this computer's clipboard (R1-R3).
    ///
    /// The peer's clipboard becomes this computer's, which is the direction a user
    /// expects to be automatic. Text and image arrive together as one item. What was
    /// written is remembered as shared, so focusing the pane afterwards does not offer
    /// the same content straight back.
    fn receive_clipboard(&mut self, entries: &[ClipboardEntry], cx: &mut Context<Self>) {
        if !self.target.clipboard() {
            return;
        }
        let Some(item) = desktop_clipboard::local_item(entries) else {
            return;
        };
        self.shared = desktop_clipboard::prints(&item);
        let before = clipboard_generation();
        cx.write_to_clipboard(item);
        // What is on the clipboard now is what was just shared; reading it back
        // would only find that out again. The write reports no failure, so only a
        // number that moved proves it landed: a clipboard another program held open
        // keeps its old content, which must still be read and offered later.
        let after = clipboard_generation();
        if after != before {
            self.read_generation = after;
        }
    }

    /// Offers this computer's clipboard to the remote side, unless it is what was
    /// last offered or last received (R1-R4). Returns whether an offer was queued.
    ///
    /// Called when the pane takes the keyboard, when the window becomes active again
    /// with the pane focused, when the session goes live with the pane focused, and
    /// on the paste shortcut, so a remote program pastes what this computer holds
    /// whether it pastes by key or from its own menu.
    ///
    /// Nothing is read from the clipboard while sharing is off or the session is not
    /// live: a session still connecting - or waiting on a certificate answer - cannot
    /// take an offer, and one queued then would only come back as a send failure on
    /// the card the user is waiting on. Going live offers it instead.
    fn share_clipboard(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.target.clipboard()
            || self.link.is_none()
            || !matches!(self.phase, DesktopPhase::Live)
        {
            return false;
        }
        self.offer_when_live = false;
        let generation = clipboard_generation();
        if generation.is_some() && generation == self.read_generation {
            return false;
        }
        // The number is kept only once the read succeeded: a clipboard another
        // program holds open reads as nothing, and remembering its number then would
        // skip that content for good instead of reading it on the next try.
        let Some(item) = cx.read_from_clipboard() else {
            return false;
        };
        self.read_generation = generation;
        let prints = desktop_clipboard::prints(&item);
        if prints.is_empty() || desktop_clipboard::already_shared(&prints, &self.shared) {
            return false;
        }
        let offer = desktop_clipboard::local_offer(&item);
        self.shared = prints;
        if offer.image_too_large {
            self.note(Notice::ClipboardTooLarge(ClipboardKind::Png));
        }
        if offer.entries.is_empty() {
            return false;
        }
        self.input.push_clipboard(offer.entries);
        true
    }

    /// Shows a notice the first time this session reports it.
    ///
    /// The clipboard becoming ready withdraws the refusal said before it: the server
    /// was still signing in, and the sentence no longer holds.
    fn note(&mut self, notice: Notice) {
        if notice == Notice::ClipboardReady {
            self.notices
                .retain(|(kind, _)| *kind != DesktopNotice::ClipboardRefused);
            return;
        }
        let Some(kind) = DesktopNotice::of(notice) else {
            return;
        };
        if self.noticed.contains(&kind) {
            return;
        }
        self.noticed.push(kind);
        let text = notice_text(notice, self.locale.strings());
        self.notices.push((kind, text));
    }

    /// The notices on screen, for a test or a capture.
    #[cfg(any(test, feature = "ui-capture"))]
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn notices(&self) -> Vec<DesktopNotice> {
        self.notices.iter().map(|(kind, _)| *kind).collect()
    }

    /// Puts the notices away. Each still counts as shown for this session.
    pub fn dismiss_notices(&mut self, cx: &mut Context<Self>) {
        if !self.notices.is_empty() {
            self.notices.clear();
            cx.notify();
        }
    }

    fn apply_state(
        &mut self,
        state: LinkState,
        failure: LinkFailure,
        reason: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        match state {
            LinkState::Idle | LinkState::Connecting => {
                if !matches!(self.phase, DesktopPhase::Live) {
                    self.phase = DesktopPhase::Connecting;
                }
                true
            }
            LinkState::Connected => {
                // The protocol accepted the session. A picture may still be on its
                // way, which is a step of its own and the last one before the
                // session is live: the desktop is being built. The pane is not live
                // yet - a session with nothing drawn is not something a user can
                // use, and the record it feeds says the same - so this is a step
                // and not the end of connecting.
                if !matches!(self.phase, DesktopPhase::Live) {
                    self.stage = DesktopStage::Establishing;
                }
                true
            }
            LinkState::Closing | LinkState::Closed => {
                if !matches!(self.phase, DesktopPhase::Ended(_)) {
                    self.fail(LinkFailure::RemoteClosed, "", cx);
                }
                false
            }
            LinkState::Failed(reported) => {
                let failure = if reported == LinkFailure::None {
                    failure
                } else {
                    reported
                };
                self.fail(failure, reason, cx);
                false
            }
        }
    }

    /// Whether the server's certificate question is waiting on screen, for a capture.
    #[cfg(feature = "ui-capture")]
    #[must_use]
    pub fn certificate_asked(&self) -> bool {
        self.certificate.is_some()
    }

    /// Answers the certificate question on screen.
    ///
    /// The link is answered before anything else happens, with the request id the
    /// question carries: a refusal that only stopped this pane would leave the
    /// server waiting out its own timeout with the worker and its socket still
    /// held. Accepting starts the sign-in, which is the next visible step; refusing
    /// ends the session, and ending it shuts the link down.
    pub fn answer_certificate(&mut self, accept: bool, cx: &mut Context<Self>) {
        let Some(question) = self.certificate.take() else {
            return;
        };
        let answer = self
            .link
            .as_ref()
            .map(|link| link.answer_certificate(question.request_id, accept));
        if let Some(Err(error)) = answer {
            self.layer_reason = Some(error.reason());
            if accept {
                // A certificate the server never hears accepted is a connection
                // that cannot go on, so the session ends with the layer's reason
                // instead of waiting for a picture that cannot arrive.
                self.fail(LinkFailure::CertificateRejected, &error.reason(), cx);
                return;
            }
            // A refusal that could not be delivered still ends the session; the
            // shutdown in fail() is what releases the connection.
        }
        if accept {
            self.stage = DesktopStage::Authenticating;
            cx.notify();
            return;
        }
        self.fail(LinkFailure::CertificateRejected, "", cx);
    }

    /// The picture for the current canvas, rebuilt once per batch.
    fn picture(&mut self) -> Option<Arc<RenderImage>> {
        let (width, height) = self.surface.size();
        if self.surface.is_empty() {
            self.image = None;
            return None;
        }
        let changed = self.surface.take_dirty();
        if !changed && self.image.is_some() && self.image_size == (width, height) {
            return self.image.clone();
        }
        let Some(buffer) =
            image::ImageBuffer::from_raw(width, height, self.surface.pixels().to_vec())
        else {
            return self.image.clone();
        };
        let frame = image::Frame::new(buffer);
        let picture = Arc::new(RenderImage::new(vec![frame]));
        self.image = Some(picture.clone());
        self.image_size = (width, height);
        Some(picture)
    }

    // ---------------------------------------------------------------- input

    fn flush_input(&mut self) {
        let Some(link) = self.link.as_ref() else {
            return;
        };
        match self.input.flush(link.as_ref()) {
            // Only a send that really carried something clears the warning: a
            // flush with nothing to send - the poll does one on every batch -
            // would otherwise wipe the sentence one frame after it was drawn, and
            // the user would never learn that their clipboard text went nowhere.
            Ok(sent) => {
                if sent > 0 {
                    self.send_problem = None;
                }
            }
            Err(error) => self.send_problem = Some(error.reason()),
        }
    }

    /// Puts the send warning on the surface, for a test or a capture.
    ///
    /// The real path sets it when a send the user asked for fails, and clears it only
    /// when a later send really carried something. This writes the same field so a
    /// shot of the strip is the strip the failure draws, not a copy of it.
    #[cfg(any(test, feature = "ui-capture"))]
    pub fn show_send_problem(&mut self, reason: &str, cx: &mut Context<Self>) {
        self.send_problem = Some(reason.to_owned());
        cx.notify();
    }

    /// Why the last send failed, when the warning is up, for a test.
    #[cfg(test)]
    #[must_use]
    pub fn send_problem(&self) -> Option<&str> {
        self.send_problem.as_deref()
    }

    /// Releases everything the remote side may still think is held (R17).
    ///
    /// Called when the picture loses the keyboard, when the window is not the
    /// active one and when the session is closed. The releases go through the same
    /// queue as everything else, so the order the user produced is kept.
    pub fn release_input(&mut self) {
        if self.input.release_all() > 0 {
            self.flush_input();
        }
        self.last_sent = None;
    }

    /// The remote pixel under a window position, through the fit the picture is
    /// drawn with at the current zoom and pan.
    ///
    /// The same inverse serves a full screen and a zoomed picture: the viewport is
    /// measured from the last painted frame, so entering or leaving the full screen
    /// changes the numbers the pointer is mapped through and nothing else.
    fn remote_point(&self, position: gpui::Point<Pixels>) -> Option<(u16, u16)> {
        to_remote_in(
            self.viewport?,
            self.surface.size(),
            self.zoom,
            self.pan,
            position,
        )
    }

    fn on_mouse_down(
        &mut self,
        button: MouseButton,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        let Some((x, y)) = self.remote_point(event.position) else {
            return;
        };
        let Some(bit) = pointer_button(button) else {
            return;
        };
        let buttons = self.input.held_buttons().with(bit);
        self.input.push_pointer(x, y, buttons);
        self.last_sent = Some((x, y));
        self.flush_input();
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some((x, y)) = self.remote_point(event.position) else {
            return;
        };
        if self.last_sent == Some((x, y)) {
            return;
        }
        let buttons = self.input.held_buttons();
        self.input.push_pointer(x, y, buttons);
        self.last_sent = Some((x, y));
        self.flush_input();
        cx.notify();
    }

    fn on_mouse_up(&mut self, button: MouseButton, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some(bit) = pointer_button(button) else {
            return;
        };
        let buttons = self.input.held_buttons().without(bit);
        let position = self
            .remote_point(event.position)
            .or(self.last_sent)
            .unwrap_or((0, 0));
        self.input.push_pointer(position.0, position.1, buttons);
        self.flush_input();
        cx.notify();
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let (dx, dy) = match event.delta {
            ScrollDelta::Lines(delta) => (delta.x, delta.y),
            ScrollDelta::Pixels(delta) => (f32::from(delta.x) / 40.0, f32::from(delta.y) / 40.0),
        };
        if !self.zoom.is_fit() && event.modifiers.shift {
            // A picture larger than the pane is moved by the wheel the user holds
            // Shift for, so the plain wheel keeps going to the remote application
            // as it always did (R18).
            let scale = 40.0;
            self.pan_by(-dx * scale, -dy * scale, cx);
            return;
        }
        // The pointer a wheel step is sent at is the one the event was caused at,
        // which GPUI reports in the event rather than in the scroll delta.
        let Some((x, y)) = self.remote_point(event.position) else {
            return;
        };
        let steps = dy.abs().max(dx.abs()).round().clamp(-10.0, 10.0) as i32;
        let step = if dy.abs() >= dx.abs() {
            if dy < 0.0 {
                Buttons::WHEEL_UP
            } else {
                Buttons::WHEEL_DOWN
            }
        } else if dx < 0.0 {
            Buttons::WHEEL_UP
        } else {
            Buttons::WHEEL_DOWN
        };
        for _ in 0..steps.abs() {
            self.input.push_wheel(x, y, step);
        }
        if steps == 0 {
            return;
        }
        self.input.push_pointer(x, y, self.input.held_buttons());
        self.flush_input();
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        // A session that ended sends no keys, so Tab moves on to the banner above the
        // pane and its commands are reached without the pointer, as from a terminal
        // (ADR-6). A live session keeps Tab: it is the remote side's key.
        let modifiers = &keystroke.modifiers;
        if keystroke.key == "tab"
            && !modifiers.control
            && !modifiers.alt
            && !modifiers.platform
            && !modifiers.shift
            && matches!(self.phase, DesktopPhase::Ended(_))
            && let Some(focus) = self.banner_focus.borrow().clone()
        {
            window.focus(&focus);
            cx.stop_propagation();
            return;
        }
        // The pane hands every key to the remote side, so the two controls that
        // belong to the pane itself are taken here, before the key table can turn
        // them into remote keys: F11 for the whole window (R18) and Alt with the
        // zoom keys (R18). Shift is the way past them - Shift+F11 is the remote
        // application's own F11, which is what makes the local control a
        // convenience rather than a lost key.
        if !keystroke.modifiers.control && !keystroke.modifiers.platform {
            match keystroke.key.as_str() {
                "f11" if !keystroke.modifiers.alt && !keystroke.modifiers.shift => {
                    self.toggle_fullscreen(window, cx);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "+" | "=" if keystroke.modifiers.alt => {
                    self.zoom_in(cx);
                    cx.stop_propagation();
                    return;
                }
                "-" if keystroke.modifiers.alt => {
                    self.zoom_out(cx);
                    cx.stop_propagation();
                    return;
                }
                "0" if keystroke.modifiers.alt => {
                    self.zoom_fit(cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        if keystroke.modifiers.platform && !cfg!(target_os = "macos") {
            // The Windows or Super key is not forwarded (see remote_modifiers), so a
            // key pressed with it would reach the remote side as its bare letter -
            // Super+V would type a v. It is this computer's shortcut, not a key.
            return;
        }
        let paste = keystroke.key == "v"
            && shortcut_modifier_held(&keystroke.modifiers)
            && !keystroke.modifiers.alt;
        if paste {
            // Paste is this computer's clipboard going to the remote one first, and
            // then the shortcut itself, so the remote program really pastes (R4).
            // The offer is queued ahead of the keys and leaves in that order. With
            // sharing off, or nothing new to offer, the shortcut goes alone and the
            // remote program pastes what its own clipboard holds. A backend that
            // cannot carry the offer says so instead of dropping it silently.
            self.share_clipboard(cx);
        }
        let Some(codes) = key_codes(&keystroke.key) else {
            // A key this build does not translate is not sent as something else.
            return;
        };
        // The modifiers the user holds are held on the remote side around the key,
        // so a shortcut arrives as the shortcut and not as its bare letter.
        for (modifier, down) in remote_modifiers(&keystroke.modifiers) {
            self.input.set_modifier(modifier, down);
        }
        self.input.push_key(codes, true);
        self.flush_input();
        cx.stop_propagation();
        cx.notify();
    }

    /// Lets go on the remote side of a modifier the user let go of here.
    ///
    /// Only releases: a modifier pressed alone is not sent, because a press the
    /// remote side reads alone - Alt opens a menu bar - is not what pressing it for
    /// a shortcut of this window meant. It goes down with the next key instead.
    fn on_modifiers_changed(&mut self, event: &ModifiersChangedEvent, cx: &mut Context<Self>) {
        let mut released = false;
        for (modifier, down) in remote_modifiers(&event.modifiers) {
            if !down {
                released |= self.input.set_modifier(modifier, false);
            }
        }
        if released {
            self.flush_input();
            cx.notify();
        }
    }

    fn on_key_up(&mut self, event: &KeyUpEvent, cx: &mut Context<Self>) {
        let Some(codes) = key_codes(&event.keystroke.key) else {
            return;
        };
        self.input.push_key(codes, false);
        self.flush_input();
        cx.stop_propagation();
        cx.notify();
    }

    // ---------------------------------------------------------------- drawing

    /// What the pane says it is doing, in the step it is on (R19).
    ///
    /// Every step of a connection the protocol reports has its own sentence, so a
    /// server that is waiting for a certificate decision and one that is deciding
    /// on a password do not look the same, and a failure names the step it
    /// stopped on instead of leaving the user to guess where it went wrong.
    fn status_card(&self, metrics: Metrics) -> gpui::Div {
        let strings = self.locale.strings();
        let (tone, headline, detail) = match &self.phase {
            DesktopPhase::NotStarted => (
                Tone::Neutral,
                format!("{} {}", self.protocol_label(), self.target.name()),
                self.availability.report().to_owned(),
            ),
            DesktopPhase::Connecting => (
                Tone::Neutral,
                self.stage.headline(strings).to_owned(),
                self.target.target(),
            ),
            DesktopPhase::Live => (
                Tone::Positive,
                strings.desktop_live.to_owned(),
                encryption_note(&self.target).to_owned(),
            ),
            DesktopPhase::Ended(failure) => (
                Tone::Danger,
                strings.desktop_ended.to_owned(),
                match (self.failed_stage, self.layer_reason()) {
                    // The step it stopped on, the fact the record states and the
                    // layer's own sentence, in that order.
                    (Some(stage), Some(reason)) => {
                        format!("{} - {} - {reason}", stage.headline(strings), failure)
                    }
                    (Some(stage), None) => format!("{} - {failure}", stage.headline(strings)),
                    (None, Some(reason)) => format!("{failure} - {reason}"),
                    (None, None) => failure.to_string(),
                },
            ),
        };
        // The status card a terminal's place shows (ADR-7): the place the picture will
        // be is waiting, live or ended, and says so the way a session without a
        // terminal does. While the connection is being made the emblem is the loading
        // mark.
        let emblem = match self.phase {
            DesktopPhase::Connecting => components::loading_indicator(metrics),
            _ => icons::icon(Icon::Monitor, metrics.icon_size(), tone.text()).into_any_element(),
        };
        let mut card = components::status_card(
            tone,
            emblem,
            div()
                .debug_selector(|| "desktop.status.headline".to_owned())
                .child(SharedString::from(headline)),
            (!detail.is_empty()).then(|| {
                div()
                    .debug_selector(|| "desktop.status.detail".to_owned())
                    .child(SharedString::from(detail))
            }),
            metrics,
        )
        .debug_selector(|| "desktop.status".to_owned());
        let warnings = self
            .send_problem
            .iter()
            .chain(self.notices.iter().map(|(_, text)| text));
        // A warning is a line of the card in its tone, with the tone's mark, wrapping
        // inside the card.
        for problem in warnings {
            card = card.child(
                div()
                    .flex()
                    .items_start()
                    .gap(metrics.scale(spacing::TIGHT))
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .h(metrics.label_size() * 1.4)
                            .child(components::status_mark(
                                Tone::Warning,
                                metrics.scale(spacing::NARROW),
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_normal()
                            .text_size(metrics.label_size())
                            .text_color(Tone::Warning.text())
                            .child(SharedString::from(problem.clone())),
                    ),
            );
        }
        // Centred and capped by the shared rule, so it is measured at the width it is
        // painted at (ADR-2).
        components::status_card_slot(card, metrics).px(metrics.scale(spacing::LOOSE))
    }

    /// The strip that says a send did not reach the remote side (R18).
    ///
    /// It is a layer over the picture rather than a line in the status card,
    /// because the card is only drawn while there is no picture: the moment input
    /// starts failing is exactly the moment the session is live, and a sentence
    /// nobody can see is the same as swallowing the text. Nothing here is cleared
    /// by the next frame - only by a later send that really carried something -
    /// so a lost paste stays on screen long enough to be read.
    ///
    /// The session's notices (R1-R5) share the strip: a refused clipboard or a folder
    /// that was not mapped is said here once, under the send warning when there is
    /// one, and stays until it is dismissed.
    fn notice_strip(&self, metrics: Metrics, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if self.send_problem.is_none() && self.notices.is_empty() {
            return None;
        }
        let line = |text: &str| {
            div()
                .whitespace_normal()
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .child(SharedString::from(text.to_owned()))
        };
        let mut lines = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE));
        if let Some(problem) = &self.send_problem {
            lines = lines.child(line(problem));
        }
        for (kind, text) in &self.notices {
            let selector = kind.selector();
            lines = lines.child(line(text).debug_selector(move || selector.to_owned()));
        }
        let dismiss = (!self.notices.is_empty()).then(|| {
            components::compact_button(
                self.locale.strings().desktop_notice_dismiss,
                Variant::Ghost,
                metrics,
            )
            .flex_none()
            .id(SharedString::from("desktop.notice.dismiss"))
            .debug_selector(|| "desktop.notice.dismiss".to_owned())
            .on_click(cx.listener(|this, _event, _window, cx| this.dismiss_notices(cx)))
        });
        // The warning banner, lifted onto the picture: it floats over a remote
        // screen whose colours are not ours, so it carries the floating layer's
        // shadow to stay apart from it (R6). It stands at the picture's foot, so the
        // zoom controls at its head stay whole in a narrow window (ADR-7).
        Some(
            components::banner(Tone::Warning, metrics)
                .absolute()
                .bottom_0()
                .left_0()
                .w(gpui::Length::Auto)
                .m(metrics.scale(spacing::TIGHT))
                .max_w(metrics.scale(NOTICE_WIDTH))
                .flex_row()
                .items_center()
                .gap(metrics.scale(spacing::TIGHT))
                .elevation(theme::Elevation::Overlay)
                .occlude()
                .debug_selector(|| "desktop.notice".to_owned())
                // A press on the strip belongs to it: the picture underneath is not
                // clicked at the same time.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_this, _event, _window, cx| cx.stop_propagation()),
                )
                .child(icons::icon(
                    Icon::Monitor,
                    metrics.small_icon_size(),
                    Tone::Warning.text(),
                ))
                .child(lines)
                .children(dismiss),
        )
    }

    /// The controls the pane owns: the whole window and the zoom steps (R18).
    ///
    /// A layer in the top right corner of the picture. Each control carries a
    /// selector of its own so the window-level tests can press the same button the
    /// user does instead of calling the method behind it.
    fn controls(&self, metrics: Metrics, cx: &mut Context<Self>) -> gpui::Div {
        let strings = self.locale.strings();
        let zoom = match self.zoom.percent() {
            Some(percent) => format!("{percent}%"),
            None => strings.desktop_zoom_fit.to_owned(),
        };
        let fullscreen = if self.fullscreen {
            strings.desktop_fullscreen_exit
        } else {
            strings.desktop_fullscreen_enter
        };
        // The shared floating bar with the toolbar's compact buttons: the controls float
        // over a picture that is not ours, like a menu floats over the page (R6).
        components::floating_bar(metrics)
            .absolute()
            .top_0()
            .right_0()
            .m(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "desktop.controls".to_owned())
            // A press on a control belongs to the control: the picture underneath
            // is not clicked at the same time.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _event, _window, cx| cx.stop_propagation()),
            )
            .child(
                components::compact_button(strings.desktop_zoom_out, Variant::Ghost, metrics)
                    .id(SharedString::from("desktop.zoom.out"))
                    .debug_selector(|| "desktop.zoom.out".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| this.zoom_out(cx))),
            )
            .child(
                div()
                    .flex_none()
                    .min_w(metrics.scale(ZOOM_LABEL_WIDTH))
                    .px(metrics.scale(spacing::HAIRLINE))
                    .text_center()
                    .text_size(metrics.caption_size())
                    .font_weight(metrics.medium_weight())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .debug_selector(|| "desktop.zoom.label".to_owned())
                    .child(SharedString::from(zoom)),
            )
            .child(
                components::compact_button(strings.desktop_zoom_in, Variant::Ghost, metrics)
                    .id(SharedString::from("desktop.zoom.in"))
                    .debug_selector(|| "desktop.zoom.in".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| this.zoom_in(cx))),
            )
            .child(components::divider_vertical(metrics).mx(metrics.scale(spacing::HAIRLINE)))
            .child(
                components::icon_label_button(
                    if self.fullscreen {
                        Icon::Restore
                    } else {
                        Icon::Maximize
                    },
                    fullscreen,
                    Variant::Ghost,
                    metrics,
                )
                .h(metrics.compact_control_height())
                .px(metrics.scale(spacing::TIGHT))
                .text_size(metrics.label_size())
                .id(SharedString::from("desktop.fullscreen"))
                .debug_selector(|| "desktop.fullscreen".to_owned())
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.toggle_fullscreen(window, cx);
                })),
            )
    }

    fn certificate_card(&self, metrics: Metrics, cx: &mut Context<Self>) -> gpui::Div {
        let strings = self.locale.strings();
        let Some(question) = &self.certificate else {
            return div();
        };
        let fingerprint = question.fingerprint.clone();
        // A question about the server's identity: the same status card the picture's
        // place shows, in the warning tone, with the answers at its foot (ADR-7).
        components::status_card(
            Tone::Warning,
            icons::icon(Icon::Lock, metrics.icon_size(), Tone::Warning.text()).into_any_element(),
            div().child(SharedString::from(question.summary())),
            // The fingerprint is what the decision is made on, so it is drawn whole -
            // wrapping, never cut to an ellipsis - in the monospace face, one click from
            // the clipboard.
            Some(
                components::copy_on_click(
                    div()
                        .min_w_0()
                        .font_family(crate::app::terminal::monospace_family())
                        .debug_selector(|| "desktop.certificate.fingerprint".to_owned()),
                    fingerprint.clone(),
                )
                .child(SharedString::from(fingerprint)),
            ),
            metrics,
        )
        .debug_selector(|| "desktop.certificate".to_owned())
        .child(
            div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap(metrics.scale(spacing::TIGHT))
                .child(
                    components::compact_button(
                        strings.desktop_certificate_reject,
                        Variant::Danger,
                        metrics,
                    )
                    .id(SharedString::from("desktop.certificate.reject"))
                    .debug_selector(|| "desktop.certificate.reject".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.answer_certificate(false, cx);
                    })),
                )
                .child(
                    components::compact_button(
                        strings.desktop_certificate_trust,
                        Variant::Primary,
                        metrics,
                    )
                    .id(SharedString::from("desktop.certificate.trust"))
                    .debug_selector(|| "desktop.certificate.trust".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.answer_certificate(true, cx);
                    })),
                ),
        )
    }
}

/// The pointer bit a GPUI button maps to, or None for a button the protocol does
/// not carry.
fn pointer_button(button: MouseButton) -> Option<Buttons> {
    match button {
        MouseButton::Left => Some(Buttons::LEFT),
        MouseButton::Middle => Some(Buttons::MIDDLE),
        MouseButton::Right => Some(Buttons::RIGHT),
        MouseButton::Navigate(_) => None,
    }
}

impl Render for DesktopPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = Metrics::default();
        if !self.watching_focus {
            // A picture that lost the keyboard may be showing a modifier the remote
            // side still thinks is held; letting go of everything is the only
            // honest thing to do before the keys go elsewhere (R17).
            self.watching_focus = true;
            let this = cx.entity().downgrade();
            window
                .on_focus_out(&self.focus, cx, move |_, _window, cx| {
                    let _ = this.update(cx, |pane, cx| {
                        pane.release_input();
                        cx.notify();
                    });
                })
                .detach();
            // Taking the keyboard, and the window becoming active again with the
            // picture focused, are the moments the user may be about to paste in a
            // remote program - by key or from its own menu - so this computer's
            // clipboard is offered then (R1-R4).
            let focused = cx.on_focus_in(&self.focus, window, |pane, _window, cx| {
                if pane.share_clipboard(cx) {
                    pane.flush_input();
                    cx.notify();
                }
            });
            let activated = cx.observe_window_activation(window, |pane, window, cx| {
                if window.is_window_active()
                    && pane.focus.is_focused(window)
                    && pane.share_clipboard(cx)
                {
                    pane.flush_input();
                    cx.notify();
                }
            });
            self.subscriptions.push(focused);
            self.subscriptions.push(activated);
        }
        // A hidden or minimised window sends no input anywhere, so the moment it
        // stops being the active one everything it may have held is let go (R17).
        // The flag makes that one transition, not one release per frame.
        let active = window.is_window_active();
        if self.was_active && !active {
            self.release_input();
        }
        self.was_active = active;
        // The session went live - a first connection or a reconnect - while the
        // picture held the keyboard, which no focus event reports: this computer's
        // clipboard is offered now, so a paste from the remote program's own menu
        // right away pastes it.
        if self.offer_when_live
            && active
            && matches!(self.phase, DesktopPhase::Live)
            && self.focus.is_focused(window)
            && self.share_clipboard(cx)
        {
            self.flush_input();
        }
        let picture = self.picture();
        // Whether the picture is what the body shows. The status card below stands in
        // for it while it is absent, and the warning strip belongs over the picture
        // only: drawing both would say the same thing twice before the first frame.
        let has_picture = picture.is_some();
        let entity = cx.entity().downgrade();

        let mut body = div()
            .debug_selector(|| "desktop.body".to_owned())
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .bg(theme::swatch(palette::CANVAS))
            .key_context(crate::app::TERMINAL_KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_children_prepainted(move |bounds, _window, app| {
                if let Some(bounds) = bounds.first().copied() {
                    let _ = entity.update(app, |pane, cx| {
                        pane.set_viewport(bounds, cx);
                    });
                }
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.on_mouse_down(MouseButton::Left, event, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.on_mouse_down(MouseButton::Middle, event, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.on_mouse_down(MouseButton::Right, event, window, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                this.on_mouse_move(event, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _window, cx| {
                    this.on_mouse_up(MouseButton::Left, event, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseUpEvent, _window, cx| {
                    this.on_mouse_up(MouseButton::Middle, event, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, event: &MouseUpEvent, _window, cx| {
                    this.on_mouse_up(MouseButton::Right, event, cx);
                }),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                this.on_scroll(event, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_key_down(event, window, cx);
            }))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _window, cx| {
                this.on_key_up(event, cx);
            }))
            .on_modifiers_changed(cx.listener(
                |this, event: &ModifiersChangedEvent, _window, cx| {
                    this.on_modifiers_changed(event, cx);
                },
            ));

        match picture {
            Some(picture) => {
                // The image is drawn with the fit its own inverse expects, so the
                // pixel under the pointer is the pixel that was drawn there (R18).
                // At the Fit step that fit is GPUI's own Contain over the whole
                // pane; at a zoom step the picture is drawn at the size the
                // geometry says and moved by the pan, and the pane crops it.
                let surface = match self.zoom {
                    DesktopZoom::Fit => div()
                        .flex_1()
                        .min_h_0()
                        .relative()
                        .debug_selector(|| "desktop.surface".to_owned())
                        .child(
                            img(picture)
                                .size_full()
                                .object_fit(gpui::ObjectFit::Contain),
                        ),
                    DesktopZoom::Percent(percent) => {
                        let (width, height) = self.surface.size();
                        let factor = f32::from(percent) / 100.0;
                        let (width, height) = (width as f32 * factor, height as f32 * factor);
                        let (pan_x, pan_y) = self.pan;
                        div()
                            .flex_1()
                            .min_h_0()
                            .relative()
                            .overflow_hidden()
                            .debug_selector(|| "desktop.surface".to_owned())
                            .child(
                                // Centred, then moved by the pan: the same two
                                // numbers content_rect_in gives the pointer, so a
                                // click lands on the pixel it is over.
                                div()
                                    .absolute()
                                    .inset_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        div()
                                            .ml(px(pan_x))
                                            .mt(px(pan_y))
                                            .w(px(width))
                                            .h(px(height))
                                            .debug_selector(|| "desktop.picture".to_owned())
                                            .child(img(picture).size_full()),
                                    ),
                            )
                    }
                };
                body = body.child(surface);
            }
            None => {
                // A link that comes up in under the floor shows its picture instead of
                // a card that flashes on the way; the pane keeps its space either way.
                let early = matches!(self.phase, DesktopPhase::Connecting)
                    && self
                        .connecting_since
                        .is_some_and(|since| since.elapsed() < crate::app::CONNECT_STATE_FLOOR);
                body = body.child(if early {
                    div().flex_1()
                } else {
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(self.status_card(metrics))
                });
            }
        }
        // The controls belong to the pane, not to the session: they are drawn over
        // the picture whenever there is a link to control (R18).
        if !matches!(self.phase, DesktopPhase::NotStarted) {
            body = body.child(self.controls(metrics, cx));
        }
        if has_picture && let Some(notice) = self.notice_strip(metrics, cx) {
            body = body.child(notice);
        }
        if self.certificate.is_some() {
            let card = self.certificate_card(metrics, cx);
            body = body.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(metrics.scale(spacing::LOOSE))
                    .child(components::status_card_slot(card, metrics)),
            );
        }
        if self.surface.is_empty()
            && let DesktopPhase::Live = self.phase
        {
            // A live session with no picture yet is not a black screen without
            // explanation: the size is what it is waiting for.
            body = body.child(
                div()
                    .absolute()
                    .bottom(px(0.0))
                    .left(px(0.0))
                    .p(metrics.scale(spacing::TIGHT))
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(SharedString::from(self.target.target())),
            );
        }
        body
    }
}

impl DesktopPane {
    /// Keeps the space the picture is drawn in, and renegotiates the remote size
    /// when it changed.
    fn set_viewport(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        let previous = self.viewport;
        self.viewport = Some(bounds);
        if previous == Some(bounds) {
            return;
        }
        // The remote desktop is resized to the space it has, in the same fit the
        // picture is drawn with: the desktop keeps its aspect ratio, so the size is
        // what the drawn rectangle can hold.
        let Some(link) = self.link.as_ref() else {
            return;
        };
        if !matches!(self.phase, DesktopPhase::Live | DesktopPhase::Connecting) {
            return;
        }
        let size = self.surface.size();
        let width = if size.0 == 0 {
            f32::from(bounds.size.width).max(1.0) as u32
        } else {
            f32::from(content_rect(bounds, size).size.width).max(1.0) as u32
        };
        let height = if size.1 == 0 {
            f32::from(bounds.size.height).max(1.0) as u32
        } else {
            f32::from(content_rect(bounds, size).size.height).max(1.0) as u32
        };
        if (width, height) == self.asked_size {
            return;
        }
        // One ask per change: a drag that resizes the window every frame would
        // otherwise renegotiate the session on every frame. A link that cannot
        // renegotiate a live session (the native one, which takes its size before it
        // dials) says so, and the picture is then scaled into the new space by the
        // fit the pointer is mapped through.
        if link.resize(width, height).is_ok() {
            self.asked_size = (width, height);
        }
        let _ = cx;
    }
}
