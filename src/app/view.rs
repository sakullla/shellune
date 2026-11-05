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
    Context, Div, Entity, FocusHandle, FontWeight, IntoElement, Render, SharedString, Stateful,
    Window, div, px,
};
use shellune_core::{HostConfig, LocalShell, Session, SessionId, SessionState, Transfer};
use shellune_terminal::GridSize;

use crate::app::actions::{
    CycleTextSize, DismissNotice, GoToHosts, GoToLocalShells, GoToSessions, GoToTransfers,
    NextDestination, PreviousDestination, ToggleLanguage,
};
use crate::app::copy::{self, Locale, Strings};
use crate::app::nav::{Destination, Platform};
use crate::app::product;
use crate::app::terminal::{PaneState, TerminalPane};
use crate::app::text_field::TextField;
use crate::app::theme::{self, Metrics, Tone, palette, spacing};
use crate::app::workspace::{Failure, PrimaryOutcome, Recovery, SectionState, Workspace};
use crate::app::{components, theme::radius};

/// The application shell view.
pub struct ShelluneView {
    workspace: Workspace,
    root_focus: FocusHandle,
    destination_focus: Vec<FocusHandle>,
    /// The one local terminal this step delivers; multi-session arrives later.
    pane: Option<Entity<TerminalPane>>,
    pane_session: Option<SessionId>,
    shell_path: Entity<TextField>,
    custom_shell_open: bool,
}

impl ShelluneView {
    /// Builds the shell in its initial state: every capability still unimplemented.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::with_workspace(cx, Workspace::new())
    }

    /// Builds the shell around an existing workspace, used by tests and by later
    /// tasks that restore state before the first frame.
    pub fn with_workspace(cx: &mut Context<Self>, workspace: Workspace) -> Self {
        let placeholder = workspace.strings().shell_custom_placeholder;
        let shell_path = cx.new(|cx| TextField::new(placeholder, cx));
        let mut view = Self {
            workspace,
            root_focus: cx.focus_handle(),
            destination_focus: Destination::ALL.iter().map(|_| cx.focus_handle()).collect(),
            pane: None,
            pane_session: None,
            shell_path,
            custom_shell_open: false,
        };
        view.bind_shell_path(cx);
        view
    }

    /// The terminal currently shown, if one was started.
    #[cfg(test)]
    pub fn pane(&self) -> Option<&Entity<TerminalPane>> {
        self.pane.as_ref()
    }

    /// Whether the free-text shell path field is on screen.
    #[cfg(test)]
    pub fn custom_shell_open(&self) -> bool {
        self.custom_shell_open
    }

    /// What the free-text shell path field holds.
    #[cfg(test)]
    pub fn shell_path_text(&self, cx: &gpui::App) -> String {
        self.shell_path.read(cx).text().to_owned()
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
        if self.workspace.destination() == Destination::LocalShells {
            self.start_from_picker(window, cx);
            return;
        }
        if self.workspace.request_primary() == PrimaryOutcome::Unavailable {
            // Keep focus inside the shell so keyboard users are not dropped.
            let handle = self.root_focus.clone();
            window.focus(&handle);
        }
        cx.notify();
    }

    /// Connects the free-text shell field to this view, so Enter starts a shell.
    fn bind_shell_path(&mut self, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        self.shell_path.update(cx, |field, _cx| {
            field.on_submit(move |text, window, app| {
                view.update(app, |view, cx| view.start_custom_shell(text, window, cx))
                    .unwrap_or(false)
            });
        });
    }

    /// Starts the shell named in the free-text field.
    ///
    /// The text is taken as written: a wrong path becomes a real launch failure,
    /// which is the only honest answer to a program that does not exist.
    fn start_custom_shell(
        &mut self,
        program: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let program = program.trim().to_owned();
        if program.is_empty() {
            return false;
        }
        let shell = LocalShell::new(program.clone(), program);
        self.start_shell(shell, Some(window), cx);
        if self.pane.is_some() {
            // The shell that started owns the keyboard, so what the user types next
            // goes to it. The field closes and, because this returns true, empties:
            // a second Return over the same path would start the shell again
            // instead of reaching it.
            self.custom_shell_open = false;
            cx.notify();
        }
        self.pane.is_some()
    }

    /// Starts whichever shell the picker currently points at.
    fn start_from_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = if self.custom_shell_open {
            self.shell_path.read(cx).text().trim().to_owned()
        } else {
            String::new()
        };
        let shell = if typed.is_empty() {
            match self.workspace.selected_shell() {
                Some(shell) => shell.clone(),
                None => {
                    let reason = self.strings().shell_none_found.to_owned();
                    self.workspace
                        .fail_local_start(Failure::new(reason, Recovery::ChooseShell));
                    cx.notify();
                    return;
                }
            }
        } else {
            LocalShell::new(typed.clone(), typed)
        };
        self.start_shell(shell, Some(window), cx);
    }

    /// Starts a shell and, when it really started, records the session.
    ///
    /// A launch failure replaces the section with the reason and keeps no pane, so
    /// the destination cannot show a terminal that has no process behind it.
    fn start_shell(
        &mut self,
        shell: LocalShell,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.pane_session {
            self.workspace.session_closed(session);
        }
        self.pane = None;
        self.pane_session = None;

        let grid = GridSize::initial();
        let font_size = self.workspace.local_font_size();
        let locale = self.workspace.locale();
        let pane =
            cx.new(|pane_cx| TerminalPane::start(shell.clone(), grid, font_size, locale, pane_cx));
        let state = pane.read(cx).state().clone();
        if !state.accepts_input() {
            let reason = state
                .failure()
                .map_or_else(|| state.label(locale), |failure| failure.to_string());
            self.workspace
                .fail_local_start(Failure::new(reason, Recovery::ChooseShell));
            cx.notify();
            return;
        }

        let session = self.workspace.register_local_session(&shell);
        let id = session.id();
        self.workspace.session_connected(id);
        // The size the pane actually uses is the one the next session starts with.
        self.workspace
            .set_local_font_size(pane.read(cx).font_size());
        cx.observe(&pane, |view, pane, cx| view.sync_pane(pane, cx))
            .detach();
        if let Some(window) = window {
            let handle = pane.read(cx).focus_handle().clone();
            window.focus(&handle);
        }
        self.pane = Some(pane);
        self.pane_session = Some(id);
        cx.notify();
    }

    /// Mirrors what the pane reports into the session list.
    ///
    /// The pane owns the truth about the process; the list only repeats it, so a
    /// shell that exited cannot stay listed as running.
    fn sync_pane(&mut self, pane: Entity<TerminalPane>, cx: &mut Context<Self>) {
        let Some(session) = self.pane_session else {
            return;
        };
        let state = pane.read(cx).state().clone();
        match state {
            PaneState::Live => {
                self.workspace.session_connected(session);
            }
            PaneState::Exited { report } => {
                // A shell that exits on its own is still over. Reporting the status it
                // exited with is the difference between "the session ended" and a
                // session list that keeps claiming a process that is gone.
                let failure = report
                    .session_failure()
                    .unwrap_or(shellune_core::SessionFailure::ProcessExited { code: Some(0) });
                self.workspace.session_failed(session, failure);
            }
            PaneState::Failed(failure) => {
                self.workspace.session_failed(session, failure);
            }
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
                components::count_label(count, destination.count_unit(strings), metrics)
                    .into_any_element()
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
                .child(SharedString::from(copy::nav_hint(
                    strings,
                    Platform::current(),
                )))
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
                state.chip_label(strings),
                state.tone(),
                metrics,
            ));
        if state == SectionState::Populated {
            chips = chips.child(components::count_label(
                self.workspace.item_count(destination),
                destination.count_unit(strings),
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

    /// The card a section shows when it holds no rows.
    fn section_card(&self, destination: Destination, state: SectionState) -> gpui::Div {
        let metrics = self.metrics();
        let strings = self.strings();
        match state {
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
        }
    }

    fn rows_for(&self, destination: Destination, list: gpui::Div) -> gpui::Div {
        match destination {
            Destination::LocalShells => self.local_rows(list),
            Destination::Hosts => self.host_rows(list),
            Destination::Sessions => self.session_rows(list),
            Destination::Transfers => self.transfer_rows(list),
        }
    }

    fn destination_panel(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let destination = self.workspace.destination();
        let state = self.workspace.state(destination);

        let body = if destination == Destination::LocalShells && self.pane.is_some() {
            self.terminal_panel(cx)
        } else {
            let card = self.section_card(destination, state);
            if state == SectionState::Populated {
                self.rows_for(destination, card)
            } else if destination == Destination::LocalShells {
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.panel_gap())
                    .child(self.shell_picker(cx))
                    .child(card)
            } else {
                card
            }
        };

        let container = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .id("content.scroll")
            .p(metrics.scale(spacing::SECTION))
            .gap(metrics.panel_gap())
            .debug_selector(|| "content".to_owned())
            .child(body);
        // A terminal fills the panel; a list of cards scrolls instead.
        if destination == Destination::LocalShells && self.pane.is_some() {
            container
        } else {
            container.overflow_y_scroll()
        }
    }

    /// The terminal and the shell it runs, filling the destination panel.
    fn terminal_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        let metrics = self.metrics();
        let mut column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .gap(metrics.scale(spacing::TIGHT))
            .child(self.shell_picker(cx));

        if let Some(pane) = &self.pane {
            // A shell that ended keeps its last screen, but the reason it stopped is
            // stated above it instead of being left for the user to guess.
            if let Some(failure) = pane.read(cx).state().failure() {
                column = column.child(
                    div()
                        .flex_none()
                        .px(metrics.scale(spacing::TIGHT))
                        .py(metrics.scale(spacing::HAIRLINE))
                        .rounded(px(radius::SMALL))
                        .bg(theme::swatch(palette::SURFACE_RAISED))
                        .border_1()
                        .border_color(theme::swatch(palette::BORDER))
                        .text_size(metrics.label_size())
                        .text_color(Tone::Danger.text())
                        .debug_selector(|| "terminal.failure".to_owned())
                        .child(SharedString::from(failure.to_string())),
                );
            }
            column = column.child(pane.clone());
        }
        column
    }

    /// The shell choices and the free-text path field.
    fn shell_picker(&self, cx: &mut Context<Self>) -> gpui::Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let mut row = div()
            .flex()
            .items_center()
            .flex_wrap()
            .flex_none()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "shell.picker".to_owned())
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.shell_picker_label),
            );

        if self.workspace.shells().is_empty() {
            row = row.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(Tone::Warning.text())
                    .child(strings.shell_none_found),
            );
        }

        for (index, shell) in self.workspace.shells().iter().enumerate() {
            let selected = index == self.workspace.shell_choice();
            row = row.child(
                div()
                    .id(("shell.choice", index))
                    .debug_selector(move || format!("shell.choice.{index}"))
                    .px(metrics.scale(spacing::TIGHT))
                    .py(metrics.scale(2.0))
                    .rounded(px(radius::SMALL))
                    .bg(theme::swatch(if selected {
                        palette::ACCENT_TINT
                    } else {
                        palette::SURFACE_RAISED
                    }))
                    .border_1()
                    .border_color(theme::swatch(if selected {
                        palette::ACCENT
                    } else {
                        palette::BORDER
                    }))
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(if selected {
                        palette::ACCENT_ON_TINT
                    } else {
                        palette::TEXT_SECONDARY
                    }))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.workspace.select_shell(index);
                        cx.notify();
                    }))
                    .child(SharedString::from(shell.name().to_owned())),
            );
        }

        row = row.child(
            div()
                .id("shell.custom")
                .debug_selector(|| "shell.custom".to_owned())
                .px(metrics.scale(spacing::TIGHT))
                .py(metrics.scale(2.0))
                .rounded(px(radius::SMALL))
                .bg(theme::swatch(if self.custom_shell_open {
                    palette::SURFACE_SELECTED
                } else {
                    palette::SURFACE_RAISED
                }))
                .border_1()
                .border_color(theme::swatch(palette::BORDER))
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .cursor_pointer()
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.custom_shell_open = !this.custom_shell_open;
                    if this.custom_shell_open {
                        let handle = this.shell_path.read(cx).focus_handle().clone();
                        window.focus(&handle);
                    }
                    cx.notify();
                }))
                .child(strings.shell_custom_button),
        );

        if self.custom_shell_open {
            row = row.child(self.shell_path.clone());
        }
        row
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
        let row = components::row(metrics).child(
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
        );
        // A host without a group shows no group chip. Using the count unit as a
        // fallback printed the word "host" in a place that means "group", which
        // reads as a group named "host".
        match host_group_chip(host) {
            Some(group) => row.child(components::chip(group, Tone::Neutral, metrics)),
            None => row,
        }
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
                let locale = this.workspace.locale();
                if let Some(pane) = this.pane.clone() {
                    pane.update(cx, |pane, cx| pane.set_locale(locale, cx));
                }
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
                    .child(self.destination_panel(cx))
                    .child(self.status_bar()),
            )
    }
}

/// The group label a host row shows, when the host belongs to a group.
///
/// A host without a group shows nothing rather than the count unit, which used to
/// print the word "host" where a group name belongs.
pub(crate) fn host_group_chip(host: &HostConfig) -> Option<String> {
    host.group()
        .map(str::trim)
        .filter(|group| !group.is_empty())
        .map(str::to_owned)
}
