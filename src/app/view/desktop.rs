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
    Bounds, ClipboardItem, Context, FocusHandle, KeyDownEvent, KeyUpEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, RenderImage, ScrollDelta,
    ScrollWheelEvent, SharedString, Window, div, img, px,
};
use shellune_core::SessionFailure;

use crate::app::components::{self, Variant};
use crate::app::copy::Locale;
use crate::app::desktop::{
    Availability, Buttons, DESKTOP_POLL_INTERVAL, DesktopLink, DesktopSurface, DesktopTarget,
    DesktopZoom, InputQueue, LinkEvent, LinkFailure, LinkState, NativeLink, availability,
    clamp_pan, content_rect, encryption_note, key_codes, to_remote_in,
};
use crate::app::icons::{self, Icon};
use crate::app::theme::{self, Metrics, Tone, palette, radius, spacing};

/// The size a session asks for before the window has measured anything.
///
/// A remote desktop negotiates its size at connect time, so the first ask is the
/// size of the window this build opens with; the pane renegotiates as soon as it
/// knows the space it really has.
pub const FIRST_DESKTOP_SIZE: (u32, u32) = (1280, 720);

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
            failed_stage: None,
            zoom: DesktopZoom::Fit,
            pan: (0.0, 0.0),
            fullscreen: false,
            viewport: None,
            asked_size: FIRST_DESKTOP_SIZE,
            last_sent: None,
            certificate: None,
            layer_reason: None,
            send_problem: None,
            polling: false,
            watching_focus: false,
            was_active: false,
            image: None,
            image_size: (0, 0),
            locale,
        }
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

    /// Starts the session: dials it and takes the first frames.
    ///
    /// A protocol this build cannot speak ends the session with the reason instead
    /// of leaving a pane that looks like it is connecting.
    pub fn start(&mut self, password: Option<&str>, cx: &mut Context<Self>) {
        if self.link.is_some() {
            return;
        }
        if self.availability.is_built() {
            self.stage = DesktopStage::Connecting;
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
        self.failed_stage = None;
        self.surface.reset();
        self.image = None;
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
            LinkEvent::Clipboard { text } => {
                // The peer's clipboard becomes this computer's, which is the only
                // direction a user expects to be automatic.
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                true
            }
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
        let paste = keystroke.key == "v"
            && (keystroke.modifiers.control || keystroke.modifiers.platform)
            && !keystroke.modifiers.alt;
        if paste {
            // Paste is this computer's clipboard going to the remote one. A backend
            // that cannot carry text says so instead of dropping it silently.
            if let Some(item) = cx.read_from_clipboard()
                && let Some(text) = item.text()
            {
                self.input.push_clipboard(text);
                self.flush_input();
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let Some(codes) = key_codes(&keystroke.key) else {
            // A key this build does not translate is not sent as something else.
            return;
        };
        self.input.push_key(codes, true);
        self.flush_input();
        cx.stop_propagation();
        cx.notify();
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
        let mut card = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .p(metrics.scale(spacing::SNUG))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::SURFACE))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(|| "desktop.status".to_owned())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(icons::icon(Icon::Monitor, metrics.scale(14.0), tone.text()))
                    .child(
                        div()
                            .text_size(metrics.scale(13.0))
                            .text_color(tone.text())
                            .child(SharedString::from(headline)),
                    ),
            )
            .child(
                div()
                    .text_size(metrics.scale(12.0))
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(SharedString::from(detail)),
            );
        if let Some(problem) = &self.send_problem {
            card = card.child(
                div()
                    .text_size(metrics.scale(12.0))
                    .text_color(Tone::Warning.text())
                    .child(SharedString::from(problem.clone())),
            );
        }
        card
    }

    /// The strip that says a send did not reach the remote side (R18).
    ///
    /// It is a layer over the picture rather than a line in the status card,
    /// because the card is only drawn while there is no picture: the moment input
    /// starts failing is exactly the moment the session is live, and a sentence
    /// nobody can see is the same as swallowing the text. Nothing here is cleared
    /// by the next frame - only by a later send that really carried something -
    /// so a lost paste stays on screen long enough to be read.
    fn notice_strip(&self, metrics: Metrics) -> Option<gpui::Div> {
        let problem = self.send_problem.as_ref()?;
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .m(metrics.scale(spacing::TIGHT))
                .max_w(px(420.0))
                .flex()
                .items_center()
                .gap(metrics.scale(spacing::TIGHT))
                .p(metrics.scale(spacing::TIGHT))
                .rounded(px(radius::MEDIUM))
                .bg(theme::swatch(palette::SURFACE))
                .border_1()
                .border_color(Tone::Warning.text())
                .debug_selector(|| "desktop.notice".to_owned())
                .child(icons::icon(
                    Icon::Monitor,
                    metrics.scale(12.0),
                    Tone::Warning.text(),
                ))
                .child(
                    div()
                        .text_size(metrics.scale(11.0))
                        .text_color(Tone::Warning.text())
                        .child(SharedString::from(problem.clone())),
                ),
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
        div()
            .absolute()
            .top_0()
            .right_0()
            .m(metrics.scale(spacing::TIGHT))
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .p(metrics.scale(spacing::HAIRLINE))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::SURFACE))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(|| "desktop.controls".to_owned())
            // A press on a control belongs to the control: the picture underneath
            // is not clicked at the same time.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _event, _window, cx| cx.stop_propagation()),
            )
            .child(
                components::button(strings.desktop_zoom_out, Variant::Ghost, metrics)
                    .id(SharedString::from("desktop.zoom.out"))
                    .debug_selector(|| "desktop.zoom.out".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| this.zoom_out(cx))),
            )
            .child(
                div()
                    .px(metrics.scale(spacing::TIGHT))
                    .text_size(metrics.scale(11.0))
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "desktop.zoom.label".to_owned())
                    .child(SharedString::from(zoom)),
            )
            .child(
                components::button(strings.desktop_zoom_in, Variant::Ghost, metrics)
                    .id(SharedString::from("desktop.zoom.in"))
                    .debug_selector(|| "desktop.zoom.in".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| this.zoom_in(cx))),
            )
            .child(
                components::button(fullscreen, Variant::Ghost, metrics)
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
        div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .p(metrics.scale(spacing::SNUG))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::SURFACE))
            .border_1()
            .border_color(Tone::Warning.text())
            .debug_selector(|| "desktop.certificate".to_owned())
            .child(
                div()
                    .text_size(metrics.scale(13.0))
                    .child(SharedString::from(question.summary())),
            )
            .child(
                div()
                    .text_size(metrics.scale(11.0))
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(SharedString::from(fingerprint)),
            )
            .child(
                div()
                    .flex()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        components::button(
                            strings.desktop_certificate_trust,
                            Variant::Primary,
                            metrics,
                        )
                        .id(SharedString::from("desktop.certificate.trust"))
                        .debug_selector(|| "desktop.certificate.trust".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.answer_certificate(true, cx);
                            },
                        )),
                    )
                    .child(
                        components::button(
                            strings.desktop_certificate_reject,
                            Variant::Danger,
                            metrics,
                        )
                        .id(SharedString::from("desktop.certificate.reject"))
                        .debug_selector(|| "desktop.certificate.reject".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.answer_certificate(false, cx);
                            },
                        )),
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
        }
        // A hidden or minimised window sends no input anywhere, so the moment it
        // stops being the active one everything it may have held is let go (R17).
        // The flag makes that one transition, not one release per frame.
        let active = window.is_window_active();
        if self.was_active && !active {
            self.release_input();
        }
        self.was_active = active;
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
            .min_h(px(0.0))
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
            }));

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
                        .min_h(px(0.0))
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
                            .min_h(px(0.0))
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
                body = body.child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(self.status_card(metrics)),
                );
            }
        }
        // The controls belong to the pane, not to the session: they are drawn over
        // the picture whenever there is a link to control (R18).
        if !matches!(self.phase, DesktopPhase::NotStarted) {
            body = body.child(self.controls(metrics, cx));
        }
        if has_picture && let Some(notice) = self.notice_strip(metrics) {
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
                    .child(card),
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
                    .text_size(metrics.scale(11.0))
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
