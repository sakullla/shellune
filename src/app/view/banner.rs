//! The banner over a terminal whose session ended or is coming back (ADR-6, R5).
//!
//! A session that dropped, failed after it had shown something, or is reconnecting
//! states that in one line above its terminal instead of a card over it: the grid moves
//! down under the banner and keeps its last screen, which can still be scrolled,
//! selected and copied. The line holds the tone's icon, one sentence, the one next step
//! (reconnect, close, or cancel while a reconnect runs), the details and a close. The
//! details unfold under the line: the raw diagnostic in the monospace face, at most
//! [`DIAGNOSTIC_MAX_HEIGHT`] high and scrolling past that, with a command that copies all
//! of it. Sessions that have nothing on screen yet - a first connection, a question
//! about a certificate or a password - keep the centred card.
//!
//! The banner never takes the keyboard when it appears. Tab from a terminal that no
//! longer takes input reaches its first control, Tab and Shift+Tab walk its controls,
//! Enter or Space presses one, and Escape closes the banner until the session states
//! something else.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    Context, Div, Entity, FocusHandle, KeyDownEvent, MouseButton, SharedString, Stateful, Window,
    div,
};
use shellune_core::SessionId;

use super::{ShelluneView, may_reconnect};
use crate::app::components::{self, Progress, Variant};
use crate::app::copy;
use crate::app::icons::Icon;
use crate::app::terminal::{self, TerminalPane};
use crate::app::theme::{self, Tone, palette, spacing};
use crate::app::workspace::{SessionPhase, SessionStatus, SessionStep};

/// The tallest the unfolded diagnostic grows before it scrolls, in logical pixels.
pub(crate) const DIAGNOSTIC_MAX_HEIGHT: f32 = 160.0;

/// The largest share of its pane's height the unfolded diagnostic takes, whatever
/// [`DIAGNOSTIC_MAX_HEIGHT`] allows at the window's text size: a short pane - 720x480
/// at 200% - keeps rows of its last screen under the details (T9).
pub(crate) const DIAGNOSTIC_MAX_SHARE: f32 = 0.4;

/// The narrowest a banner draws its commands with their words, in logical pixels; a
/// narrower one - a split pane, a small window at 200% - draws them as icons named by
/// their tooltips, so the sentence keeps its room on the one line.
const BANNER_WORDS_WIDTH: f32 = 560.0;

/// The banner's controls, in the order the keyboard walks them; each has its own
/// focus handle in [`ShelluneView`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BannerStop {
    /// Reconnect, close the session, or cancel the reconnect.
    Next,
    /// Show or hide the diagnostic.
    Details,
    /// Copy the diagnostic.
    Copy,
    /// Close the banner.
    Dismiss,
}

impl BannerStop {
    fn slot(self) -> usize {
        match self {
            Self::Next => 0,
            Self::Details => 1,
            Self::Copy => 2,
            Self::Dismiss => 3,
        }
    }
}

/// What a banner states, decided once per draw.
enum BannerState {
    /// The session is over: its status, and whether the next step can run here.
    Ended(SessionStatus),
    /// A reconnect runs over the last screen of the session.
    Reconnecting,
}

type Press = Rc<dyn Fn(&mut ShelluneView, &mut Window, &mut Context<ShelluneView>)>;

impl ShelluneView {
    /// What the banner of session `id` states, when it has one to state.
    ///
    /// The session record answers first: it is where a refused reconnect is written.
    /// The pane is only asked while the record has not caught up with it (R17).
    fn banner_state(&self, id: SessionId, cx: &gpui::App) -> Option<BannerState> {
        let status = self.workspace.status(id);
        match status.as_ref().map(SessionStatus::phase) {
            Some(SessionPhase::Ended) => status.map(BannerState::Ended),
            Some(SessionPhase::Starting)
                if self.reconnect_screen(id).is_some()
                    && self
                        .workspace
                        .connection_wait(id)
                        .is_none_or(|wait| wait.visible()) =>
            {
                Some(BannerState::Reconnecting)
            }
            // A remote desktop that ended states it in the same banner, above its own
            // card (ADR-7).
            _ => self
                .pane_for(id)
                .and_then(|pane| pane.read(cx).state().failure())
                .or_else(|| {
                    self.desktop_for(id)
                        .and_then(|pane| pane.read(cx).phase().failure())
                })
                .map(|failure| BannerState::Ended(SessionStatus::ended(failure))),
        }
    }

    /// The words a banner is closed against: a closed banner stays closed until its
    /// session states something else.
    fn banner_key(state: &BannerState) -> String {
        match state {
            BannerState::Ended(status) => format!(
                "ended|{}|{}",
                status.headline(copy::Locale::English),
                status.diagnostic().unwrap_or_default()
            ),
            BannerState::Reconnecting => "reconnecting".to_owned(),
        }
    }

    /// Whether session `id` shows its banner now: it has one to state and the user has
    /// not closed it.
    pub(super) fn banner_shown(&self, id: SessionId, cx: &gpui::App) -> bool {
        self.banner_state(id, cx).is_some_and(|state| {
            self.banner_closed.get(&id).map(String::as_str) != Some(&Self::banner_key(&state))
        })
    }

    /// Closes the banner of session `id` until its session states something else, and
    /// gives the keyboard back to its terminal when the banner held it (ADR-6).
    pub(super) fn dismiss_banner(
        &mut self,
        id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.banner_state(id, cx) else {
            return;
        };
        let held = self
            .banner_focus
            .iter()
            .any(|handle| handle.is_focused(window));
        self.banner_closed.insert(id, Self::banner_key(&state));
        if held {
            self.banner_focus_terminal(id, window, cx);
        }
        cx.notify();
    }

    /// Closes the banner of the session on screen, for Escape; says whether one was.
    pub(super) fn dismiss_active_banner(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(id) = self.bar_session().or(self.active) else {
            return false;
        };
        if !self.banner_shown(id, cx) {
            return false;
        }
        self.dismiss_banner(id, window, cx);
        true
    }

    /// Gives the keyboard to the terminal under the banner of session `id`.
    fn banner_focus_terminal(
        &mut self,
        id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let handle = self
            .pane_for(id)
            .or_else(|| self.reconnect_screen(id))
            .map(|pane| pane.read(cx).focus_handle().clone())
            .or_else(|| {
                self.desktop_for(id)
                    .map(|pane| pane.read(cx).focus_handle().clone())
            });
        match handle {
            Some(handle) => window.focus(&handle),
            None => self.reclaim_focus(window, cx),
        }
    }

    /// The last screen kept for session `id` while it has no terminal of its own: a
    /// reconnect runs, or one was cancelled (ADR-6).
    ///
    /// While the attempt asks a question or has failed, its card is what the tab shows,
    /// so the screen is not offered then.
    pub(super) fn reconnect_screen(&self, id: SessionId) -> Option<&Entity<TerminalPane>> {
        if self.pane_for(id).is_some() {
            return None;
        }
        let (screen, _) = self.reconnect_screens.get(&id)?;
        let phase = self.workspace.status(id)?.phase();
        let form = self.form_session == Some(id) && self.showing_connection_form;
        let quiet = self
            .remote
            .as_ref()
            .is_some_and(|panel| panel.status().is_busy() && !panel.status().is_asking());
        match phase {
            SessionPhase::Starting if !form || quiet => Some(screen),
            SessionPhase::Ended if !form => Some(screen),
            _ => None,
        }
    }

    /// Whether the connection card of the form on screen gives way to the last screen
    /// of the session it reconnects (ADR-6).
    pub(super) fn connection_behind_screen(&self) -> bool {
        self.form_session
            .is_some_and(|id| self.reconnect_screen(id).is_some())
    }

    /// Keeps the last screen of a remote session that is about to reconnect (ADR-6).
    pub(super) fn keep_reconnect_screen(&mut self, id: SessionId) {
        // A banner closed before the reconnect is open again: whatever the attempt ends
        // in - a cancel back to the same reason included - is news (T9).
        self.banner_closed.remove(&id);
        let failure = self
            .workspace
            .session(id)
            .and_then(|session| match session.state() {
                shellune_core::SessionState::Disconnected(failure)
                | shellune_core::SessionState::Failed(failure) => Some(failure.clone()),
                _ => None,
            });
        match (self.pane_for(id).cloned(), failure) {
            (Some(pane), Some(failure)) => {
                self.reconnect_screens.insert(id, (pane, failure));
            }
            // A reconnect from a kept screen keeps it: the screen is still the last one.
            (None, Some(failure)) => {
                if let Some(kept) = self.reconnect_screens.get_mut(&id) {
                    kept.1 = failure;
                }
            }
            _ => {}
        }
    }

    /// Forgets the last screen of session `id`: a new terminal took its place, or the
    /// session is gone.
    pub(super) fn forget_reconnect_screen(&mut self, id: SessionId) {
        self.reconnect_screens.remove(&id);
        self.banner_closed.remove(&id);
    }

    /// Stops the reconnect of session `id` and goes back to its last screen and the
    /// reason it ended, as before the reconnect was pressed (ADR-6).
    pub(super) fn cancel_reconnect(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some((_, failure)) = self.reconnect_screens.get(&id).cloned() else {
            return;
        };
        self.discard_attempt(id);
        if let Some(index) = self.attempt_index(id) {
            self.attempts.remove(index);
        }
        if self.form_session == Some(id) {
            if let Some(panel) = self.remote.as_mut() {
                panel.clear_secrets();
            }
            self.remote = None;
            self.chain = None;
            self.form_session = None;
            self.showing_connection_form = false;
            self.reconnect = None;
            self.pending_ephemeral = None;
            self.forward_only_dial = false;
            self.pending_forward_rules.clear();
        }
        self.workspace.session_failed(id, failure);
        // The tab shows the kept screen again, so the session is the one on screen.
        if self.workspace.layout().active_session() == Some(id) {
            self.active = Some(id);
        }
        cx.notify();
    }

    /// The banner of session `id`, and the handle of its first control, when it shows
    /// one (ADR-6).
    ///
    /// The banner comes in parts stacked in order: its line, which keeps its height,
    /// and the details it unfolded, which give way first in a short pane.
    pub(super) fn status_banner(
        &self,
        id: SessionId,
        cx: &mut Context<Self>,
    ) -> Option<(Vec<gpui::AnyElement>, FocusHandle)> {
        if self.reconnect_screen(id).is_some()
            && self
                .workspace
                .connection_wait(id)
                .is_some_and(|wait| !wait.due(cx))
        {
            return None;
        }
        if !self.banner_shown(id, cx) {
            return None;
        }
        let state = self.banner_state(id, cx)?;
        let (line, detail) = match &state {
            BannerState::Ended(status) => self.ended_banner(id, status, cx),
            BannerState::Reconnecting => (self.reconnecting_banner(id, cx), None),
        };
        let first = self.banner_stops(id, &state).into_iter().next()?;
        let line = match state {
            BannerState::Reconnecting => theme::enter(
                line,
                SharedString::from(format!("session.reconnect.enter.{id:?}")),
                theme::Enter::Rise,
            ),
            BannerState::Ended(_) => line.into_any_element(),
        };
        let mut parts = vec![line];
        parts.extend(detail);
        Some((parts, self.banner_focus[first.slot()].clone()))
    }

    /// The controls the banner of session `id` draws, in the keyboard's order.
    fn banner_stops(&self, id: SessionId, state: &BannerState) -> Vec<BannerStop> {
        let mut stops = Vec::with_capacity(4);
        match state {
            BannerState::Ended(status) => {
                if self.banner_next_step(id, status).is_some() {
                    stops.push(BannerStop::Next);
                }
                if status.diagnostic().is_some() {
                    stops.push(BannerStop::Details);
                    if self.diagnostics_open(id) {
                        stops.push(BannerStop::Copy);
                    }
                }
            }
            BannerState::Reconnecting => stops.push(BannerStop::Next),
        }
        stops.push(BannerStop::Dismiss);
        stops
    }

    /// The one step an ended banner offers: reconnect while another attempt can help
    /// and the attempt's own card is not on screen, close once nothing can.
    fn banner_next_step(&self, id: SessionId, status: &SessionStatus) -> Option<SessionStep> {
        match status.step() {
            Some(SessionStep::Reconnect)
                if self.workspace.session(id).is_some_and(may_reconnect)
                    && !self.connection_inline() =>
            {
                Some(SessionStep::Reconnect)
            }
            Some(SessionStep::Close) => Some(SessionStep::Close),
            _ => None,
        }
    }

    /// Moves the keyboard from banner control `from` to the next one (or the previous
    /// one with `back`); past either end it goes back to the terminal.
    fn step_banner_focus(
        &mut self,
        id: SessionId,
        from: BannerStop,
        back: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.banner_state(id, cx) else {
            self.banner_focus_terminal(id, window, cx);
            return;
        };
        let stops = self.banner_stops(id, &state);
        let at = stops.iter().position(|stop| *stop == from);
        let next = match (at, back) {
            (Some(at), false) => stops.get(at + 1).copied(),
            (Some(at), true) => at.checked_sub(1).and_then(|at| stops.get(at).copied()),
            (None, _) => stops.first().copied(),
        };
        match next {
            Some(stop) => window.focus(&self.banner_focus[stop.slot()]),
            None => self.banner_focus_terminal(id, window, cx),
        }
        cx.notify();
    }

    /// A banner control: pressed by the pointer, by Enter or Space while it holds the
    /// keyboard, and a stop of the banner's keyboard walk.
    fn banner_control(
        &self,
        id: SessionId,
        stop: BannerStop,
        button: Stateful<Div>,
        press: Press,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let ring = theme::swatch(palette::FOCUS_RING);
        let keyed = press.clone();
        let active_session = self.bar_session().or(self.active) == Some(id);
        button
            .when(active_session, |button| {
                button
                    .track_focus(&self.banner_focus[stop.slot()])
                    .focus(move |style| style.border_2().border_color(ring))
            })
            .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                cx.stop_propagation();
            })
            .on_key_down(cx.listener(move |view, event: &KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                let modifiers = &keystroke.modifiers;
                let bare = !modifiers.control && !modifiers.alt && !modifiers.platform;
                match keystroke.key.as_str() {
                    "enter" | "space" if bare && !modifiers.shift => {
                        cx.stop_propagation();
                        keyed(view, window, cx);
                    }
                    "tab" if bare => {
                        cx.stop_propagation();
                        view.step_banner_focus(id, stop, modifiers.shift, window, cx);
                    }
                    _ => {}
                }
            }))
            .on_click(cx.listener(move |view, _event, window, cx| {
                cx.stop_propagation();
                press(view, window, cx);
            }))
    }

    /// The sentence of a banner: one line cut with an ellipsis, the whole of it and the
    /// session's target in its tooltip.
    fn banner_sentence(
        &self,
        id: SessionId,
        selector: &'static str,
        sentence: String,
    ) -> Stateful<Div> {
        let target = self
            .workspace
            .session(id)
            .map(|session| self.target_of(session))
            .filter(|target| !target.is_empty());
        let tooltip = match target {
            Some(target) => format!("{sentence}\n{target}"),
            None => sentence.clone(),
        };
        div()
            .id(SharedString::from(format!("{selector}.{id:?}")))
            .debug_selector(move || selector.to_owned())
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .tooltip(components::tooltip(SharedString::from(tooltip)))
            .child(SharedString::from(sentence))
    }

    /// The banner's one line: its full height while the pane has room, and in a pane
    /// too short for it and the bar - a stacked split in a small window at 200% - the
    /// line gives way and is cut rather than pushing the bar out of the window; the
    /// bar keeps the session's Reconnect and menu.
    fn banner_line(frame: Div, metrics: theme::Metrics) -> Div {
        frame
            .h(metrics.large_control_height())
            .flex_shrink()
            .min_h_0()
            .overflow_hidden()
    }

    /// Whether the banner of session `id` draws its commands as icons: the terminal it
    /// stands over - or the window, when the files have its place - is narrower than
    /// [`BANNER_WORDS_WIDTH`] at the window's text size.
    fn banner_is_narrow(&self, id: SessionId, cx: &gpui::App) -> bool {
        let limit = self.metrics().scale(BANNER_WORDS_WIDTH);
        let files_cover = self.files.is_open(id) && !self.files_docked();
        let pane_width = self
            .pane_for(id)
            .or_else(|| self.reconnect_screen(id))
            .filter(|_| !files_cover)
            .and_then(|pane| pane.read(cx).bar_width());
        pane_width.unwrap_or(self.viewport_width) < limit
    }

    /// The banner's close: a square icon button with its name and Escape in its tooltip.
    fn banner_dismiss(
        &self,
        id: SessionId,
        selector: &'static str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let button = components::icon_only_button(Icon::Close, metrics)
            .size(metrics.hit_target())
            .flex_none()
            .id(SharedString::from(format!("{selector}.{id:?}")))
            .debug_selector(move || selector.to_owned())
            .tooltip(components::tooltip(copy::ux().banner_dismiss));
        self.banner_control(
            id,
            BannerStop::Dismiss,
            button,
            Rc::new(move |view, window, cx| view.dismiss_banner(id, window, cx)),
            cx,
        )
    }

    /// A multi-line text in the monospace face, each of its lines wrapped at the width it
    /// is given, that copies all of it on a click (ADR-6).
    fn copyable_lines(
        &self,
        id: SessionId,
        selector: &'static str,
        text: &str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let copied = text.to_owned();
        let hover = theme::swatch(palette::TEXT_PRIMARY);
        div()
            .id(SharedString::from(format!("{selector}.{id:?}")))
            .debug_selector(move || selector.to_owned())
            // Its whole height inside the box that scrolls it.
            .flex()
            .flex_col()
            .flex_none()
            .min_w_0()
            .font_family(terminal::monospace_family())
            .text_size(metrics.caption_size())
            .text_color(theme::swatch(palette::TEXT_SECONDARY))
            .cursor_pointer()
            .hover(move |style| style.text_color(hover))
            .tooltip(components::tooltip(copy::ux().copy_click_hint))
            .on_click(cx.listener(move |view, _event, _window, cx| {
                cx.stop_propagation();
                view.copy_text(copied.clone(), cx);
            }))
            .children(text.lines().map(|line| {
                div()
                    .min_w_0()
                    .whitespace_normal()
                    .child(SharedString::from(line.to_owned()))
            }))
    }

    /// The banner of a session that is over (ADR-6): its line, and the details when
    /// they are unfolded.
    fn ended_banner(
        &self,
        id: SessionId,
        status: &SessionStatus,
        cx: &mut Context<Self>,
    ) -> (Div, Option<gpui::AnyElement>) {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let locale = self.workspace.locale();
        // A process that ended with status 0 keeps the neutral tone: the reason is still
        // stated, but a normal exit is not an error.
        let tone = status.tone();
        let headline = status.headline(locale);
        // "Disconnected · keepalive timed out": the headline and the first line of the
        // reason; the whole reason is in the details.
        let reason = status
            .diagnostic()
            .and_then(|reason| reason.lines().map(str::trim).find(|line| !line.is_empty()))
            .map(str::to_owned);
        let sentence = match reason {
            Some(reason) => format!("{headline} \u{00B7} {reason}"),
            None => headline,
        };
        let icon = if tone == Tone::Danger {
            Icon::Plug
        } else {
            Icon::Terminal
        };
        let open = self.diagnostics_open(id);
        let narrow = self.banner_is_narrow(id, cx);
        let mut row = Self::banner_line(
            components::inline_banner_frame(tone, icon, metrics),
            metrics,
        )
        .debug_selector(|| "terminal.failure".to_owned())
        .child(self.banner_sentence(id, "terminal.failure.headline", sentence));
        match self.banner_next_step(id, status) {
            Some(SessionStep::Reconnect) => {
                let tooltip = match self.keymap.label("session.reconnect") {
                    Some(shortcut) => format!("{} ({shortcut})", strings.session_reconnect),
                    None => strings.session_reconnect.to_owned(),
                };
                let button = if narrow {
                    components::primary_icon_button(Icon::Refresh, metrics)
                } else {
                    components::icon_label_button(
                        Icon::Refresh,
                        strings.session_reconnect,
                        Variant::Primary,
                        metrics,
                    )
                    .h(metrics.compact_control_height())
                };
                let button = button
                    .flex_none()
                    .id(SharedString::from(format!("terminal.failure.next.{id:?}")))
                    .debug_selector(|| "terminal.failure.next".to_owned())
                    .tooltip(components::tooltip(SharedString::from(tooltip)));
                row = row.child(self.banner_control(
                    id,
                    BannerStop::Next,
                    button,
                    Rc::new(move |view, _window, cx| view.reconnect_session(id, cx)),
                    cx,
                ));
            }
            Some(SessionStep::Close) => {
                let button = if narrow {
                    components::icon_only_button(Icon::Close, metrics).size(metrics.hit_target())
                } else {
                    components::compact_button(strings.session_close, Variant::Secondary, metrics)
                }
                .flex_none()
                .id(SharedString::from(format!("terminal.failure.next.{id:?}")))
                .debug_selector(|| "terminal.failure.next".to_owned())
                .tooltip(components::tooltip(strings.session_close));
                row = row.child(self.banner_control(
                    id,
                    BannerStop::Next,
                    button,
                    Rc::new(move |view, _window, cx| view.request_close_session(id, cx)),
                    cx,
                ));
            }
            _ => {}
        }
        if status.diagnostic().is_some() {
            let label = if open {
                strings.session_details_hide
            } else {
                strings.session_details_show
            };
            let button = if narrow {
                components::icon_only_button(
                    if open {
                        Icon::ChevronUp
                    } else {
                        Icon::ChevronDown
                    },
                    metrics,
                )
                .size(metrics.hit_target())
            } else {
                components::compact_button(label, Variant::Ghost, metrics)
            };
            let button = button
                .flex_none()
                .id(SharedString::from(format!(
                    "terminal.failure.details.{id:?}"
                )))
                .debug_selector(|| "terminal.failure.details".to_owned())
                .tooltip(components::tooltip(label));
            row = row.child(self.banner_control(
                id,
                BannerStop::Details,
                button,
                Rc::new(move |view, window, cx| {
                    view.toggle_diagnostic(id, cx);
                    // A long diagnostic is laid out in the box that scrolls it over two
                    // frames: the second is asked for here rather than left to the next
                    // input that happens to redraw the window.
                    let view = cx.entity().downgrade();
                    window.on_next_frame(move |_window, app| {
                        let _ = view.update(app, |_view, cx| cx.notify());
                    });
                }),
                cx,
            ));
        }
        row = row.child(self.banner_dismiss(id, "terminal.failure.dismiss", cx));

        let mut unfolded = None;
        if open && let Some(reason) = status.diagnostic() {
            let copied = reason.to_owned();
            let copy_button = components::icon_label_button(
                Icon::Copy,
                ux.banner_copy_diagnostic,
                Variant::Ghost,
                metrics,
            )
            .h(metrics.compact_control_height())
            .flex_none()
            .id(SharedString::from(format!("terminal.failure.copy.{id:?}")))
            .debug_selector(|| "terminal.failure.copy".to_owned());
            let copy_button = self.banner_control(
                id,
                BannerStop::Copy,
                copy_button,
                Rc::new(move |view, _window, cx| view.copy_text(copied.clone(), cx)),
                cx,
            );
            let detail = div()
                .id(SharedString::from(format!(
                    "terminal.failure.detail.{id:?}"
                )))
                .debug_selector(|| "terminal.failure.detail".to_owned())
                .flex()
                .flex_col()
                .flex_shrink()
                .min_h_0()
                .w_full()
                .max_h(metrics.scale(DIAGNOSTIC_MAX_HEIGHT))
                .overflow_y_scroll()
                .gap(metrics.scale(spacing::HALF))
                .px(metrics.scale(spacing::SNUG))
                .py(metrics.scale(spacing::NARROW))
                .bg(theme::swatch(palette::SURFACE))
                .border_b_1()
                .border_color(theme::swatch(palette::BORDER))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(metrics.scale(spacing::TIGHT))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED))
                                .child(ux.banner_diagnostic),
                        )
                        .child(copy_button),
                )
                // The raw reason, copied by one click and never carrying a secret: it is
                // the layer's own wording, which is what a bug report needs.
                .child(
                    // One block per line of the reason, each wrapped at the box's width;
                    // a click anywhere on it copies the whole reason.
                    self.copyable_lines(id, "terminal.failure.diagnostic", reason, cx),
                );
            // The box keeps to a share of the pane as well as to its own most: in a
            // short pane the details give way before the last screen does.
            let room = div()
                .flex()
                .flex_col()
                .flex_shrink()
                .min_h_0()
                .w_full()
                .max_h(gpui::relative(DIAGNOSTIC_MAX_SHARE))
                .debug_selector(|| "terminal.failure.room".to_owned())
                .child(detail);
            unfolded = Some(theme::enter(
                room,
                SharedString::from(format!("session.banner.detail.{id:?}")),
                theme::Enter::Rise,
            ));
        }
        (row, unfolded)
    }

    /// The banner over the last screen of a session that is reconnecting (ADR-6): the
    /// target it dials, a bar that says it is working, and Cancel.
    fn reconnecting_banner(&self, id: SessionId, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let target = self
            .workspace
            .session(id)
            .map(|session| self.target_of(session))
            .unwrap_or_default();
        let sentence = copy::fill(ux.banner_reconnecting, &[("target", &target)]);
        let narrow = self.banner_is_narrow(id, cx);
        let cancel = if narrow {
            components::icon_only_button(Icon::Stop, metrics).size(metrics.hit_target())
        } else {
            components::compact_button(ux.banner_cancel, Variant::Secondary, metrics)
        }
        .flex_none()
        .id(SharedString::from(format!(
            "terminal.reconnecting.cancel.{id:?}"
        )))
        .debug_selector(|| "terminal.reconnecting.cancel".to_owned())
        .tooltip(components::tooltip(ux.banner_cancel));
        let row = Self::banner_line(
            components::inline_banner_frame(Tone::Warning, Icon::Refresh, metrics),
            metrics,
        )
        .debug_selector(|| "terminal.banner".to_owned())
        .child(self.banner_sentence(id, "terminal.reconnecting.headline", sentence))
        .child(self.banner_control(
            id,
            BannerStop::Next,
            cancel,
            Rc::new(move |view, _window, cx| view.cancel_reconnect(id, cx)),
            cx,
        ))
        .child(self.banner_dismiss(id, "terminal.reconnecting.dismiss", cx));
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .debug_selector(|| "terminal.reconnecting".to_owned())
            .child(row)
            .child(components::progress_bar(
                SharedString::from(format!("session.banner.progress.{id:?}")),
                Progress::Indeterminate,
                metrics,
            ))
    }

    /// The banner of session `id` handed to `pane`, when it shows one.
    pub(super) fn hand_banner(
        &self,
        id: SessionId,
        pane: &Entity<TerminalPane>,
        cx: &mut Context<Self>,
    ) {
        if let Some((banner, focus)) = self.status_banner(id, cx) {
            let active_session = self.bar_session().or(self.active) == Some(id);
            pane.read(cx)
                .set_status_banner(banner, active_session.then_some(focus));
        }
    }

    /// Whether the banner of session `id` is on screen, for a test.
    #[cfg(test)]
    pub fn banner_shown_for_test(&self, id: SessionId, cx: &gpui::App) -> bool {
        self.banner_shown(id, cx)
    }

    /// Which of the banner's controls holds the keyboard - 0 the next step, 1 the
    /// details, 2 the copy, 3 the close - for a test.
    #[cfg(test)]
    pub fn banner_focus_for_test(&self, window: &Window) -> Option<usize> {
        self.banner_focus
            .iter()
            .position(|handle| handle.is_focused(window))
    }

    /// Stops the reconnect of session `id`, the way its banner's Cancel does, for a test.
    #[cfg(test)]
    pub fn cancel_reconnect_for_test(&mut self, id: SessionId, cx: &mut Context<Self>) {
        self.cancel_reconnect(id, cx);
    }

    /// Whether the last screen of session `id` is kept for a reconnect, for a test.
    #[cfg(test)]
    pub fn keeps_reconnect_screen_for_test(&self, id: SessionId) -> bool {
        self.reconnect_screens.contains_key(&id)
    }

    /// Forgets the kept last screen of session `id`, for a test about the card of a
    /// session that has nothing to show.
    #[cfg(test)]
    pub fn forget_reconnect_screen_for_test(&mut self, id: SessionId) {
        self.forget_reconnect_screen(id);
    }
}
