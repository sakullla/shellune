//! The desktop pane: the window's side of an RDP or VNC session (R8, R10,
//! R17-R20, R23).
//!
//! The pane owns no protocol logic. It draws one BGRA canvas with the fit GPUI
//! itself uses, maps the pointer through the inverse of that same fit, queues what
//! the user does and hands the queue to the link in order. Everything with a rule
//! worth testing lives in [crate::app::desktop]; what lives here is the window:
//! layout, focus, key and pointer events, the status card and the certificate
//! question.

use std::{rc::Rc, sync::Arc};

use gpui::prelude::*;
use gpui::{
    Bounds, Context, CursorStyle, FocusHandle, KeyDownEvent, KeyUpEvent, Modifiers,
    ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Render, RenderImage, ScrollDelta, ScrollWheelEvent, SharedString, Subscription, Window, div,
    img, px,
};
use shellune_core::SessionFailure;
use shellune_desktop::PointerShape;

use crate::app::components::{self, Variant};
use crate::app::copy::{Locale, Strings};
use crate::app::desktop::{
    ALT_LEFT, Availability, Buttons, CONTROL_LEFT, ClipboardEntry, ClipboardKind,
    DESKTOP_POLL_INTERVAL, DesktopLink, DesktopSurface, DesktopTarget, DesktopZoom, DriveProblem,
    InputQueue, KeyCodes, LinkEvent, LinkFailure, LinkState, NativeLink, Notice, SHIFT_LEFT,
    availability, clamp_pan, content_rect_in, encryption_note, key_codes, to_remote_in,
};
use crate::app::desktop_clipboard;
use crate::app::icons::{self, Icon};
use crate::app::text_field::TextField;
use crate::app::theme::{self, Elevated, Metrics, Tone, palette, spacing};

#[cfg(all(target_os = "macos", not(test)))]
mod mac_hotkeys {
    use std::{ffi::c_void, ptr::NonNull};
    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn PushSymbolicHotKeyMode(options: u32) -> *mut c_void;
        fn PopSymbolicHotKeyMode(token: *mut c_void);
    }

    /// The Carbon mode stack is main-thread-only. The pane owns its token
    /// while it has keyboard focus and Carbon limits it to the foreground app.
    pub(super) struct Capture(NonNull<c_void>);
    impl Capture {
        pub(super) fn acquire() -> Option<Self> {
            // kHIHotKeyModeAllDisabledExceptUniversalAccess.
            NonNull::new(unsafe { PushSymbolicHotKeyMode(2) }).map(Self)
        }
    }
    impl Drop for Capture {
        fn drop(&mut self) {
            unsafe { PopSymbolicHotKeyMode(self.0.as_ptr()) };
        }
    }
}

/// The size a session asks for before the window has measured anything.
///
/// A remote desktop negotiates its size at connect time, so the first ask is the
/// size of the window this build opens with; the pane renegotiates as soon as it
/// knows the space it really has.
pub const FIRST_DESKTOP_SIZE: (u32, u32) = (1280, 720);

/// The widest the notice strip over a picture grows, before text scale.
const NOTICE_WIDTH: f32 = 420.0;

/// The room the zoom step's words take between the two zoom buttons, before text
/// scale: "100%" and "Fit" / "适应窗口" in either language, so the buttons do not move.
const ZOOM_LABEL_WIDTH: f32 = 72.0;

/// Whether a certificate question is still waiting for an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CertificateQuestion {
    request_id: u32,
    host: String,
    subject: String,
    issuer: String,
    fingerprint: String,
}

struct SignIn {
    cancel_focus: FocusHandle,
    connect_focus: FocusHandle,
    user: Option<gpui::Entity<TextField>>,
    password: gpui::Entity<TextField>,
    focus_pending: bool,
    rejection: Option<String>,
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

/// One remote desktop on screen.
pub struct DesktopPane {
    target: DesktopTarget,
    availability: Availability,
    focus: FocusHandle,
    link: Option<Box<dyn DesktopLink>>,
    surface: DesktopSurface,
    pointer_shape: Option<PointerShape>,
    pointer_image: Option<Arc<RenderImage>>,
    #[cfg(target_os = "macos")]
    native_cursor: Option<(f32, gpui::BitmapCursor)>,
    pointer_dirty: bool,
    #[cfg(test)]
    presented_cursor: CursorStyle,
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
    /// Last elapsed second displayed while waiting for a real connection.
    wait_seconds: Option<u64>,
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
    asked_scale: u32,
    pending_dial: Option<Option<shellune_core::SecretString>>,
    /// The last remote pixel a motion reported, so movement inside one pixel is not
    /// sent twice.
    last_sent: Option<(u16, u16)>,
    /// Fractional wheel steps from a precision touchpad.
    wheel_remainder: [f32; 2],
    certificate: Option<CertificateQuestion>,
    certificate_store: shellune_storage::TrustStore,
    previous_pin: Option<String>,
    remember_certificate: bool,
    pin_error: Option<String>,
    sign_in: Option<SignIn>,
    provided_password: bool,
    certificate_remember_focus: FocusHandle,
    certificate_reject_focus: FocusHandle,
    certificate_trust_focus: FocusHandle,
    /// What the protocol layer said about why it ended, kept verbatim.
    layer_reason: Option<String>,
    /// What the last start was handed to sign in with, for the tests that prove the
    /// password reaches the link. `None` until a start; `Some(None)` is "no password".
    #[cfg(test)]
    started_password: Option<Option<String>>,
    /// Whether the pointer is over the pane, which is when the controls over the
    /// picture are drawn. A bar that is always there covers a remote screen whose
    /// colours are not ours, and the corner it sits in is where a remote window keeps
    /// its own controls.
    controls_hovered: bool,
    /// Whether the pointer is over the bar or its handle, which is drawn above the
    /// picture and occludes it.
    ///
    /// The bar has to block the pointer so a press on a control does not also
    /// reach the remote screen, and an occluding layer is the one place the body
    /// stops being hovered: the pointer moving onto the bar would take the bar
    /// away in the same frame it arrived. Tracking the bar's own hover and
    /// OR-ing it with the body's is what keeps it under the pointer while the
    /// pointer is on it - including the folded handle, which is drawn only while
    /// one of the two says the pointer is here.
    bar_hovered: bool,
    /// Whether the bar over the picture is folded away on purpose.
    ///
    /// The fold is the pane's own memory: another frame, a redraw and a step of the
    /// session keep it, and it is never written down - a new attempt starts with the
    /// bar shown, because the fold was a decision about this screen.
    controls_collapsed: bool,
    /// How many frames drew the bar over the picture, for the test that a folded bar
    /// is not drawn at all. The test window keeps the debug bounds of elements that
    /// are gone, so a frame count is the only honest way to say "not drawn".
    #[cfg(test)]
    controls_frames: u32,
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
    #[cfg(all(target_os = "macos", not(test)))]
    hotkey_capture: Option<mac_hotkeys::Capture>,
    /// Cached desktop tiles in row-major order. A frame replaces its dirty tiles.
    images: Rc<Vec<Option<Arc<RenderImage>>>>,
    image_size: (u32, u32),
    locale: Locale,
    /// The parts of the clipboard content last offered to the remote side or written
    /// here from it, so the same content is not offered again (R1-R3).
    shared: Vec<u64>,
    /// The clipboard's number at the pane's last read of it, so an unchanged
    /// clipboard is not read again (see [clipboard_generation]).
    read_generation: Option<u64>,
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
    pub fn new(
        target: DesktopTarget,
        locale: Locale,
        certificate_store: shellune_storage::TrustStore,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release(|pane: &mut Self, cx| {
            pane.close_link(cx);
            if let Some(image) = pane.pointer_image.take() {
                cx.drop_image(image, None);
            }
            for image in Rc::make_mut(&mut pane.images).drain(..).flatten() {
                cx.drop_image(image, None);
            }
        })
        .detach();
        Self {
            availability: availability(target.protocol()),
            target,
            focus: cx.focus_handle(),
            link: None,
            surface: DesktopSurface::new(),
            pointer_shape: None,
            pointer_image: None,
            #[cfg(target_os = "macos")]
            native_cursor: None,
            pointer_dirty: false,
            #[cfg(test)]
            presented_cursor: CursorStyle::Arrow,
            input: InputQueue::new(),
            phase: DesktopPhase::NotStarted,
            stage: DesktopStage::NotStarted,
            connecting_since: None,
            wait_seconds: None,
            failed_stage: None,
            zoom: DesktopZoom::Fit,
            pan: (0.0, 0.0),
            fullscreen: false,
            viewport: None,
            asked_size: FIRST_DESKTOP_SIZE,
            asked_scale: 100,
            pending_dial: None,
            last_sent: None,
            wheel_remainder: [0.0; 2],
            certificate: None,
            certificate_store,
            previous_pin: None,
            remember_certificate: false,
            pin_error: None,
            sign_in: None,
            provided_password: false,
            certificate_remember_focus: cx.focus_handle(),
            certificate_reject_focus: cx.focus_handle(),
            certificate_trust_focus: cx.focus_handle(),
            layer_reason: None,
            #[cfg(test)]
            started_password: None,
            #[cfg(test)]
            starts: 0,
            // A shot of a session is a shot of its controls too, and the capture runs
            // with no pointer over the picture: there the bar starts shown.
            controls_hovered: cfg!(feature = "ui-capture"),
            bar_hovered: false,
            controls_collapsed: false,
            #[cfg(test)]
            controls_frames: 0,
            send_problem: None,
            polling: false,
            watching_focus: false,
            was_active: false,
            #[cfg(all(target_os = "macos", not(test)))]
            hotkey_capture: None,
            images: Rc::default(),
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
        certificate_store: shellune_storage::TrustStore,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut pane = Self::new(target, locale, certificate_store, cx);
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
        // The next layout supplies the fullscreen dimensions to the live link.
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

    /// Folds the controls over the picture away, or brings them back.
    ///
    /// Folded, the bar is not drawn at all - not even while the pointer is over the
    /// picture - and only a small handle in the same corner brings it back. The state
    /// belongs to the pane and is never written down.
    pub fn set_controls_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        if self.controls_collapsed != collapsed {
            self.controls_collapsed = collapsed;
            // The bar's own hover was recorded while it was drawn; folding it away
            // takes that element out of the tree, and a stale hover would keep the
            // handle on screen after the pointer had left the picture.
            self.bar_hovered = false;
            cx.notify();
        }
    }

    /// How many frames drew the bar over the picture, for the tests.
    #[cfg(test)]
    #[must_use]
    pub fn controls_frames_for_test(&self) -> u32 {
        self.controls_frames
    }

    /// Whether the bar over the picture is folded away, for the tests.
    #[cfg(test)]
    #[must_use]
    pub fn controls_collapsed_for_test(&self) -> bool {
        self.controls_collapsed
    }

    /// Fit and 100% are the two steps the readout names, so pressing it moves
    /// between them and the +/- buttons keep their own ladder.
    ///
    /// A zoom out of the way (50% or 75%) goes back to Fit rather than to 100%: the
    /// readout says what the picture is now, and the next press is the other end.
    pub fn zoom_toggle(&mut self, cx: &mut Context<Self>) {
        let next = if self.zoom.is_fit() {
            DesktopZoom::Percent(100)
        } else {
            DesktopZoom::Fit
        };
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

    /// Whether the controls over the picture are drawn, for the tests that check they
    /// stay out of the way until the pointer is over the pane.
    #[cfg(test)]
    #[must_use]
    pub fn controls_revealed_for_test(&self) -> bool {
        self.controls_hovered
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
            self.provided_password = password.is_some_and(|p| !p.is_empty());
            self.pending_dial = Some(password.map(shellune_core::SecretString::new));
            if self.viewport.is_some() {
                self.dial_pending(cx);
            } else {
                // Usually the first layout starts the dial with the exact pixel size.
                // A background tab may have no layout until it is selected.
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(120))
                        .await;
                    let _ = this.update(cx, |pane, cx| pane.dial_pending(cx));
                })
                .detach();
                cx.notify();
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

    fn dial_pending(&mut self, cx: &mut Context<Self>) {
        let Some(password) = self.pending_dial.take() else {
            return;
        };
        let (width, height) = self.asked_size;
        let pin = match self.certificate_store.desktop_certificate(
            self.protocol_label(),
            self.target.address(),
            self.target.port(),
        ) {
            Ok(pin) => pin,
            Err(error) => {
                self.fail(LinkFailure::Io, &error.to_string(), cx);
                return;
            }
        };
        self.previous_pin = pin;
        let allowed_pin =
            if self.target.certificate() == shellune_core::RdpCertificatePolicy::Reject {
                None
            } else {
                self.previous_pin.as_deref()
            };
        match NativeLink::open(
            &self.target,
            password.as_ref().map(|p| p.expose()),
            allowed_pin,
            self.asked_scale,
            width,
            height,
        ) {
            Ok(link) => self.attach(Box::new(link), cx),
            Err(error) => self.fail(LinkFailure::Connect, &error.reason(), cx),
        }
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
        self.set_pointer(None);
        self.wait_seconds = None;
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
        if !was_live && !matches!(self.stage, DesktopStage::NotStarted) {
            self.failed_stage = Some(self.stage);
        }
        self.certificate = None;
        self.set_pointer(None);
        self.phase = DesktopPhase::Ended(failure.session_failure(
            reason,
            self.target.protocol(),
            &target,
            was_live,
        ));
        self.connecting_since = None;
        self.polling = false;
        // Nothing is held after a session ended: the remote side would otherwise
        // keep a modifier down for a connection that is gone.
        self.release_input();
        self.flush_input();
        self.close_link(cx);
        cx.notify();
    }

    fn close_link(&mut self, cx: &gpui::App) {
        self.pending_dial = None;
        self.sign_in = None;
        if let Some(mut link) = self.link.take() {
            // Wake pending network I/O now. A DNS lookup or TCP connect may still
            // have to finish before the backend can join its worker.
            link.shutdown();
            cx.background_executor()
                .spawn(async move { drop(link) })
                .detach();
        }
    }

    /// Stops the session and releases what it owns. Safe to call twice.
    pub fn stop(&mut self, cx: &gpui::App) {
        self.release_input();
        self.flush_input();
        self.close_link(cx);
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

    /// Whether the surface is showing a live picture.
    pub fn is_live(&self) -> bool {
        matches!(self.phase, DesktopPhase::Live)
    }

    fn reconnecting(&self) -> bool {
        self.phase == DesktopPhase::Connecting && !self.surface.is_empty()
    }

    fn set_pointer(&mut self, shape: Option<PointerShape>) {
        if self.pointer_shape == shape {
            return;
        }
        #[cfg(target_os = "macos")]
        {
            self.native_cursor = None;
        }
        self.pointer_shape = shape;
        self.pointer_dirty = true;
    }

    fn update_pointer_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.pointer_dirty) {
            return;
        }
        if let Some(old) = self.pointer_image.take() {
            cx.drop_image(old, Some(window));
        }
        let shape = match &self.pointer_shape {
            Some(PointerShape::Default)
                if self.target.protocol() == shellune_core::ProtocolKind::Rdp =>
            {
                Some(windows_arrow())
            }
            other => other.as_ref(),
        };
        if let Some(PointerShape::Bitmap {
            width,
            height,
            pixels,
            ..
        }) = shape
        {
            self.pointer_image = Some(Arc::new(RenderImage::from_bgra(
                gpui::size(
                    gpui::DevicePixels(i32::from(*width)),
                    gpui::DevicePixels(i32::from(*height)),
                ),
                pixels.clone(),
            )));
        }
    }

    /// A native precision cursor remains visible until there is a bitmap to paint.
    fn fallback_cursor(&self) -> CursorStyle {
        match (&self.pointer_shape, self.is_live(), self.target.protocol()) {
            (Some(PointerShape::Hidden), true, _) => CursorStyle::None,
            (None, true, shellune_core::ProtocolKind::Vnc) => CursorStyle::Crosshair,
            _ => CursorStyle::Arrow,
        }
    }

    /// What the surface asks the window to show, for the tests.
    #[cfg(test)]
    #[must_use]
    pub fn surface_cursor_for_test(&self) -> CursorStyle {
        self.presented_cursor
    }

    /// Whether the controls over the picture are drawn.
    ///
    /// The pointer may be over the picture itself or over the bar that sits on
    /// it: the bar occludes the body's hitbox, so its own hover has to count, or
    /// the pointer would hide the bar the moment it reached it.
    fn controls_shown(&self) -> bool {
        self.controls_hovered || self.bar_hovered
    }

    /// Whether the bar is drawn, for the tests.
    #[cfg(test)]
    #[must_use]
    pub fn controls_shown_for_test(&self) -> bool {
        self.controls_shown()
    }

    /// Records the pointer arriving on or leaving the bar itself.
    fn set_bar_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if self.bar_hovered != hovered {
            self.bar_hovered = hovered;
            cx.notify();
        }
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
        let had_problem = self.send_problem.is_some();
        if !events.iter().any(|event| {
            matches!(
                event,
                LinkEvent::State {
                    state: LinkState::Connecting,
                    ..
                }
            )
        }) {
            self.flush_input();
        }
        if self.phase == DesktopPhase::Connecting
            && let Some(elapsed) = self.connecting_since.map(|since| since.elapsed())
            && elapsed >= crate::app::CONNECT_STATE_FLOOR
            && self.wait_seconds != Some(elapsed.as_secs())
        {
            self.wait_seconds = Some(elapsed.as_secs());
            cx.notify();
        }
        if events.is_empty() && had_problem != self.send_problem.is_some() {
            cx.notify();
        }
        if events.is_empty() {
            return self.phase.accepts_input() || matches!(self.phase, DesktopPhase::NotStarted);
        }
        let mut keep = true;
        let mut redraw = had_problem != self.send_problem.is_some();
        for event in events {
            redraw |= match &event {
                LinkEvent::Frame(_) => !self.is_live(),
                LinkEvent::Pointer(shape) => self.pointer_shape.as_ref() != Some(shape),
                _ => true,
            };
            keep = self.apply(event, cx) && keep;
        }
        if redraw || self.surface.has_dirty_tiles() {
            cx.notify();
        }
        keep
    }

    fn apply(&mut self, event: LinkEvent, cx: &mut Context<Self>) -> bool {
        match event {
            LinkEvent::Pointer(shape) => {
                self.set_pointer(Some(shape));
                true
            }
            LinkEvent::Frame(region) => {
                if self.reconnecting() {
                    let mut next = DesktopSurface::new();
                    if !next.apply(&region) {
                        return true;
                    }
                    self.surface = next;
                } else if !self.surface.apply(&region) {
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
                self.release_input();
                self.remember_certificate = false;
                self.pin_error = None;
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
        let before = cx.clipboard_generation();
        cx.write_to_clipboard(item);
        // What is on the clipboard now is what was just shared; reading it back
        // would only find that out again. The write reports no failure, so only a
        // number that moved proves it landed: a clipboard another program held open
        // keeps its old content, which must still be read and offered later.
        let after = cx.clipboard_generation();
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
        let generation = cx.clipboard_generation();
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
        let offer = desktop_clipboard::local_offer(&item, self.target.protocol());
        self.shared = prints;
        if offer.image_too_large {
            self.note(Notice::ClipboardTooLarge(ClipboardKind::Dib));
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
        if self.send_problem.is_some() || !self.notices.is_empty() {
            self.send_problem = None;
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
                self.set_pointer(None);
                if self.is_live() {
                    self.asked_size = (0, 0);
                    self.input = InputQueue::new();
                    self.last_sent = None;
                    self.wheel_remainder = [0.0; 2];
                    self.send_problem = None;
                    self.stage = DesktopStage::Connecting;
                    self.connecting_since = Some(std::time::Instant::now());
                    self.wait_seconds = None;
                }
                self.phase = DesktopPhase::Connecting;
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
                if failure == LinkFailure::CredentialsRequired
                    || (failure == LinkFailure::Authentication
                        && self.provided_password
                        && !self.is_live())
                {
                    let rejected =
                        (failure == LinkFailure::Authentication).then(|| reason.to_owned());
                    self.close_link(cx);
                    self.polling = false;
                    self.certificate = None;
                    self.stage = DesktopStage::Authenticating;
                    self.phase = DesktopPhase::Connecting;
                    self.connecting_since = None;
                    let user =
                        (self.target.protocol() == shellune_core::ProtocolKind::Rdp).then(|| {
                            cx.new(|cx| {
                                let mut field = TextField::new("", cx);
                                field.set_text(self.target.user().unwrap_or_default(), cx);
                                field
                            })
                        });
                    let password = cx.new(|cx| {
                        let mut field = TextField::new("", cx);
                        field.set_masked(true, cx);
                        field.set_revealable(true, cx);
                        field
                    });
                    for field in user.iter().chain(std::iter::once(&password)) {
                        cx.observe(field, |_, _, cx| cx.notify()).detach();
                    }
                    self.sign_in = Some(SignIn {
                        cancel_focus: cx.focus_handle(),
                        connect_focus: cx.focus_handle(),
                        user,
                        password,
                        focus_pending: true,
                        rejection: rejected,
                    });
                    cx.notify();
                    return false;
                }
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
        if accept
            && self.remember_certificate
            && let Some(question) = &self.certificate
            && let Err(error) = self.certificate_store.remember_desktop_certificate(
                self.protocol_label(),
                self.target.address(),
                self.target.port(),
                &question.fingerprint,
            )
        {
            self.pin_error = Some(error.to_string());
            cx.notify();
            return;
        }
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

    fn update_picture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.surface.size() != self.image_size {
            for old in Rc::make_mut(&mut self.images).drain(..).flatten() {
                cx.drop_image(old, Some(window));
            }
            self.image_size = self.surface.size();
        }
        let dirty: Vec<_> = self.surface.take_dirty_tiles().collect();
        if dirty.is_empty() {
            return;
        }
        let images = Rc::make_mut(&mut self.images);
        images.resize_with(self.surface.tile_count(), || None);
        for index in dirty {
            if let Some(old) = images[index].take() {
                cx.drop_image(old, Some(window));
            }
            images[index] = self.surface.tile(index).map(|(w, h, bytes)| {
                Arc::new(RenderImage::from_bgra(
                    gpui::size(gpui::DevicePixels(w as i32), gpui::DevicePixels(h as i32)),
                    bytes,
                ))
            });
        }
    }

    /// Lay out the desktop once. Moving the cursor reuses all cached tile images.
    fn picture_tiles(&self) -> impl IntoElement + 'static {
        let images = Rc::clone(&self.images);
        let size = self.image_size;
        gpui::canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                let scale = window.scale_factor();
                let clip = window.content_mask().bounds;
                for (index, image) in images.iter().enumerate() {
                    let Some(image) = image else {
                        continue;
                    };
                    let tile = desktop_tile_bounds(bounds, size, index, scale);
                    if !tile.is_empty()
                        && tile.to_pixels(scale).intersects(&clip)
                        && let Err(error) = window.paint_image_tile(tile, Arc::clone(image))
                    {
                        eprintln!("could not paint desktop tile: {error}");
                    }
                }
            },
        )
        .absolute()
        .inset_0()
    }

    // ---------------------------------------------------------------- input

    fn flush_input(&mut self) {
        if self.reconnecting() {
            return;
        }
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
        #[cfg(all(target_os = "macos", not(test)))]
        {
            self.hotkey_capture = None;
        }
        if self.input.release_all() > 0 {
            self.flush_input();
        }
        self.last_sent = None;
        self.wheel_remainder = [0.0; 2];
    }

    /// The remote pixel under a window position, through the fit the picture is
    /// drawn with at the current zoom and pan.
    ///
    /// The same inverse serves a full screen and a zoomed picture: the viewport is
    /// measured from the last painted frame, so entering or leaving the full screen
    /// changes the numbers the pointer is mapped through and nothing else.
    fn remote_point(&self, position: gpui::Point<Pixels>) -> Option<(u16, u16)> {
        if self.certificate.is_some() || !self.is_live() {
            return None;
        }
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
        // The overlay follows movement at display speed, including movement
        // inside one magnified desktop pixel. The cached desktop is reused.
        #[cfg(target_os = "macos")]
        let software_cursor = self.native_cursor.is_none();
        #[cfg(not(target_os = "macos"))]
        let software_cursor = true;
        if software_cursor && self.pointer_image.is_some() {
            cx.notify();
        }
        let Some((x, y)) = self.remote_point(event.position) else {
            return;
        };
        if self.last_sent != Some((x, y)) {
            self.input.push_pointer(x, y, self.input.held_buttons());
            self.last_sent = Some((x, y));
        }
        // Do not wait for the UI polling timer during a drag. The worker queue
        // coalesces consecutive moves without crossing a press or release.
        if self.input.held_buttons().any_button() {
            self.flush_input();
        }
    }

    fn on_mouse_up(&mut self, button: MouseButton, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if self.certificate.is_some() || !self.is_live() {
            return;
        }
        let Some(bit) = pointer_button(button) else {
            return;
        };
        if !self.input.held_buttons().contains(bit) {
            return;
        }
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
        let Some((x, y)) = self.remote_point(event.position) else {
            return;
        };
        cx.stop_propagation();
        if !self.zoom.is_fit() && event.modifiers.shift {
            self.wheel_remainder = [0.0; 2];
            self.pan_by(-dx * 40.0, -dy * 40.0, cx);
            return;
        }
        // GPUI deltas describe content movement. Negative Y reveals later rows
        // and negative X reveals columns to the right. Keep sub-notch motion
        // across short touchpad gestures, independently for each axis.
        let mut sent = false;
        for (remainder, (delta, positive, negative)) in self.wheel_remainder.iter_mut().zip([
            (dy, Buttons::WHEEL_UP, Buttons::WHEEL_DOWN),
            (dx, Buttons::WHEEL_LEFT, Buttons::WHEEL_RIGHT),
        ]) {
            if *remainder * delta < 0.0 {
                *remainder = 0.0;
            }
            let total = *remainder + delta;
            *remainder = total.fract();
            let steps = total.trunc().clamp(-10.0, 10.0) as i32;
            let button = if steps > 0 { positive } else { negative };
            for _ in 0..steps.unsigned_abs() {
                self.input.push_wheel(x, y, button);
                sent = true;
            }
        }
        if !sent {
            return;
        }
        self.last_sent = Some((x, y));
        self.flush_input();
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if self.certificate.is_some() {
            if keystroke.key == "tab"
                && !keystroke.modifiers.control
                && !keystroke.modifiers.alt
                && !keystroke.modifiers.platform
            {
                let mut stops = vec![
                    &self.certificate_reject_focus,
                    &self.certificate_trust_focus,
                ];
                if self
                    .certificate
                    .as_ref()
                    .is_some_and(|q| !q.fingerprint.is_empty())
                {
                    stops.push(&self.certificate_remember_focus);
                }
                let active = stops.iter().position(|f| f.is_focused(window));
                let next = if keystroke.modifiers.shift {
                    (active.unwrap_or(0) + stops.len() - 1) % stops.len()
                } else {
                    active.map_or(0, |n| (n + 1) % stops.len())
                };
                window.focus(stops[next]);
                cx.stop_propagation();
            } else if keystroke.key == "escape" && keystroke.modifiers == Modifiers::default() {
                window.focus(&self.focus);
                self.answer_certificate(false, cx);
                cx.stop_propagation();
            } else if matches!(keystroke.key.as_str(), "enter" | "space")
                && keystroke.modifiers == Modifiers::default()
            {
                cx.stop_propagation();
            }
            // The pending decision owns input. Local accelerators may still bubble.
            return;
        }

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
        if self.reconnecting() {
            return;
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

    fn on_key_up(&mut self, event: &KeyUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.certificate.is_some() {
            if event.keystroke.key == "escape" && event.keystroke.modifiers == Modifiers::default()
            {
                window.focus(&self.focus);
                self.answer_certificate(false, cx);
                cx.stop_propagation();
            }
            return;
        }
        if self.reconnecting() {
            return;
        }
        let Some(codes) = key_codes(&event.keystroke.key) else {
            return;
        };
        if !self
            .input
            .held_keys()
            .iter()
            .any(|held| held.scancode == codes.scancode && held.extended == codes.extended)
        {
            return;
        }
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
                if self.reconnecting() {
                    crate::app::copy::fill(
                        crate::app::copy::ux_for(self.locale).banner_reconnecting,
                        &[("target", &self.target.target())],
                    )
                } else {
                    self.stage.headline(strings).to_owned()
                },
                match self.wait_seconds {
                    Some(seconds) => format!(
                        "{}  ·  {} {}:{:02}",
                        self.target.target(),
                        strings.desktop_wait_time,
                        seconds / 60,
                        seconds % 60
                    ),
                    None => self.target.target(),
                },
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
        let dismiss = (self.send_problem.is_some() || !self.notices.is_empty()).then(|| {
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
        Some(cursor_over(
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
            // The strip is a notice of this window's, laid over the picture: it
            // keeps the arrow the picture hides.
            CursorStyle::Arrow,
        ))
    }

    /// The controls the pane owns: the whole window and the zoom steps (R18).
    ///
    /// Docked top-center over the picture so it stays clear of a remote window's
    /// top-right close/maximize/minimize buttons and top-left menu bar. Each control
    /// carries a selector of its own so the window-level tests can press the same
    /// button the user does instead of calling the method behind it.
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
        //
        // The bar is laid out either way - a press on a step should not depend on a
        // frame of hover first - and the pane reveals it as soon as the pointer is
        // anywhere over the picture.
        let mut bar = components::floating_bar(metrics)
            .when(!self.controls_shown(), |bar| bar.opacity(0.0))
            .debug_selector(|| "desktop.controls".to_owned())
            .id(SharedString::from("desktop.controls.bar"))
            // The bar is above an occluding layer, so the body stops being hovered
            // when the pointer lands on it: the bar's own hover is what keeps it
            // drawn while a control is under the pointer.
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                this.set_bar_hovered(*hovered, cx);
            }))
            // A press on a control belongs to the control: the picture underneath
            // is not clicked at the same time.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _event, _window, cx| cx.stop_propagation()),
            )
            .child(
                components::compact_button(strings.desktop_zoom_out, Variant::Ghost, metrics)
                    .when(self.zoom.zoomed_out() == self.zoom, |button| {
                        button.opacity(0.45).cursor_default()
                    })
                    .id(SharedString::from("desktop.zoom.out"))
                    .debug_selector(|| "desktop.zoom.out".to_owned())
                    .tooltip(components::tooltip(format!(
                        "{} (Alt+-)",
                        strings.desktop_zoom_out
                    )))
                    .when(self.zoom.zoomed_out() != self.zoom, |button| {
                        button.on_click(cx.listener(|this, _event, _window, cx| this.zoom_out(cx)))
                    }),
            )
            // The readout is the switch between the two steps it names: pressing
            // "Fit" asks for 100% and pressing "100%" asks to fit the window again.
            .child(
                components::compact_button(zoom, Variant::Ghost, metrics)
                    .flex_none()
                    .min_w(metrics.scale(ZOOM_LABEL_WIDTH))
                    .justify_center()
                    .id(SharedString::from("desktop.zoom.fit"))
                    .debug_selector(|| "desktop.zoom.fit".to_owned())
                    .tooltip(components::tooltip(if self.zoom.is_fit() {
                        "100%".to_owned()
                    } else {
                        format!(
                            "{} (Alt+0) · {}",
                            strings.desktop_zoom_fit,
                            crate::app::copy::ux_for(self.locale).desktop_pan_hint
                        )
                    }))
                    .on_click(cx.listener(|this, _event, _window, cx| this.zoom_toggle(cx))),
            )
            .child(
                components::compact_button(strings.desktop_zoom_in, Variant::Ghost, metrics)
                    .when(self.zoom.zoomed_in() == self.zoom, |button| {
                        button.opacity(0.45).cursor_default()
                    })
                    .id(SharedString::from("desktop.zoom.in"))
                    .debug_selector(|| "desktop.zoom.in".to_owned())
                    .tooltip(components::tooltip(format!(
                        "{} (Alt+=)",
                        strings.desktop_zoom_in
                    )))
                    .when(self.zoom.zoomed_in() != self.zoom, |button| {
                        button.on_click(cx.listener(|this, _event, _window, cx| this.zoom_in(cx)))
                    }),
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
                .tooltip(components::tooltip(format!("{fullscreen} (F11)")))
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.toggle_fullscreen(window, cx);
                })),
            )
            // Folding the bar away is the pane's own control, so a remote screen is
            // left alone once the person has said they do not want the bar over it.
            .child(components::divider_vertical(metrics).mx(metrics.scale(spacing::HAIRLINE)))
            .child(
                components::icon_only_button(Icon::ChevronUp, metrics)
                    .id(SharedString::from("desktop.controls.collapse"))
                    .debug_selector(|| "desktop.controls.collapse".to_owned())
                    .tooltip(components::tooltip(strings.desktop_hide_controls))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.set_controls_collapsed(true, cx);
                    })),
            );
        // The bar is this window's control, not part of the picture: it shows the
        // ordinary arrow even while the picture underneath hides its pointer. The
        // buttons keep their own pointing hand, which is set on them and painted
        // after the bar.
        bar.style().mouse_cursor = Some(CursorStyle::Arrow);
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .pt(metrics.scale(spacing::TIGHT))
            .child(bar)
    }

    /// The small handle that brings a folded bar back.
    ///
    /// Docked top-center and kept softly visible while the picture is live so the
    /// user never loses the way to unfold the bar, strengthening to full opacity on
    /// hover.
    fn expand_handle(&self, metrics: Metrics, cx: &mut Context<Self>) -> gpui::Div {
        let mut handle = components::floating_bar(metrics)
            .when(!self.controls_shown(), |handle| handle.opacity(0.65))
            .debug_selector(|| "desktop.controls.handle".to_owned())
            .id(SharedString::from("desktop.controls.handle.bar"))
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                this.set_bar_hovered(*hovered, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _event, _window, cx| cx.stop_propagation()),
            )
            .child(
                components::icon_only_button(Icon::ChevronDown, metrics)
                    .id(SharedString::from("desktop.controls.expand"))
                    .debug_selector(|| "desktop.controls.expand".to_owned())
                    .tooltip(components::tooltip(
                        self.locale.strings().desktop_show_controls,
                    ))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.set_controls_collapsed(false, cx);
                    })),
            );
        // The handle is over the picture too, and it is a control: like the bar it
        // shows the arrow the picture hides.
        handle.style().mouse_cursor = Some(CursorStyle::Arrow);
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .pt(metrics.scale(spacing::TIGHT))
            .child(handle)
    }

    fn submit_credentials(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(fields) = &self.sign_in else {
            return;
        };
        if let Some(user) = &fields.user {
            let name = user.read(cx).text().trim();
            if name.is_empty() {
                window.focus(user.read(cx).focus_handle());
                return;
            }
            self.target.set_user(name.to_owned());
        }
        let password = shellune_core::SecretString::new(fields.password.read(cx).text());
        if password.is_empty() {
            window.focus(fields.password.read(cx).focus_handle());
            return;
        }
        fields
            .password
            .update(cx, |field, cx| field.set_text("", cx));
        self.sign_in = None;
        window.focus(&self.focus);
        self.start(Some(password.expose()), cx);
    }

    fn sign_in_form(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let strings = self.locale.strings();
        let metrics = theme::metrics();
        let fields = self.sign_in.as_mut().expect("sign-in form is active");
        let password = fields.password.clone();
        let user = fields.user.clone();
        if std::mem::take(&mut fields.focus_pending) {
            let field = user
                .as_ref()
                .filter(|f| f.read(cx).text().is_empty())
                .unwrap_or(&password);
            window.focus(field.read(cx).focus_handle());
        }
        let ready = !password.read(cx).text().is_empty()
            && user
                .as_ref()
                .is_none_or(|u| !u.read(cx).text().trim().is_empty());
        let row = |label, field: gpui::Entity<TextField>, selector: &'static str| {
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .w_full()
                .gap(metrics.scale(spacing::NARROW))
                .child(components::form_label(label, metrics))
                .child(
                    div()
                        .w_full()
                        .debug_selector(move || selector.to_owned())
                        .child(field),
                )
        };
        let cancel_focus = fields.cancel_focus.clone();
        let connect_focus = fields.connect_focus.clone();
        let mut stops: Vec<FocusHandle> = user
            .iter()
            .chain(std::iter::once(&password))
            .map(|field| field.read(cx).focus_handle().clone())
            .collect();
        stops.push(cancel_focus.clone());
        if ready {
            stops.push(connect_focus.clone());
        }
        let connect = if ready {
            components::button(strings.remote_connect, Variant::Primary, metrics)
        } else {
            components::button_with_state(
                strings.remote_connect,
                Variant::Primary,
                components::ControlState::Disabled,
                metrics,
            )
        };
        let card = components::status_card(
            Tone::Info,
            icons::icon(Icon::Lock, metrics.icon_size(), Tone::Info.text()).into_any_element(),
            div().child(strings.desktop_credentials_title),
            Some(div().child(format!("{} · {}", self.target.name(), self.target.target()))),
            metrics,
        )
        .debug_selector(|| "desktop.signin.card".to_owned())
        .gap(metrics.scale(spacing::BASE))
        .children(fields.rejection.clone().map(|reason| {
            div()
                .text_size(metrics.caption_size())
                .text_color(Tone::Danger.text())
                .child(reason)
        }))
        .children(
            user.clone()
                .map(|field| row(strings.host_field_user, field, "desktop.signin.user")),
        )
        .child(row(
            strings.auth_password,
            password.clone(),
            "desktop.signin.password",
        ))
        .child(
            div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap(metrics.scale(spacing::TIGHT))
                .child(
                    components::button(strings.host_cancel, Variant::Secondary, metrics)
                        .id("desktop.signin.cancel")
                        .track_focus(&cancel_focus)
                        .focus(|s| {
                            s.border_2()
                                .border_color(theme::swatch(palette::FOCUS_RING))
                        })
                        .on_click(cx.listener(|pane, _, _, cx| {
                            pane.sign_in = None;
                            pane.fail(LinkFailure::Cancelled, "", cx);
                        })),
                )
                .child(connect.id("desktop.signin.connect").when(ready, |btn| {
                    btn.track_focus(&connect_focus)
                        .focus(|s| {
                            s.border_2()
                                .border_color(theme::swatch(palette::FOCUS_RING))
                        })
                        .on_click(
                            cx.listener(|pane, _, window, cx| pane.submit_credentials(window, cx)),
                        )
                })),
        );
        div()
            .id("desktop.signin")
            .track_focus(&self.focus)
            .debug_selector(|| "desktop.signin".to_owned())
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .justify_center()
            .bg(theme::swatch(palette::CANVAS))
            .child(
                div()
                    .id("desktop.signin.scroll")
                    .w_full()
                    .min_h_0()
                    .max_h_full()
                    .overflow_y_scroll()
                    .p(metrics.scale(spacing::BASE))
                    .child(components::status_card_slot(card, metrics)),
            )
            .capture_key_down(cx.listener(move |pane, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "enter" | "space" if cancel_focus.is_focused(window) => {
                        pane.sign_in = None;
                        pane.fail(LinkFailure::Cancelled, "", cx);
                    }
                    "enter" => pane.submit_credentials(window, cx),
                    "space" if connect_focus.is_focused(window) => {
                        pane.submit_credentials(window, cx)
                    }
                    "escape" => {
                        pane.sign_in = None;
                        pane.fail(LinkFailure::Cancelled, "", cx);
                    }
                    "tab" => {
                        let at = stops.iter().position(|f| f.is_focused(window));
                        let next = if event.keystroke.modifiers.shift {
                            (at.unwrap_or(0) + stops.len() - 1) % stops.len()
                        } else {
                            at.map_or(0, |n| (n + 1) % stops.len())
                        };
                        window.focus(&stops[next]);
                    }
                    _ => return,
                }
                cx.stop_propagation();
            }))
    }

    fn certificate_card(&self, metrics: Metrics, cx: &mut Context<Self>) -> gpui::Div {
        let strings = self.locale.strings();
        let Some(question) = &self.certificate else {
            return div();
        };
        let label = |text| {
            div()
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .child(text)
        };
        let mut details = div()
            .flex()
            .flex_col()
            .min_w_0()
            .gap(metrics.scale(spacing::TIGHT));
        for (field, value) in [
            (strings.desktop_certificate_subject, &question.subject),
            (strings.desktop_certificate_issuer, &question.issuer),
        ] {
            if !value.trim().is_empty() {
                details = details.child(
                    div()
                        .child(label(field))
                        .child(SharedString::from(value.clone())),
                );
            }
        }
        if !question.fingerprint.is_empty() {
            let octets: Vec<_> = question.fingerprint.split(':').collect();
            details = details.child(
                div().child(label(strings.trust_fingerprint)).child(
                    components::copy_on_click(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .font_family(crate::app::terminal::monospace_family())
                            .debug_selector(|| "desktop.certificate.fingerprint".to_owned()),
                        question.fingerprint.clone(),
                    )
                    .children(octets.chunks(16).map(|row| {
                        div().child(
                            row.chunks(4)
                                .map(|group| group.join(":"))
                                .collect::<Vec<_>>()
                                .join("  "),
                        )
                    })),
                ),
            );
        }
        if let Some(pin) = &self.previous_pin
            && pin != &question.fingerprint
        {
            details = details
                .child(label(strings.desktop_certificate_changed))
                .child(pin.clone());
        }
        if !question.fingerprint.is_empty() {
            details = details.child(
                components::checkbox(
                    "desktop.certificate.remember",
                    strings.desktop_certificate_remember,
                    self.remember_certificate,
                    metrics,
                )
                .track_focus(&self.certificate_remember_focus)
                .focus(|style| {
                    style
                        .border_1()
                        .border_color(theme::swatch(palette::FOCUS_RING))
                })
                .on_click(cx.listener(|pane, _, _, cx| {
                    pane.remember_certificate = !pane.remember_certificate;
                    pane.pin_error = None;
                    cx.notify();
                })),
            );
        }
        if let Some(error) = &self.pin_error {
            details = details.child(error.clone());
        }
        components::status_card(
            Tone::Warning,
            icons::icon(Icon::Lock, metrics.icon_size(), Tone::Warning.text()).into_any_element(),
            div().child(strings.desktop_certificate_title),
            Some(div().child(SharedString::from(if question.host.is_empty() {
                self.target.target()
            } else {
                question.host.clone()
            }))),
            metrics,
        )
        .debug_selector(|| "desktop.certificate".to_owned())
        .child(
            details
                .text_size(metrics.label_size())
                .text_color(theme::swatch(palette::TEXT_PRIMARY)),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap(metrics.scale(spacing::TIGHT))
                .child(
                    components::compact_button(
                        strings.desktop_certificate_reject,
                        Variant::Secondary,
                        metrics,
                    )
                    .id(SharedString::from("desktop.certificate.reject"))
                    .track_focus(&self.certificate_reject_focus)
                    .focus(|style| {
                        style
                            .border_2()
                            .border_color(theme::swatch(palette::FOCUS_RING))
                    })
                    .debug_selector(|| "desktop.certificate.reject".to_owned())
                    .on_click(cx.listener(|this, _event, window, cx| {
                        cx.stop_propagation();
                        window.focus(&this.focus);
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
                    .track_focus(&self.certificate_trust_focus)
                    .focus(|style| {
                        style
                            .border_2()
                            .border_color(theme::swatch(palette::FOCUS_RING))
                    })
                    .debug_selector(|| "desktop.certificate.trust".to_owned())
                    .on_click(cx.listener(|this, _event, window, cx| {
                        cx.stop_propagation();
                        window.focus(&this.focus);
                        this.answer_certificate(true, cx);
                    })),
                ),
        )
    }
}

/// Windows sends no pixels for SYSPTR_DEFAULT. Use a white, dark-outlined
/// arrow until the server selects a specific (possibly user-customized) shape.
fn windows_arrow() -> &'static PointerShape {
    static ARROW: std::sync::OnceLock<PointerShape> = std::sync::OnceLock::new();
    ARROW.get_or_init(|| {
        let rows = [
            "#",
            "##",
            "#.#",
            "#..#",
            "#...#",
            "#....#",
            "#.....#",
            "#......#",
            "#.......#",
            "#........#",
            "#.........#",
            "#..........#",
            "#...........#",
            "#.......#####",
            "#...#...#",
            "#..# #...#",
            "#.#  #...#",
            "##    #...#",
            "#     #...#",
            "       ###",
        ];
        let mut pixels = vec![0; 16 * rows.len() * 4];
        for (y, row) in rows.iter().enumerate() {
            for (x, v) in row.bytes().enumerate() {
                let pixel = match v {
                    b'#' => [0, 0, 0, 255],
                    b'.' => [255; 4],
                    _ => continue,
                };
                pixels[(y * 16 + x) * 4..(y * 16 + x + 1) * 4].copy_from_slice(&pixel);
            }
        }
        PointerShape::Bitmap {
            width: 16,
            height: rows.len() as u16,
            hotspot: (0, 0),
            pixels: pixels.into(),
        }
    })
}

/// Round each shared source edge once in window coordinates. Rounding a tile's
/// position and width independently can leave a physical pixel unpainted.
fn desktop_tile_bounds(
    bounds: Bounds<Pixels>,
    desktop: (u32, u32),
    index: usize,
    scale: f32,
) -> Bounds<gpui::DevicePixels> {
    use crate::app::desktop::DESKTOP_TILE;
    let (width, height) = desktop;
    let columns = width.div_ceil(DESKTOP_TILE);
    let x = index as u32 % columns * DESKTOP_TILE;
    let y = index as u32 / columns * DESKTOP_TILE;
    let edge = |origin: Pixels, length: Pixels, offset: u32, total: u32| {
        let position = origin + length * (offset.min(total) as f32 / total as f32);
        gpui::DevicePixels((f32::from(position) * scale).round() as i32)
    };
    let x_edge = |x| edge(bounds.left(), bounds.size.width, x, width);
    let y_edge = |y| edge(bounds.top(), bounds.size.height, y, height);
    Bounds::from_corners(
        gpui::point(x_edge(x), y_edge(y)),
        gpui::point(x_edge(x + DESKTOP_TILE), y_edge(y + DESKTOP_TILE)),
    )
}

/// Shows `cursor` while the pointer is over `element`.
///
/// GPUI has no cursor builder on a `Div` in this version: the element reads
/// [`gpui::Style::mouse_cursor`] at prepaint, so the field is set here and the
/// element is handed back to the builder chain. A later-painted child that sets
/// its own cursor (a button's pointing hand) wins over this one.
fn cursor_over(mut element: gpui::Div, cursor: CursorStyle) -> gpui::Div {
    element.style().mouse_cursor = Some(cursor);
    element
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
        let metrics = theme::metrics();
        if self.sign_in.is_some() {
            return self.sign_in_form(window, cx);
        }
        self.update_pointer_image(window, cx);
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
                }
                cx.notify();
            });
            let activated = cx.observe_window_activation(window, |pane, window, cx| {
                if !window.is_window_active() {
                    pane.release_input();
                }
                cx.notify();
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
        #[cfg(all(target_os = "macos", not(test)))]
        if active && self.is_live() && self.certificate.is_none() && self.focus.is_focused(window) {
            if self.hotkey_capture.is_none() {
                self.hotkey_capture = mac_hotkeys::Capture::acquire();
            }
        } else {
            self.hotkey_capture = None;
        }
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
        self.update_picture(window, cx);
        // Whether the picture is what the body shows. The status card below stands in
        // for it while it is absent, and the warning strip belongs over the picture
        // only: drawing both would say the same thing twice before the first frame.
        let has_picture = !self.images.is_empty();
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
            .on_children_prepainted(move |bounds, window, app| {
                if let Some(bounds) = bounds.first().copied() {
                    let _ = entity.update(app, |pane, cx| {
                        pane.set_viewport(bounds, window.scale_factor(), cx);
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
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_key_down(event, window, cx);
            }))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| {
                this.on_key_up(event, window, cx);
            }))
            .on_modifiers_changed(cx.listener(
                |this, event: &ModifiersChangedEvent, _window, cx| {
                    this.on_modifiers_changed(event, cx);
                },
            ))
            // The identity is what lets the pointer's arrival be observed: the bar
            // over the picture follows it, so it is out of the way of a remote screen
            // nobody is pointing at.
            .id(SharedString::from("desktop.body"))
            // The bar over the picture follows the pointer: over the pane it is drawn,
            // away from it the remote screen is left alone.
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                if this.controls_hovered != *hovered {
                    this.controls_hovered = *hovered;
                    cx.notify();
                }
            }));

        #[cfg(test)]
        {
            self.presented_cursor = CursorStyle::Arrow;
        }
        if has_picture {
            // The image is drawn with the fit its own inverse expects, so the
            // pixel under the pointer is the pixel that was drawn there (R18).
            // At the Fit step that fit is GPUI's own Contain over the whole
            // pane; at a zoom step the picture is drawn at the size the
            // geometry says and moved by the pan, and the pane crops it.
            // Share the fitted and panned rectangle with pointer hit testing.
            // Bounds use window coordinates; the image is placed in its pane.
            let image = if let Some(viewport) = self.viewport {
                let rect = content_rect_in(viewport, self.surface.size(), self.zoom, self.pan);
                let mut picture_layer = div()
                    .absolute()
                    .overflow_hidden()
                    .left(rect.origin.x - viewport.origin.x)
                    .top(rect.origin.y - viewport.origin.y)
                    .w(rect.size.width)
                    .h(rect.size.height)
                    .debug_selector(|| "desktop.picture".to_owned())
                    .child(self.picture_tiles());
                let mut style = self.fallback_cursor();
                let shape = match &self.pointer_shape {
                    Some(PointerShape::Default)
                        if self.target.protocol() == shellune_core::ProtocolKind::Rdp =>
                    {
                        Some(windows_arrow())
                    }
                    other => other.as_ref(),
                };
                if self.is_live()
                    && self.certificate.is_none()
                    && let Some(PointerShape::Bitmap {
                        width,
                        height,
                        hotspot,
                        ..
                    }) = shape
                    && let Some(cursor) = &self.pointer_image
                {
                    let scale = if self.pointer_shape == Some(PointerShape::Default) {
                        1.0
                    } else {
                        f32::from(rect.size.width) / self.surface.size().0 as f32
                    };
                    #[cfg(target_os = "macos")]
                    let native_style = {
                        if !self
                            .native_cursor
                            .as_ref()
                            .is_some_and(|(old, _)| *old == scale)
                        {
                            self.native_cursor = window
                                .bitmap_cursor(cursor, *hotspot, scale)
                                .map(|c| (scale, c));
                        }
                        self.native_cursor.as_ref().map(|(_, c)| c.style())
                    };
                    #[cfg(not(target_os = "macos"))]
                    let native_style: Option<CursorStyle> = None;
                    if let Some(native) = native_style {
                        style = native;
                    } else {
                        let position = window.mouse_position();
                        if self.controls_hovered && !self.bar_hovered && rect.contains(&position) {
                            style = CursorStyle::None;
                            picture_layer = picture_layer.child(
                                img(cursor.clone())
                                    .absolute()
                                    .debug_selector(|| "desktop.pointer".to_owned())
                                    .left(
                                        position.x
                                            - rect.origin.x
                                            - px(scale * f32::from(hotspot.0)),
                                    )
                                    .top(
                                        position.y
                                            - rect.origin.y
                                            - px(scale * f32::from(hotspot.1)),
                                    )
                                    .w(px(scale * f32::from(*width)))
                                    .h(px(scale * f32::from(*height))),
                            );
                        }
                    }
                }
                #[cfg(test)]
                {
                    self.presented_cursor = style;
                }
                cursor_over(picture_layer, style).into_any_element()
            } else {
                div().size_full().into_any_element()
            };
            let surface = div()
                .flex_1()
                .min_h_0()
                .relative()
                .overflow_hidden()
                .debug_selector(|| "desktop.surface".to_owned())
                .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                    this.on_scroll(event, cx);
                }))
                .child(image);
            body = body.child(surface);
        } else {
            // A link that comes up in under the floor shows its picture instead of
            // a card that flashes on the way; the pane keeps its space either way.
            let early = matches!(self.phase, DesktopPhase::Connecting)
                && self
                    .connecting_since
                    .is_some_and(|since| since.elapsed() < crate::app::CONNECT_STATE_FLOOR);
            body = body.child(if early || self.certificate.is_some() {
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
        if self.reconnecting() && self.certificate.is_none() {
            body = body.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(self.status_card(metrics)),
            );
        }
        if has_picture && let Some(notice) = self.notice_strip(metrics, cx) {
            body = body.child(notice);
        }
        if self.certificate.is_some() {
            let card = self.certificate_card(metrics, cx);
            // The question is this window's, over a picture that may be hiding the
            // pointer: it keeps the arrow so the card can be read and answered.
            body = body.child(cursor_over(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(metrics.scale(spacing::LOOSE))
                    .child(
                        div()
                            .id("desktop.certificate.scroll")
                            .min_h_0()
                            .max_h_full()
                            .w_full()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .items_center()
                            .child(components::status_card_slot(card, metrics)),
                    ),
                CursorStyle::Arrow,
            ));
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
        // The controls belong to the pane, not to the session: they are drawn over
        // the picture whenever there is a link to control (R18). They go on last so
        // nothing drawn over the picture - the warning strip, the certificate card -
        // sits between the pointer and a control. A folded bar is not drawn at all;
        // only its handle is, and only while the pointer is over the picture.
        if has_picture {
            if self.controls_collapsed {
                body = body.child(self.expand_handle(metrics, cx));
            } else {
                #[cfg(test)]
                {
                    self.controls_frames += 1;
                }
                body = body.child(self.controls(metrics, cx));
            }
        }
        body
    }
}

impl DesktopPane {
    /// Keeps the space the picture is drawn in, and renegotiates the remote size
    /// when it changed.
    fn set_viewport(&mut self, bounds: Bounds<Pixels>, scale: f32, cx: &mut Context<Self>) {
        if self.viewport != Some(bounds) {
            self.viewport = Some(bounds);
            cx.notify();
        }
        let Some(size) = framebuffer_size(bounds.size, scale) else {
            return;
        };
        // If the framebuffer was capped, use its effective pixels per logical point.
        let percent = ((size.0 as f32 / f32::from(bounds.size.width)).min(scale) * 100.)
            .round()
            .clamp(100., 500.) as u32;
        if size != self.asked_size || percent != self.asked_scale {
            if let Some(link) = &self.link
                && (!self.phase.accepts_input() || link.resize(size.0, size.1, percent).is_err())
            {
                return;
            }
            self.asked_size = size;
            self.asked_scale = percent;
        }
        if self.pending_dial.is_some() {
            self.dial_pending(cx);
        }
    }
}

/// Use the pane's physical pixels independently of local zoom or the previous
/// framebuffer's aspect ratio. Retain the aspect ratio when limiting a 5K/8K display.
fn framebuffer_size(size: gpui::Size<Pixels>, scale: f32) -> Option<(u32, u32)> {
    let w = f64::from(f32::from(size.width) * scale);
    let h = f64::from(f32::from(size.height) * scale);
    if w < 1.0 || h < 1.0 {
        return None;
    }
    let limit = (shellune_desktop::MAX_FRAME_BYTES as f64 / (4.0 * w * h))
        .sqrt()
        .min(8192.0 / w.max(h))
        .min(1.0);
    let w = ((w * limit).floor() as u32).clamp(200, 8192) & !1;
    let h = ((h * limit).floor() as u32).clamp(200, 8192);
    Some((w, h))
}

#[cfg(test)]
mod tile_tests {
    use super::*;
    #[test]
    fn retina_window_and_fullscreen_request_the_entire_physical_viewport() {
        assert_eq!(
            framebuffer_size(gpui::size(px(1120.), px(674.)), 2.),
            Some((2240, 1348))
        );
        assert_eq!(
            framebuffer_size(gpui::size(px(1470.), px(923.)), 2.),
            Some((2940, 1846))
        );
        assert_eq!(framebuffer_size(gpui::size(px(0.), px(400.)), 2.), None);
    }

    #[test]
    fn adjoining_tiles_cover_the_panned_desktop_at_fractional_dpi() {
        let bounds = Bounds::new(
            gpui::point(px(-13.25), px(47.4)),
            gpui::size(px(1243.2), px(810.1)),
        );
        let desktop = (2940, 1912);
        for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
            let tile = |index| desktop_tile_bounds(bounds, desktop, index, scale);
            let cols = desktop.0.div_ceil(crate::app::desktop::DESKTOP_TILE) as usize;
            let count = cols * desktop.1.div_ceil(crate::app::desktop::DESKTOP_TILE) as usize;
            for index in 0..count {
                if index % cols < cols - 1 {
                    assert_eq!(tile(index).right(), tile(index + 1).left());
                }
                if index < 18 {
                    assert_eq!(tile(index).bottom(), tile(index + cols).top());
                }
            }
            assert_eq!(
                tile(0).left().0,
                (f32::from(bounds.left()) * scale).round() as i32
            );
            assert_eq!(
                tile(count - 1).right().0,
                (f32::from(bounds.right()) * scale).round() as i32
            );
            assert_eq!(
                tile(count - 1).bottom().0,
                (f32::from(bounds.bottom()) * scale).round() as i32
            );
        }
    }
}
