//! Managing the session records listed on the local shells and sessions pages.
//!
//! A tab that is closed leaves its session behind as a record ("closed"), so the list
//! says what ran. Without a way to act on those records the list only grows; this is
//! where a record is opened, reopened, closed or removed, from the row's own buttons
//! or from its context menu, and where every closed record is cleared at once.

use gpui::prelude::*;
use gpui::{Context, Div, MouseButton, Pixels, Point, SharedString, Stateful, Window, div, px};
use shellune_core::{Session, SessionId, SessionKind, SessionState};

use super::ShelluneView;
use super::overlays::clamp_menu;
use crate::app::components;
use crate::app::icons::{self, Icon};
use crate::app::theme::{self, Metrics, palette, radius, spacing};

/// One line of a context menu.
pub(super) fn menu_entry(
    label: &'static str,
    selector: &'static str,
    icon: Option<Icon>,
    enabled: bool,
    danger: bool,
    metrics: Metrics,
) -> Stateful<Div> {
    let hover = theme::swatch(palette::SURFACE_HOVER);
    let color = if !enabled {
        theme::swatch(palette::TEXT_MUTED)
    } else if danger {
        theme::swatch(palette::DANGER)
    } else {
        theme::swatch(palette::TEXT_PRIMARY)
    };
    div()
        .id(selector)
        .debug_selector(move || selector.to_owned())
        .flex()
        .items_center()
        .gap(metrics.scale(spacing::TIGHT))
        .h(metrics.scale(30.0))
        .px(metrics.scale(spacing::TIGHT + 2.0))
        .rounded(px(radius::MEDIUM))
        .text_size(metrics.label_size())
        .text_color(color)
        .when(enabled, |item| {
            item.cursor_pointer().hover(move |style| style.bg(hover))
        })
        .children(icon.map(|icon| icons::icon(icon, metrics.small_icon_size(), color)))
        .child(label)
}

/// The hairline between groups of menu entries.
pub(super) fn menu_divider(metrics: Metrics) -> Div {
    div()
        .h(px(1.0))
        .mx(metrics.scale(spacing::HAIRLINE))
        .my(metrics.scale(2.0))
        .bg(theme::swatch(palette::BORDER))
}

/// The floating card a context menu is drawn in.
pub(super) fn menu_card(selector: &'static str, metrics: Metrics) -> Div {
    div()
        .debug_selector(move || selector.to_owned())
        .flex()
        .flex_col()
        .min_w(metrics.scale(200.0))
        .p(metrics.scale(spacing::HAIRLINE))
        .rounded(px(radius::LARGE))
        .bg(theme::swatch(palette::SURFACE_RAISED))
        .border_1()
        .border_color(theme::swatch(palette::BORDER_STRONG))
        .shadow_lg()
}

impl ShelluneView {
    /// Whether the session record is closed: its tab is gone and only the record is
    /// left.
    fn is_closed(&self, id: SessionId) -> bool {
        self.workspace
            .session(id)
            .is_some_and(|session| matches!(session.state(), SessionState::Closed))
    }

    /// Opens the menu of one session row where the pointer is.
    ///
    /// Like the tab menu it takes the keyboard, so Escape closes it.
    pub(super) fn open_session_menu(
        &mut self,
        id: SessionId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shell_menu_open = false;
        self.tab_menu = None;
        self.remember_focus(window, cx);
        self.session_menu = Some((id, position));
        window.focus(&self.root_focus.clone());
        cx.notify();
    }

    /// Whether a session row's menu is open.
    #[allow(dead_code)]
    #[must_use]
    pub fn session_menu_open(&self) -> bool {
        self.session_menu.is_some()
    }

    pub(super) fn close_session_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session_menu.take().is_some() {
            self.restore_overlay_focus(window, cx);
            cx.notify();
        }
    }

    /// Removes one session: its tab first when it still has one, then its record.
    ///
    /// A session with transfers or forwards running asks first, as its close button
    /// does; the record then stays until the user has answered, because removing it
    /// would take the question's subject away.
    pub fn remove_session(&mut self, id: SessionId, cx: &mut Context<Self>) {
        if !self.is_closed(id) {
            if self.close_needs_confirmation(id) {
                self.closing = Some(id);
                cx.notify();
                return;
            }
            self.close_session(id, cx);
        }
        self.workspace.session_removed(id);
        cx.notify();
    }

    /// Removes every record whose tab was closed; open sessions are untouched.
    pub fn remove_closed_sessions(&mut self, cx: &mut Context<Self>) {
        for id in self.workspace.closed_sessions() {
            self.workspace.session_removed(id);
        }
        cx.notify();
    }

    /// Starts a closed session again: the same shell, or the same host's form.
    ///
    /// The new session takes the old record's place, so reopening does not leave a
    /// closed copy behind it.
    pub fn reopen_session(&mut self, id: SessionId, window: &mut Window, cx: &mut Context<Self>) {
        self.duplicate_session(id, window, cx);
        if self.is_closed(id) {
            self.workspace.session_removed(id);
        }
        cx.notify();
    }

    /// The quiet line above a list that holds closed records, with the button that
    /// clears them.
    pub(super) fn closed_sessions_bar(
        &self,
        sessions: &[&Session],
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let closed = sessions
            .iter()
            .filter(|session| matches!(session.state(), SessionState::Closed))
            .count();
        if closed == 0 {
            return None;
        }
        let metrics = self.metrics();
        let strings = self.strings();
        Some(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(metrics.scale(spacing::TIGHT))
                .px(metrics.scale(spacing::HAIRLINE))
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .debug_selector(|| "sessions.closed".to_owned())
                .child(SharedString::from(format!(
                    "{} · {closed}",
                    strings.session_closed_summary
                )))
                .child(
                    components::icon_label_button(
                        Icon::Trash,
                        strings.session_menu_remove_closed,
                        components::Variant::Ghost,
                        metrics,
                    )
                    .id("sessions.remove_closed")
                    .debug_selector(|| "sessions.remove_closed".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.remove_closed_sessions(cx);
                    })),
                ),
        )
    }

    /// The buttons at the end of a session row: remove for a closed record, and the
    /// menu with every other action.
    pub(super) fn session_row_actions(
        &self,
        id: SessionId,
        index: usize,
        cx: &mut Context<Self>,
    ) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let closed = self.is_closed(id);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(metrics.scale(2.0))
            .when(closed, |row| {
                row.child(
                    components::icon_only_button(Icon::Trash, metrics)
                        .id(("row.remove", index))
                        .debug_selector(move || format!("row.remove.{index}"))
                        .tooltip(components::tooltip(strings.session_menu_remove))
                        .on_click(cx.listener(move |this, _event, _window, cx| {
                            cx.stop_propagation();
                            this.remove_session(id, cx);
                        })),
                )
            })
            .child(
                components::icon_only_button(Icon::More, metrics)
                    .id(("row.more", index))
                    .debug_selector(move || format!("row.more.{index}"))
                    .tooltip(components::tooltip(strings.session_more))
                    .on_click(
                        cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                            cx.stop_propagation();
                            let position = event.position();
                            this.open_session_menu(id, position, window, cx);
                        }),
                    ),
            )
    }

    /// The menu of one session row.
    pub(super) fn session_menu_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let (id, position) = self.session_menu?;
        let session = self.workspace.session(id)?;
        let metrics = self.metrics();
        let strings = self.strings();
        let viewport = window.viewport_size();
        let closed = matches!(session.state(), SessionState::Closed);
        let openable = self.pane_for(id).is_some();
        let any_closed = !self.workspace.closed_sessions().is_empty();

        let mut menu = menu_card("session.menu", metrics);
        if openable {
            menu = menu.child(
                menu_entry(
                    strings.session_menu_open,
                    "session.menu.open",
                    Some(Icon::ArrowRight),
                    true,
                    false,
                    metrics,
                )
                .on_click(cx.listener(move |this, _event, window, cx| {
                    this.close_session_menu(window, cx);
                    this.activate_in(id, window, cx);
                })),
            );
        }
        if closed {
            menu = menu.child(
                menu_entry(
                    strings.session_menu_reopen,
                    "session.menu.reopen",
                    Some(Icon::Refresh),
                    true,
                    false,
                    metrics,
                )
                .on_click(cx.listener(move |this, _event, window, cx| {
                    this.close_session_menu(window, cx);
                    this.reopen_session(id, window, cx);
                })),
            );
        } else {
            menu = menu.child(
                menu_entry(
                    strings.tab_menu_duplicate,
                    "session.menu.duplicate",
                    Some(Icon::Copy),
                    true,
                    false,
                    metrics,
                )
                .on_click(cx.listener(move |this, _event, window, cx| {
                    this.close_session_menu(window, cx);
                    this.duplicate_session(id, window, cx);
                })),
            );
        }
        if let SessionKind::Remote { host } = session.kind().clone() {
            menu = menu.child(
                menu_entry(
                    strings.forward_open,
                    "session.menu.forwards",
                    Some(Icon::Forward),
                    true,
                    false,
                    metrics,
                )
                .on_click(cx.listener(move |this, _event, window, cx| {
                    this.close_session_menu(window, cx);
                    this.show_forwards_scope(crate::app::forwarding::ForwardScope::Host(host), cx);
                })),
            );
        }
        menu = menu.child(menu_divider(metrics));
        if !closed {
            menu = menu.child(
                menu_entry(
                    strings.session_menu_close,
                    "session.menu.close",
                    Some(Icon::Close),
                    true,
                    false,
                    metrics,
                )
                .on_click(cx.listener(move |this, _event, window, cx| {
                    this.close_session_menu(window, cx);
                    this.request_close_session(id, cx);
                })),
            );
        }
        menu = menu
            .child(
                menu_entry(
                    strings.session_menu_remove,
                    "session.menu.remove",
                    Some(Icon::Trash),
                    true,
                    true,
                    metrics,
                )
                .on_click(cx.listener(move |this, _event, window, cx| {
                    this.close_session_menu(window, cx);
                    this.remove_session(id, cx);
                })),
            )
            .child(
                menu_entry(
                    strings.session_menu_remove_closed,
                    "session.menu.remove_closed",
                    None,
                    any_closed,
                    false,
                    metrics,
                )
                .when(any_closed, |item| {
                    item.on_click(cx.listener(|this, _event, window, cx| {
                        this.close_session_menu(window, cx);
                        this.remove_closed_sessions(cx);
                    }))
                }),
            );

        Some(
            super::overlay_layer().child(
                div()
                    .relative()
                    .w(viewport.width)
                    .h(viewport.height)
                    .child(
                        div()
                            .id("session.menu.backdrop")
                            .absolute()
                            .inset_0()
                            .occlude()
                            .on_any_mouse_down(cx.listener(|this, _event, window, cx| {
                                this.close_session_menu(window, cx);
                            })),
                    )
                    .child(
                        gpui::anchored()
                            .position(clamp_menu(
                                position,
                                viewport,
                                metrics.scale(210.0),
                                px(0.0),
                            ))
                            .snap_to_window_with_margin(metrics.scale(spacing::TIGHT))
                            .child(div().occlude().child(menu)),
                    ),
            ),
        )
    }

    /// Opens the head's "more" menu where the pointer pressed it (R5, R12).
    ///
    /// The low-frequency session actions live here so the head keeps one short row. What
    /// a user needs at a glance - close, split, the terminal/files switch, reconnect and
    /// save - stays on the head itself; everything that is a one-off belongs to a menu.
    pub(super) fn open_session_more(
        &mut self,
        id: SessionId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shell_menu_open = false;
        self.tab_menu = None;
        self.session_menu = None;
        self.remember_focus(window, cx);
        self.session_more = Some((id, position));
        // The terminal keeps the keyboard, as it does for the tab menu: Escape is bound
        // where the shell answers it, and the menu takes no keys of its own (R5).
        cx.notify();
    }

    /// Whether the head's more menu is open.
    #[allow(dead_code)]
    #[must_use]
    pub fn session_more_open(&self) -> bool {
        self.session_more.is_some()
    }

    pub(super) fn close_session_more(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session_more.take().is_some() {
            self.restore_overlay_focus(window, cx);
            cx.notify();
        }
    }

    /// The menu of the session on the head.
    pub(super) fn session_more_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let (id, position) = self.session_more?;
        let session = self.workspace.session(id)?;
        let metrics = self.metrics();
        let strings = self.strings();
        let viewport = window.viewport_size();
        let closed = matches!(session.state(), SessionState::Closed);
        let forward_host = match session.kind() {
            SessionKind::Remote { host } if self.book.host(*host).is_some() => Some(*host),
            _ => None,
        };
        let more_than_one = self.workspace.layout().tabs().len() > 1;

        let mut menu = menu_card("session.more.menu", metrics);
        if !closed {
            menu = menu.child(
                menu_entry(
                    strings.tab_menu_duplicate,
                    "session.more.duplicate",
                    Some(Icon::Copy),
                    true,
                    false,
                    metrics,
                )
                .on_click(cx.listener(move |this, _event, window, cx| {
                    this.close_session_more(window, cx);
                    this.duplicate_session(id, window, cx);
                })),
            );
        }
        if let Some(host) = forward_host {
            menu = menu.child(
                menu_entry(
                    crate::app::copy::ux().session_forwards,
                    "session.more.forwards",
                    Some(Icon::Forward),
                    true,
                    false,
                    metrics,
                )
                .on_click(cx.listener(move |this, _event, window, cx| {
                    this.close_session_more(window, cx);
                    this.show_forwards_scope(crate::app::forwarding::ForwardScope::Host(host), cx);
                })),
            );
        }
        menu = menu.child(menu_divider(metrics));
        if !closed {
            menu = menu.child(
                menu_entry(
                    strings.session_menu_close,
                    "session.more.close",
                    Some(Icon::Close),
                    true,
                    false,
                    metrics,
                )
                .on_click(cx.listener(move |this, _event, window, cx| {
                    this.close_session_more(window, cx);
                    this.request_close_session(id, cx);
                })),
            );
        }
        if more_than_one {
            menu = menu.child(
                menu_entry(
                    strings.tab_menu_close_others,
                    "session.more.close_others",
                    None,
                    true,
                    false,
                    metrics,
                )
                .on_click(cx.listener(move |this, _event, window, cx| {
                    this.close_session_more(window, cx);
                    this.close_other_tabs(id, cx);
                })),
            );
        }

        Some(
            super::overlay_layer().child(
                div()
                    .relative()
                    .w(viewport.width)
                    .h(viewport.height)
                    .child(
                        div()
                            .id("session.more.backdrop")
                            .absolute()
                            .inset_0()
                            .occlude()
                            .on_any_mouse_down(cx.listener(|this, _event, window, cx| {
                                this.close_session_more(window, cx);
                            })),
                    )
                    .child(
                        gpui::anchored()
                            .position(clamp_menu(
                                position,
                                viewport,
                                metrics.scale(220.0),
                                px(0.0),
                            ))
                            .snap_to_window_with_margin(metrics.scale(spacing::TIGHT))
                            .child(div().occlude().child(menu)),
                    ),
            ),
        )
    }
    /// The right-click handler of a session row.
    pub(super) fn session_row_menu_listener(
        &self,
        id: SessionId,
        cx: &mut Context<Self>,
    ) -> impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static {
        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
            cx.stop_propagation();
            this.open_session_menu(id, event.position, window, cx);
        })
    }
}

/// The mouse button a row's menu opens with.
pub(super) const MENU_BUTTON: MouseButton = MouseButton::Right;
