//! The Shellune shell view: navigation rail, header, destination panel, status bar.
//!
//! The composition follows the navigation model in [`crate::app::nav`] and the
//! state model in [`crate::app::workspace`]. Render functions never decide whether a
//! capability exists: they read [`SectionState`] and draw what it reports, which is
//! what keeps a planned or failed section from looking finished.
//!
//! Layout, in one sentence: a fixed navigation rail on the left, a header naming
//! the current object, a scrollable destination panel, and a status bar that
//! carries the product tagline, the counts that are actually non-zero, and the
//! text-size control.

use gpui::prelude::*;
use gpui::{
    Context, Div, FocusHandle, FontWeight, IntoElement, Render, SharedString, Stateful, Window,
    div, px,
};
use shellune_core::{HostConfig, Session, SessionState, Transfer};

use crate::app::actions::{
    CycleTextSize, DismissNotice, GoToHosts, GoToLocalShells, GoToSessions, GoToTransfers,
    NextDestination, PreviousDestination, ToggleLanguage,
};
use crate::app::copy::{self, Locale, Strings};
use crate::app::nav::{Destination, Platform};
use crate::app::product;
use crate::app::theme::{self, Metrics, Tone, palette, spacing};
use crate::app::workspace::{PrimaryOutcome, SectionState, Workspace};
use crate::app::{components, theme::radius};

/// The application shell view.
pub struct ShelluneView {
    workspace: Workspace,
    root_focus: FocusHandle,
    destination_focus: Vec<FocusHandle>,
}

impl ShelluneView {
    /// Builds the shell in its initial state: every capability still unimplemented.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::with_workspace(cx, Workspace::new())
    }

    /// Builds the shell around an existing workspace, used by tests and by later
    /// tasks that restore state before the first frame.
    pub fn with_workspace(cx: &mut Context<Self>, workspace: Workspace) -> Self {
        Self {
            workspace,
            root_focus: cx.focus_handle(),
            destination_focus: Destination::ALL.iter().map(|_| cx.focus_handle()).collect(),
        }
    }

    /// The current workspace state.
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Focus handle of the shell root, which owns the key context.
    pub fn root_focus(&self) -> &FocusHandle {
        &self.root_focus
    }

    /// Focus handle of one navigation entry.
    pub fn destination_focus(&self, destination: Destination) -> &FocusHandle {
        &self.destination_focus[destination.index()]
    }

    fn strings(&self) -> &'static Strings {
        self.workspace.strings()
    }

    fn metrics(&self) -> Metrics {
        self.workspace.metrics()
    }

    fn primary_label(&self, destination: Destination, strings: &Strings) -> &'static str {
        match destination {
            Destination::LocalShells => strings.primary_local,
            Destination::Hosts => strings.primary_hosts,
            Destination::Sessions => strings.primary_sessions,
            Destination::Transfers => strings.primary_transfers,
        }
    }

    /// Selects a destination, moving keyboard focus with it.
    fn select(&mut self, destination: Destination, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.select(destination);
        let handle = self.destination_focus(destination).clone();
        window.focus(&handle);
        cx.notify();
    }

    fn request_primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.request_primary() == PrimaryOutcome::Unavailable {
            // Keep focus inside the shell so keyboard users are not dropped.
            let handle = self.root_focus.clone();
            window.focus(&handle);
        }
        cx.notify();
    }

    fn brand(&self) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE))
            .px(metrics.scale(spacing::SNUG))
            .py(metrics.scale(spacing::BASE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .text_size(metrics.display_size())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(product::NAME),
                    )
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(format!(
                                "{} · {}",
                                product::PRONUNCIATION,
                                strings.workspace
                            ))),
                    ),
            )
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(product::TAGLINE_ZH),
            )
    }

    fn nav_entry(
        &self,
        destination: Destination,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let state = self.workspace.state(destination);
        let tone = state.tone();
        let selected = self.workspace.destination() == destination;
        let focused = self.destination_focus(destination).is_focused(window);

        let background = if selected {
            palette::SURFACE_SELECTED
        } else {
            palette::SURFACE
        };
        let label_color = if selected {
            palette::TEXT_PRIMARY
        } else {
            palette::TEXT_SECONDARY
        };
        let border = if focused {
            palette::ACCENT
        } else if selected {
            palette::BORDER_STRONG
        } else {
            palette::SURFACE
        };

        let index = destination.index();
        let count = self.workspace.item_count(destination);

        div()
            .id(("nav", index))
            .flex()
            .items_center()
            .justify_between()
            .gap(metrics.scale(spacing::TIGHT))
            .h(metrics.nav_row_height())
            .px(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(background))
            .border_1()
            .border_color(theme::swatch(border))
            .text_size(metrics.body_size())
            .text_color(theme::swatch(label_color))
            .track_focus(self.destination_focus(destination))
            .debug_selector(move || destination.debug_selector().to_owned())
            .on_click(cx.listener(move |this, _event, window, cx| {
                this.select(Destination::ALL[index], window, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .min_w(px(0.0))
                    // The accent bar marks the current object; it is drawn in the
                    // row colour when the entry is not selected, so the row does
                    // not shift when the selection moves.
                    .child(
                        div()
                            .w(metrics.scale(2.0))
                            .h(metrics.scale(16.0))
                            .rounded(metrics.scale(1.0))
                            .bg(theme::swatch(if selected {
                                palette::ACCENT
                            } else {
                                background
                            })),
                    )
                    .child(components::status_dot(tone, metrics))
                    .child(
                        div()
                            .truncate()
                            .font_weight(if selected {
                                FontWeight::MEDIUM
                            } else {
                                FontWeight::NORMAL
                            })
                            .child(destination.label(strings)),
                    ),
            )
            .child(if count > 0 {
                components::count_label(count, strings.count_sessions, metrics).into_any_element()
            } else {
                components::keycap(destination.shortcut_label(Platform::current()), metrics)
                    .into_any_element()
            })
    }

    fn rail(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let mut rail = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(metrics.rail_width())
            .h_full()
            .bg(theme::swatch(palette::SURFACE))
            .border_r_1()
            .border_color(theme::swatch(palette::BORDER))
            .child(self.brand())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(2.0))
                    .px(metrics.scale(spacing::TIGHT)),
            );

        for destination in Destination::ALL {
            rail = rail.child(self.nav_entry(destination, window, cx));
        }

        rail.child(
            div()
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::HAIRLINE))
                .p(metrics.scale(spacing::TIGHT))
                .mt_auto()
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .child(strings.hint_nav)
                .child(
                    div()
                        .id("zoom.control")
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(metrics.scale(spacing::TIGHT))
                        .px(metrics.scale(spacing::TIGHT))
                        .py(metrics.scale(spacing::HAIRLINE))
                        .rounded(px(radius::SMALL))
                        .border_1()
                        .border_color(theme::swatch(palette::BORDER))
                        .debug_selector(|| "zoom.control".to_owned())
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.workspace.cycle_text_size();
                            cx.notify();
                        }))
                        .child(strings.zoom_control)
                        .child(
                            div()
                                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                .child(self.workspace.zoom().label()),
                        ),
                ),
        )
    }

    fn notice_strip(&self, cx: &mut Context<Self>) -> Option<Div> {
        let notice = self.workspace.notice()?;
        let metrics = self.metrics();
        let strings = self.strings();
        Some(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(metrics.scale(spacing::TIGHT))
                .px(metrics.scale(spacing::BASE))
                .py(metrics.scale(spacing::TIGHT))
                .bg(theme::swatch(palette::SURFACE_RAISED))
                .border_b_1()
                .border_color(theme::swatch(palette::BORDER))
                .text_size(metrics.label_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .debug_selector(|| "notice".to_owned())
                .child(notice.text(strings))
                .child(
                    div()
                        .id("notice.dismiss")
                        .px(metrics.scale(spacing::TIGHT))
                        .py(metrics.scale(2.0))
                        .rounded(px(radius::SMALL))
                        .text_color(Tone::Accent.text())
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.workspace.dismiss_notice();
                            cx.notify();
                        }))
                        .child("✕"),
                ),
        )
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let destination = self.workspace().destination();
        let state = self.workspace().state(destination);
        let mut chips = div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .child(components::chip(
                state.chip_label(strings).unwrap_or(strings.count_sessions),
                state.tone(),
                metrics,
            ));
        if state == SectionState::Populated {
            chips = chips.child(components::count_label(
                self.workspace.item_count(destination),
                match destination {
                    Destination::Hosts => strings.count_hosts,
                    Destination::Transfers => strings.count_transfers,
                    Destination::LocalShells | Destination::Sessions => strings.count_sessions,
                },
                metrics,
            ));
        }

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(metrics.scale(spacing::BASE))
            .h(metrics.header_height())
            .flex_none()
            .px(metrics.scale(spacing::BASE))
            .bg(theme::swatch(palette::CANVAS))
            .border_b_1()
            .border_color(theme::swatch(palette::BORDER))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(1.0))
                    .min_w(px(0.0))
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(format!(
                                "{} · {}",
                                strings.object_label,
                                destination.label(strings)
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(metrics.scale(spacing::SNUG))
                            .child(
                                div()
                                    .text_size(metrics.heading_size())
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                    .truncate()
                                    .debug_selector(|| "header.title".to_owned())
                                    .child(destination.label(strings)),
                            )
                            .child(chips),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(destination.summary(strings)),
                    )
                    .child(
                        div()
                            .id("header.primary")
                            .debug_selector(|| "header.primary".to_owned())
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.request_primary(window, cx);
                            }))
                            .child(components::action_button(
                                self.primary_label(destination, strings),
                                self.workspace.can_act(destination),
                                metrics,
                            )),
                    ),
            )
    }

    fn destination_panel(&self) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let destination = self.workspace.destination();
        let state = self.workspace.state(destination);

        let card = match state {
            SectionState::Planned => components::state_card(
                Tone::Planned,
                strings.planned_title,
                match destination {
                    Destination::LocalShells => strings.planned_local,
                    Destination::Hosts => strings.planned_hosts,
                    Destination::Sessions => strings.planned_sessions,
                    Destination::Transfers => strings.planned_transfers,
                },
                None,
                metrics,
            ),
            SectionState::Loading => components::state_card(
                Tone::Warning,
                strings.loading_title,
                strings.loading_body,
                None,
                metrics,
            ),
            SectionState::Failed => {
                let failure = self.workspace.failure(destination);
                components::state_card(
                    Tone::Danger,
                    strings.failed_title,
                    failure.map_or_else(
                        || strings.loading_body.to_owned(),
                        |failure| failure.reason().to_owned(),
                    ),
                    Some(SharedString::from(format!(
                        "{} · {}",
                        strings.recovery_label,
                        failure.map_or(strings.recovery_retry, |failure| failure
                            .recovery()
                            .label(strings))
                    ))),
                    metrics,
                )
            }
            SectionState::Empty => components::state_card(
                Tone::Neutral,
                match destination {
                    Destination::LocalShells => strings.empty_local_title,
                    Destination::Hosts => strings.empty_hosts_title,
                    Destination::Sessions => strings.empty_sessions_title,
                    Destination::Transfers => strings.empty_transfers_title,
                },
                match destination {
                    Destination::LocalShells => strings.empty_local_body,
                    Destination::Hosts => strings.empty_hosts_body,
                    Destination::Sessions => strings.empty_sessions_body,
                    Destination::Transfers => strings.empty_transfers_body,
                },
                Some(SharedString::from(format!(
                    "{} · {}",
                    strings.next_step,
                    self.primary_label(destination, strings)
                ))),
                metrics,
            ),
            SectionState::Populated => div().flex().flex_col().gap(metrics.scale(spacing::TIGHT)),
        };

        let card = match state {
            SectionState::Populated => match destination {
                Destination::LocalShells => self.local_rows(card),
                Destination::Hosts => self.host_rows(card),
                Destination::Sessions => self.session_rows(card),
                Destination::Transfers => self.transfer_rows(card),
            },
            _ => card,
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .id("content.scroll")
            .overflow_y_scroll()
            .p(metrics.scale(spacing::SECTION))
            .gap(metrics.panel_gap())
            .debug_selector(|| "content".to_owned())
            .child(card)
    }

    fn local_rows(&self, list: Div) -> Div {
        let sessions: Vec<&Session> = self.workspace.local_sessions().collect();
        self.session_rows_from(list, &sessions)
    }

    fn session_rows(&self, list: Div) -> Div {
        let sessions: Vec<&Session> = self.workspace.sessions().items().iter().collect();
        self.session_rows_from(list, &sessions)
    }

    fn session_rows_from(&self, mut list: Div, sessions: &[&Session]) -> Div {
        for (index, session) in sessions.iter().enumerate() {
            list = list.child(
                div()
                    .debug_selector(move || format!("row.{index}"))
                    .child(self.session_row(session)),
            );
        }
        list
    }

    fn host_rows(&self, mut list: Div) -> Div {
        for (index, host) in self.workspace.hosts().items().iter().enumerate() {
            list = list.child(
                div()
                    .debug_selector(move || format!("row.{index}"))
                    .child(self.host_row(host)),
            );
        }
        list
    }

    fn transfer_rows(&self, mut list: Div) -> Div {
        for (index, transfer) in self.workspace.transfers().items().iter().enumerate() {
            list = list.child(
                div()
                    .debug_selector(move || format!("row.{index}"))
                    .child(self.transfer_row(transfer)),
            );
        }
        list
    }

    fn session_row(&self, session: &Session) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let locale = self.workspace.locale();
        let tone = Tone::for_session_state(session.state());
        let kind = theme::session_kind_label(session.kind(), strings.local_shell_fallback);
        // A failed or lost session shows the reason the transport reported instead
        // of a generic "disconnected", so the user can act on it.
        let detail = match session.state() {
            SessionState::Disconnected(failure) | SessionState::Failed(failure) => {
                format!("{kind} · {}", theme::session_failure_text(failure))
            }
            state => format!("{kind} · {}", copy::session_state_label(state, locale)),
        };
        components::row(metrics)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .min_w(px(0.0))
                    .child(components::status_dot(tone, metrics))
                    .child(components::stacked(
                        session.title().to_owned(),
                        detail,
                        metrics,
                    )),
            )
            .child(components::chip(
                copy::session_state_label(session.state(), locale),
                tone,
                metrics,
            ))
    }

    fn host_row(&self, host: &HostConfig) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        components::row(metrics)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .min_w(px(0.0))
                    .child(components::status_dot(Tone::Neutral, metrics))
                    .child(components::stacked(
                        host.name().to_owned(),
                        host.target(),
                        metrics,
                    )),
            )
            .child(components::chip(
                host.group().unwrap_or(strings.count_hosts).to_owned(),
                Tone::Neutral,
                metrics,
            ))
    }

    fn transfer_row(&self, transfer: &Transfer) -> Div {
        let metrics = self.metrics();
        let locale = self.workspace.locale();
        let tone = Tone::for_transfer_state(transfer.state());
        components::row(metrics)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .min_w(px(0.0))
                    .child(components::status_dot(tone, metrics))
                    .child(components::stacked(
                        format!(
                            "{} {}",
                            theme::transfer_direction_label(transfer.direction()),
                            transfer.source()
                        ),
                        format!(
                            "→ {} · {}",
                            transfer.destination(),
                            copy::transfer_state_label(transfer.state(), locale)
                        ),
                        metrics,
                    )),
            )
            .child(components::chip(
                copy::transfer_state_label(transfer.state(), locale),
                tone,
                metrics,
            ))
    }

    fn status_bar(&self) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let locale: Locale = self.workspace.locale();
        let hosts = self.workspace.hosts().count();
        let sessions = self.workspace.sessions().count();
        let transfers = self.workspace.transfers().count();
        let counts = if hosts + sessions + transfers == 0 {
            strings.nothing_running.to_owned()
        } else {
            format!(
                "{hosts} {hosts_unit} · {sessions} {sessions_unit} · {transfers} {transfers_unit}",
                hosts_unit = strings.count_hosts,
                sessions_unit = strings.count_sessions,
                transfers_unit = strings.count_transfers
            )
        };

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(metrics.scale(spacing::BASE))
            .h(metrics.status_bar_height())
            .flex_none()
            .px(metrics.scale(spacing::BASE))
            .border_t_1()
            .border_color(theme::swatch(palette::BORDER))
            .text_size(metrics.caption_size())
            .text_color(theme::swatch(palette::TEXT_MUTED))
            .child(SharedString::from(format!(
                "{} {} · {} {}",
                product::NAME,
                self.version(),
                strings.language_control,
                locale.endonym()
            )))
            .child(
                div()
                    .truncate()
                    .debug_selector(|| "status.tagline".to_owned())
                    .child(product::TAGLINE_EN),
            )
            .child(SharedString::from(counts))
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
}

impl Render for ShelluneView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = self.metrics();
        let notice = self.notice_strip(cx);

        div()
            .id("shellune.root")
            .flex()
            .size_full()
            .bg(theme::swatch(palette::CANVAS))
            .text_size(metrics.body_size())
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .key_context("Shellune")
            .track_focus(&self.root_focus)
            .on_action(cx.listener(|this, _: &GoToLocalShells, window, cx| {
                this.select(Destination::LocalShells, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToHosts, window, cx| {
                this.select(Destination::Hosts, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToSessions, window, cx| {
                this.select(Destination::Sessions, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToTransfers, window, cx| {
                this.select(Destination::Transfers, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextDestination, window, cx| {
                this.workspace.select_next();
                let next = this.workspace.destination();
                this.select(next, window, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousDestination, window, cx| {
                this.workspace.select_previous();
                let previous = this.workspace.destination();
                this.select(previous, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CycleTextSize, _window, cx| {
                this.workspace.cycle_text_size();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleLanguage, _window, cx| {
                this.workspace.toggle_locale();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &DismissNotice, _window, cx| {
                this.workspace.dismiss_notice();
                cx.notify();
            }))
            .child(self.rail(window, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .child(self.header(cx))
                    .children(notice)
                    .child(self.destination_panel())
                    .child(self.status_bar()),
            )
    }
}
