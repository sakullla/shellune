//! The forwarding page: saved rules, their real runtime, and the form (R11).

use gpui::prelude::*;
use gpui::{
    App, Context, Entity, FontWeight, KeyDownEvent, ScrollHandle, SharedString, Window, div, px,
};
use shellune_core::{
    BindExposure, ForwardFault, ForwardId, ForwardKind, ForwardRule, HostConfig, HostId, SessionId,
    SessionKind, SessionState,
};
use shellune_ssh::{ForwardCommand, ForwardFailure};
use shellune_storage::ForwardStore;

use super::ShelluneView;
use crate::app::components::{self, ControlState, Variant};
use crate::app::copy::{self, Strings};
use crate::app::forwarding::{
    ForwardConfirm, ForwardEditor, ForwardPhase, ForwardScope, ForwardStart, ForwardStatusFilter,
};
use crate::app::icons::{self, Icon};
use crate::app::text_field::TextField;
use crate::app::theme::{self, Tone, palette, radius, spacing};

/// The width of a port field beside its address, before text scale: five digits and
/// the field's own padding.
const FORWARD_PORT_WIDTH: f32 = 120.0;

/// The narrowest a tunnel row's words get before its commands wrap under them, before
/// text scale.
const FORWARD_ROW_WORDS_MIN: f32 = 200.0;

/// How many hosts the through-host picker lists before its list scrolls.
const FORWARD_HOST_ROWS: f32 = 6.0;

/// The widest a host's name gets in the through-host control and its picker, before
/// text scale; its address takes the rest.
const FORWARD_HOST_NAME_MAX: f32 = 200.0;

/// The least width of the forwards search field before toolbar controls wrap.
const FORWARD_SEARCH_MIN_WIDTH: f32 = 180.0;

/// The through-host picker of the tunnel form, while it is open (ADR-7).
///
/// A search field over the saved hosts with one candidate marked for the keyboard: a
/// book of any size is one short list to narrow instead of a row of segments that runs
/// out of the form.
pub(super) struct ForwardHostPicker {
    search: Entity<TextField>,
    /// The search the candidates were last narrowed by: a new phrase moves the mark
    /// back to the first candidate.
    phrase: String,
    /// The candidate Enter picks, as an index into the narrowed list.
    highlight: usize,
    scroll: ScrollHandle,
}

impl ShelluneView {
    pub(super) fn reload_forwards(&mut self) {
        let store = ForwardStore::at(self.store.root());
        let read = store.load().map_err(|error| error.to_string());
        self.adopt_forwards(read);
    }

    /// Takes the saved tunnels as they were read: now, or behind the splash (R7).
    pub(super) fn adopt_forwards(&mut self, read: Result<shellune_core::ForwardBook, String>) {
        match read {
            Ok(book) => {
                self.forwards.replace_book(book);
                self.forwards.clear_load_error();
                self.sync_forward_section();
            }
            Err(error) => self.forwards.note_load_error(error),
        }
    }

    pub(super) fn sync_forward_section(&mut self) {
        let rules = self.forwards.book().rules().to_vec();
        self.workspace.replace_forwards(rules);
    }

    /// Opens the global page. Nothing listens.
    pub(super) fn open_global_forwards(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.show_forwards_scope(ForwardScope::All, cx);
    }

    /// Opens the rules of one host or session in the port-forwarding dialog.
    /// Nothing listens.
    pub(super) fn show_forwards_scope(&mut self, scope: ForwardScope, cx: &mut Context<Self>) {
        self.forwards.set_scope(scope);
        self.sync_forward_section();
        self.open_tool(super::shell::Tool::Forwards, cx);
    }

    /// Shows the global page after a pure-forward connection is up. Nothing new is bound here.
    pub(super) fn present_forwards(&mut self, cx: &mut Context<Self>) {
        self.show_forwards_scope(ForwardScope::All, cx);
    }

    pub(super) fn open_forward_editor(&mut self, cx: &mut Context<Self>) {
        // A tunnel opened from one host's tools goes through that host.
        let host = match self.forwards.scope() {
            ForwardScope::Host(id) if self.book.host(id).is_some() => Some(id),
            _ => self.book.hosts().first().map(HostConfig::id),
        };
        self.forwards.begin_new(host);
        self.forward_host_picker = None;
        self.fill_forward_fields("", "127.0.0.1", "", "", "", cx);
        cx.notify();
    }

    /// Whether the through-host picker is open over a tunnel form on screen.
    pub(super) fn forward_host_picker_shown(&self) -> bool {
        self.forward_host_picker.is_some()
            && self.tool == Some(super::shell::Tool::Forwards)
            && self.forwards.editor().is_some()
    }

    /// Opens the through-host picker with an empty search that has the keyboard and the
    /// chosen host marked, or closes it when it is already open.
    pub(super) fn toggle_forward_host_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.forward_host_picker.is_some() {
            self.close_forward_host_picker(window, cx);
            return;
        }
        let placeholder = copy::ux_for(self.workspace.locale()).fwd_host_search;
        let search = cx.new(|cx| {
            let mut field = TextField::new(placeholder, cx);
            field.set_leading_icon(Icon::Search, cx);
            field
        });
        // A new phrase marks the first candidate again; the list itself is read from
        // the field on every draw.
        cx.observe(&search, |this, search, cx| {
            let text = search.read(cx).text().to_owned();
            if let Some(picker) = this.forward_host_picker.as_mut()
                && picker.phrase != text
            {
                picker.phrase = text;
                picker.highlight = 0;
                picker.scroll.scroll_to_item(0);
            }
            cx.notify();
        })
        .detach();
        window.focus(search.read(cx).focus_handle());
        let chosen = self.forwards.editor().and_then(|editor| editor.host);
        let highlight = self
            .book
            .hosts()
            .iter()
            .position(|host| Some(host.id()) == chosen)
            .unwrap_or(0);
        let scroll = ScrollHandle::new();
        scroll.scroll_to_item(highlight);
        self.forward_host_picker = Some(ForwardHostPicker {
            search,
            phrase: String::new(),
            highlight,
            scroll,
        });
        cx.notify();
    }

    /// Closes the through-host picker and gives the keyboard back to its control.
    pub(super) fn close_forward_host_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.forward_host_picker.take().is_some() {
            window.focus(&self.forward_host_focus);
            cx.notify();
        }
    }

    /// The saved hosts the open picker offers for what is typed in it, in saved order:
    /// those whose name or address holds the phrase, ignoring case.
    fn forward_host_candidates(&self, cx: &App) -> Vec<HostId> {
        let phrase = self
            .forward_host_picker
            .as_ref()
            .map(|picker| picker.search.read(cx).text().trim().to_lowercase())
            .unwrap_or_default();
        self.book
            .hosts()
            .iter()
            .filter(|host| {
                phrase.is_empty()
                    || host.name().to_lowercase().contains(&phrase)
                    || host.address().to_lowercase().contains(&phrase)
            })
            .map(HostConfig::id)
            .collect()
    }

    /// Takes `id` as the host the tunnel goes through and closes the picker.
    fn pick_forward_host(&mut self, id: HostId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.forwards.editor_mut() {
            editor.host = Some(id);
        }
        self.close_forward_host_picker(window, cx);
        cx.notify();
    }

    /// Up and Down move the mark through the candidates, Enter picks the marked one and
    /// Escape closes the list. They are taken before the search field sees them: the
    /// field would read Enter as a submit of its own.
    fn forward_host_picker_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.modifiers.number_of_modifiers() != 0 {
            return;
        }
        let candidates = self.forward_host_candidates(cx);
        let Some(picker) = self.forward_host_picker.as_mut() else {
            return;
        };
        let last = candidates.len().saturating_sub(1);
        match event.keystroke.key.as_str() {
            "down" => {
                picker.highlight = (picker.highlight + 1).min(last);
                picker.scroll.scroll_to_item(picker.highlight);
            }
            "up" => {
                picker.highlight = picker.highlight.saturating_sub(1).min(last);
                picker.scroll.scroll_to_item(picker.highlight);
            }
            "enter" => {
                if let Some(id) = candidates.get(picker.highlight.min(last)).copied() {
                    self.pick_forward_host(id, window, cx);
                }
            }
            "escape" => self.close_forward_host_picker(window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The keys of the closed through-host control: Enter, Space or Down opens the
    /// picker, and Tab walks on to the form's fields.
    fn forward_host_control_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform {
            return;
        }
        match event.keystroke.key.as_str() {
            "enter" | "space" | "down" if !modifiers.shift => {
                self.toggle_forward_host_picker(window, cx);
            }
            "tab" => {
                let fields = self.forward_tab_fields();
                let field = if modifiers.shift {
                    fields.last()
                } else {
                    fields.first()
                };
                if let Some(field) = field {
                    window.focus(field.read(cx).focus_handle());
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The search, the marked candidate and how many candidates the open through-host
    /// picker shows, for the tests.
    #[cfg(test)]
    pub(crate) fn forward_host_picker_for_test(&self, cx: &App) -> Option<(String, usize, usize)> {
        let picker = self.forward_host_picker.as_ref()?;
        Some((
            picker.search.read(cx).text().to_owned(),
            picker.highlight,
            self.forward_host_candidates(cx).len(),
        ))
    }

    /// The host the tunnel form goes through, for the tests.
    #[cfg(test)]
    pub(crate) fn forward_editor_host_for_test(&self) -> Option<HostId> {
        self.forwards.editor().and_then(|editor| editor.host)
    }

    /// Whether the through-host control holds the keyboard, for the tests.
    #[cfg(test)]
    pub(crate) fn forward_host_control_focused_for_test(&self, window: &Window) -> bool {
        self.forward_host_focus.is_focused(window)
    }

    /// The through-host control: the chosen host as a field that opens a searchable
    /// list, with the list under it while it is open (ADR-7).
    fn forward_host_control(&self, editor: &ForwardEditor, cx: &mut Context<Self>) -> gpui::Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let chosen = editor.host.and_then(|id| self.book.host(id));
        let ring = theme::swatch(palette::FOCUS_RING);
        let open = self.forward_host_picker.is_some();
        let trigger = components::field_frame(
            if open {
                ControlState::Focused
            } else {
                ControlState::Resting
            },
            metrics,
        )
        .id("forwards.host")
        .debug_selector(|| "forwards.host".to_owned())
        .track_focus(&self.forward_host_focus)
        .focus(move |style| style.border_color(ring))
        .cursor_pointer()
        .child(icons::icon(
            Icon::Server,
            metrics.small_icon_size(),
            theme::swatch(palette::TEXT_MUTED),
        ))
        .child(
            div()
                .flex_none()
                .max_w(metrics.scale(FORWARD_HOST_NAME_MAX))
                .truncate()
                .text_size(metrics.label_size())
                .text_color(theme::swatch(if chosen.is_some() {
                    palette::TEXT_PRIMARY
                } else {
                    palette::TEXT_MUTED
                }))
                .debug_selector(|| "forwards.host.name".to_owned())
                .child(SharedString::from(
                    chosen
                        .map_or(ux.fwd_host_choose, HostConfig::name)
                        .to_owned(),
                )),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(crate::app::terminal::monospace_family())
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .children(chosen.map(|host| SharedString::from(host.target()))),
        )
        .child(icons::icon(
            if open {
                Icon::ChevronUp
            } else {
                Icon::ChevronDown
            },
            metrics.small_icon_size(),
            theme::swatch(palette::TEXT_MUTED),
        ))
        .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
            view.forward_host_control_key(event, window, cx);
        }))
        .on_click(cx.listener(|view, _event, window, cx| {
            view.toggle_forward_host_picker(window, cx);
        }));
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::HAIRLINE))
            // A press anywhere else closes the list; the control itself toggles it.
            .when(open, |control| {
                control.on_mouse_down_out(cx.listener(|view, _event, window, cx| {
                    view.close_forward_host_picker(window, cx);
                }))
            })
            .child(trigger)
            .children(self.forward_host_picker_card(editor, cx))
    }

    /// The open through-host picker: its search field over the hosts it narrows.
    fn forward_host_picker_card(
        &self,
        editor: &ForwardEditor,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let picker = self.forward_host_picker.as_ref()?;
        let metrics = self.metrics();
        let ux = copy::ux();
        let candidates = self.forward_host_candidates(cx);
        let marked = picker.highlight.min(candidates.len().saturating_sub(1));
        let row_height = metrics.control_height();
        let mut list = div()
            .id("forwards.host.options")
            .debug_selector(|| "forwards.host.options".to_owned())
            .flex()
            .flex_col()
            .max_h(row_height * FORWARD_HOST_ROWS)
            .overflow_y_scroll()
            .track_scroll(&picker.scroll);
        for (index, id) in candidates.iter().copied().enumerate() {
            let Some(host) = self.book.host(id) else {
                continue;
            };
            let marked_row = index == marked;
            let chosen = editor.host == Some(id);
            let hover = theme::swatch(palette::SURFACE_HOVER);
            list = list.child(
                div()
                    .id(SharedString::from(format!("forwards.host.option.{index}")))
                    .debug_selector(move || format!("forwards.host.option.{index}"))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .h(row_height)
                    .px(metrics.scale(spacing::TIGHT))
                    .rounded(px(radius::SMALL))
                    .cursor_pointer()
                    .when(marked_row, |row| {
                        row.bg(theme::swatch(palette::SURFACE_SELECTED))
                    })
                    .when(!marked_row, |row| row.hover(move |style| style.bg(hover)))
                    .child(icons::icon(
                        if chosen { Icon::Check } else { Icon::Server },
                        metrics.small_icon_size(),
                        theme::swatch(if chosen || marked_row {
                            palette::ACCENT
                        } else {
                            palette::TEXT_MUTED
                        }),
                    ))
                    .child(
                        div()
                            .flex_none()
                            .max_w(metrics.scale(FORWARD_HOST_NAME_MAX))
                            .truncate()
                            .text_size(metrics.label_size())
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(SharedString::from(host.name().to_owned())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(crate::app::terminal::monospace_family())
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(host.target())),
                    )
                    .on_click(cx.listener(move |view, _event, window, cx| {
                        view.pick_forward_host(id, window, cx);
                    })),
            );
        }
        let none = candidates.is_empty().then(|| {
            div()
                .debug_selector(|| "forwards.host.none".to_owned())
                .px(metrics.scale(spacing::TIGHT))
                .py(metrics.scale(spacing::TIGHT))
                .whitespace_normal()
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .child(ux.fwd_host_no_match)
        });
        Some(
            components::menu_card("forwards.host.picker", metrics)
                .w_full()
                .min_w_0()
                .capture_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                    view.forward_host_picker_key(event, window, cx);
                }))
                .child(
                    div()
                        .flex_none()
                        .debug_selector(|| "forwards.host.search".to_owned())
                        .child(picker.search.clone()),
                )
                .child(list)
                .children(none),
        )
    }

    fn fill_forward_fields(
        &mut self,
        label: &str,
        listen_host: &str,
        listen_port: &str,
        target_host: &str,
        target_port: &str,
        cx: &mut Context<Self>,
    ) {
        self.forward_label.update(cx, |field, cx| {
            field.set_text(label, cx);
        });
        self.forward_listen_host.update(cx, |field, cx| {
            field.set_text(listen_host, cx);
        });
        self.forward_listen_port.update(cx, |field, cx| {
            field.set_text(listen_port, cx);
        });
        self.forward_target_host.update(cx, |field, cx| {
            field.set_text(target_host, cx);
        });
        self.forward_target_port
            .update(cx, |field, cx| field.set_text(target_port, cx));
    }

    pub(super) fn edit_forward(&mut self, id: ForwardId, cx: &mut Context<Self>) {
        let Some(rule) = self.forwards.book().rule(id).cloned() else {
            return;
        };
        self.forwards.begin_edit(id);
        self.forward_host_picker = None;
        let target = rule.target();
        self.fill_forward_fields(
            rule.label().unwrap_or(""),
            rule.listen().host(),
            &rule.listen().port().get().to_string(),
            target.map(shellune_core::Endpoint::host).unwrap_or(""),
            &target
                .map(|endpoint| endpoint.port().get().to_string())
                .unwrap_or_default(),
            cx,
        );
        cx.notify();
    }

    fn report_forward_action_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.forwards.note_action_error(error.clone());
        let message = copy::fill(copy::ux().fwd_action_failed, &[("reason", &error)]);
        self.toast(Tone::Danger, message, None, cx);
    }

    fn save_forward_inner(&mut self, cx: &mut Context<Self>) -> Option<ForwardId> {
        let label = self.forward_label.read(cx).text().to_owned();
        let listen_host = self.forward_listen_host.read(cx).text().to_owned();
        let listen_port = self.forward_listen_port.read(cx).text().to_owned();
        let target_host = self.forward_target_host.read(cx).text().to_owned();
        let target_port = self.forward_target_port.read(cx).text().to_owned();
        let (book, id) = match self.forwards.preview_with_label(
            &label,
            &listen_host,
            &listen_port,
            &target_host,
            &target_port,
        ) {
            Ok(result) => result,
            Err(error) => {
                self.forwards.fail_editor(error);
                cx.notify();
                return None;
            }
        };
        if let Err(error) = self.write_forwards_file(&book) {
            self.forwards.fail_editor(error.to_string());
            cx.notify();
            return None;
        }
        self.forwards.adopt(book);
        self.sync_forward_section();
        self.note_data_changed(cx);
        cx.notify();
        Some(id)
    }

    pub(super) fn save_forward(&mut self, cx: &mut Context<Self>) {
        let _ = self.save_forward_inner(cx);
    }

    pub(super) fn save_and_start_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.save_forward_inner(cx) {
            self.start_forward(id, cx);
        }
    }

    pub(super) fn copy_forward(&mut self, id: ForwardId, cx: &mut Context<Self>) {
        let Some(rule) = self.forwards.copy_of(id) else {
            self.report_forward_action_error("that forward is not saved".to_owned(), cx);
            cx.notify();
            return;
        };
        let mut book = self.forwards.book().clone();
        if let Err(error) = book.insert(rule) {
            self.report_forward_action_error(error.to_string(), cx);
            cx.notify();
            return;
        }
        if let Err(error) = self.write_forwards_file(&book) {
            self.report_forward_action_error(error.to_string(), cx);
            cx.notify();
            return;
        }
        self.forwards.adopt(book);
        self.sync_forward_section();
        self.note_data_changed(cx);
        cx.notify();
    }

    pub(super) fn delete_forward(&mut self, id: ForwardId, cx: &mut Context<Self>) {
        if let Some(session) = self
            .forwards
            .rows()
            .into_iter()
            .find(|row| row.rule.id() == id)
            .and_then(|row| match row.phase {
                ForwardPhase::Listening { session, .. }
                | ForwardPhase::Starting { session, .. } => Some(session),
                _ => None,
            })
        {
            let _ = self.forwards.send(session, ForwardCommand::Stop { id });
        }
        let book = match self.forwards.without(id) {
            Ok(book) => book,
            Err(error) => {
                self.report_forward_action_error(error, cx);
                cx.notify();
                return;
            }
        };
        if let Err(error) = self.write_forwards_file(&book) {
            self.report_forward_action_error(error.to_string(), cx);
            cx.notify();
            return;
        }
        self.forwards.adopt(book);
        self.sync_forward_section();
        self.note_data_changed(cx);
        cx.notify();
    }

    pub(super) fn start_forward(&mut self, id: ForwardId, cx: &mut Context<Self>) {
        self.forwards.clear_action_error();
        let host = self.forwards.book().rule(id).map(ForwardRule::host);
        let connected = host.and_then(|host| self.carrier_for_host(host));
        match self.forwards.prepare_start(id, connected) {
            Ok(ForwardStart::Exposure { id, .. }) => {
                // Starting waits for the button that calls `confirm_forward_exposure`.
                self.pending_exposure = Some(id);
            }
            Ok(ForwardStart::Connection { id, host }) => {
                if let Some(saved) = self.book.host(host).cloned() {
                    self.open_forward_connection(&saved, id, cx);
                } else {
                    self.report_forward_action_error(
                        format!("host {host} is not saved. Nothing was connected."),
                        cx,
                    );
                }
            }
            Ok(ForwardStart::Ready { session, spec }) => {
                let id = spec.id;
                if !self.forwards.send(session, ForwardCommand::Start(spec)) {
                    self.forwards.detach(session);
                }
                let _ = id;
                self.watch_forwards(cx);
            }
            Err(error) => self.report_forward_action_error(error, cx),
        }
        self.sync_forward_section();
        cx.notify();
    }

    /// Starts every inactive rule belonging to `host`.
    pub(super) fn start_host_forwards(&mut self, host: HostId, cx: &mut Context<Self>) {
        let ids = self.forwards.startable_ids_for_host(host);
        if ids.is_empty() {
            return;
        }
        self.forwards.clear_action_error();
        if self.carrier_for_host(host).is_some() {
            for id in ids {
                self.start_forward(id, cx);
                if self.pending_exposure.is_some() {
                    break;
                }
            }
            return;
        }
        let Some(saved) = self.book.host(host).cloned() else {
            self.report_forward_action_error(
                format!("host {host} is not saved. Nothing was connected."),
                cx,
            );
            cx.notify();
            return;
        };
        let mut ready_for_dial = Vec::new();
        for id in ids {
            if let Some(rule) = self.forwards.book().rule(id)
                && rule.needs_exposure_confirmation()
            {
                self.pending_exposure = Some(id);
                cx.notify();
                return;
            }
            ready_for_dial.push(id);
        }
        if !ready_for_dial.is_empty() {
            self.open_forward_connection_many(&saved, ready_for_dial, cx);
        }
        self.sync_forward_section();
        cx.notify();
    }

    /// Stops every active rule belonging to `host`.
    pub(super) fn stop_host_forwards(&mut self, host: HostId, cx: &mut Context<Self>) {
        let ids = self.forwards.active_ids_for_host(host);
        if ids.is_empty() {
            return;
        }
        self.forwards.clear_action_error();
        for id in ids {
            let _ = self.forwards.stop_one(id);
        }
        self.sync_forward_section();
        self.watch_forwards(cx);
        cx.notify();
    }

    /// Restarts all rules in the current scope that failed due to SSH carrier disconnect.
    pub(super) fn restart_dropped_forwards(&mut self, cx: &mut Context<Self>) {
        let dropped = self.forwards.dropped_rules();
        if dropped.is_empty() {
            return;
        }
        self.forwards.clear_action_error();
        let mut by_host: Vec<(HostId, Vec<ForwardId>)> = Vec::new();
        for rule in dropped {
            if let Some((_, list)) = by_host.iter_mut().find(|(h, _)| *h == rule.host()) {
                list.push(rule.id());
            } else {
                by_host.push((rule.host(), vec![rule.id()]));
            }
        }
        let mut dial_pending: Option<(HostConfig, Vec<ForwardId>)> = None;
        for (host, ids) in by_host {
            if self.carrier_for_host(host).is_some() {
                for id in ids {
                    self.start_forward(id, cx);
                }
            } else if let Some(saved) = self.book.host(host).cloned() {
                if dial_pending.is_none() {
                    dial_pending = Some((saved, ids));
                }
            } else {
                self.report_forward_action_error(
                    format!("host {host} is not saved. Nothing was connected."),
                    cx,
                );
            }
        }
        if let Some((saved, ids)) = dial_pending {
            self.open_forward_connection_many(&saved, ids, cx);
        }
        self.sync_forward_section();
        cx.notify();
    }

    pub(super) fn confirm_forward_exposure(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.pending_exposure.take() else {
            return;
        };
        self.forwards.confirm_exposure(id);
        self.start_forward(id, cx);
    }

    pub(super) fn cancel_forward_exposure(&mut self, cx: &mut Context<Self>) {
        self.pending_exposure = None;
        self.forwards.cancel_exposure();
        cx.notify();
    }

    pub(super) fn stop_forward(&mut self, id: ForwardId, cx: &mut Context<Self>) {
        let _ = self.forwards.stop_one(id);
        self.sync_forward_section();
        self.watch_forwards(cx);
        cx.notify();
    }

    /// Dials a shell-less connection for one saved rule.
    ///
    /// The flag is set before the form can dial, so a host that already has
    /// credentials does not open a terminal on the way.
    fn open_forward_connection(
        &mut self,
        host: &HostConfig,
        id: ForwardId,
        cx: &mut Context<Self>,
    ) {
        self.open_forward_connection_many(host, vec![id], cx);
    }

    /// Dials a shell-less connection for one or more saved rules on the same host.
    fn open_forward_connection_many(
        &mut self,
        host: &HostConfig,
        ids: Vec<ForwardId>,
        cx: &mut Context<Self>,
    ) {
        self.forward_only_dial = true;
        self.pending_forward_rules = ids;
        self.park_focused_form();
        self.open_remote_after_park(host, cx);
    }

    /// Stops only the rules this session carries.
    pub(super) fn stop_session_forwards(&mut self, id: SessionId) {
        let _ = self.forwards.stop_session(id);
    }

    pub(super) fn carrier_for_host(&self, host: HostId) -> Option<SessionId> {
        self.workspace
            .sessions()
            .items()
            .iter()
            .find_map(|session| {
                let SessionKind::Remote { host: saved } = session.kind() else {
                    return None;
                };
                if *saved == host
                    && matches!(session.state(), SessionState::Connected)
                    // A forwarding listener is an SSH capability; asking the set keeps a
                    // protocol without one from ever being offered as its carrier (R18).
                    && self
                        .workspace
                        .capabilities(session.id())
                        .allows(shellune_core::Capability::Forwarding)
                    && self.forwards.has_carrier(session.id())
                {
                    Some(session.id())
                } else {
                    None
                }
            })
    }

    pub(super) fn watch_forwards(&mut self, cx: &mut Context<Self>) {
        if self.forwards_polling {
            return;
        }
        self.forwards_polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(40))
                    .await;
                match this.update(cx, |view, cx| view.pump_forwards(cx)) {
                    Ok(true) => {}
                    _ => break,
                }
            }
        })
        .detach();
    }

    fn pump_forwards(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = self.note_dead_forward_carriers();
        let before = self.forwards.carrier_sessions();
        if self.forwards.pump() {
            changed = true;
        }
        for id in before {
            if !self.forwards.has_carrier(id) && self.forward_carriers.contains_key(&id) {
                self.forward_carriers.remove(&id);
                let _ = self
                    .workspace
                    .session_failed(id, shellune_core::SessionFailure::ConnectionLost);
                changed = true;
            }
        }
        if changed {
            self.sync_forward_section();
            cx.notify();
        }
        if !self.forwards.has_carriers() && self.forward_carriers.is_empty() {
            self.forwards_polling = false;
            return false;
        }
        true
    }

    /// A pure-forward connection that ended is no longer a carrier.
    fn note_dead_forward_carriers(&mut self) -> bool {
        let ids: Vec<SessionId> = self.forward_carriers.keys().copied().collect();
        let mut changed = false;
        for id in ids {
            let dead = self.forward_carriers.get_mut(&id).is_some_and(|link| {
                link.drain().iter().any(|event| {
                    matches!(
                        event,
                        crate::app::remote::LinkEvent::Exited { .. }
                            | crate::app::remote::LinkEvent::FailedRemote(_)
                            | crate::app::remote::LinkEvent::FailedChain(_)
                    )
                })
            });
            if dead {
                self.forwards.detach(id);
                self.forward_carriers.remove(&id);
                let _ = self
                    .workspace
                    .session_failed(id, shellune_core::SessionFailure::ConnectionLost);
                changed = true;
            }
        }
        changed
    }

    /// Starts rules that were waiting for this pure-forward connection.
    pub(super) fn start_pending_forwards(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let pending = std::mem::take(&mut self.pending_forward_rules);
        for id in pending {
            let Some(rule) = self.forwards.book().rule(id).cloned() else {
                continue;
            };
            // Exposure was already accepted before this connection was dialled.
            // Re-arm it: the first confirmation was spent when the dial was chosen.
            if rule.needs_exposure_confirmation() {
                self.forwards.confirm_exposure(id);
            }
            match self.forwards.prepare_start(id, Some(session)) {
                Ok(ForwardStart::Ready { session, spec }) => {
                    let _ = self.forwards.send(session, ForwardCommand::Start(spec));
                }
                Ok(ForwardStart::Exposure { id, .. }) => {
                    self.pending_exposure = Some(id);
                }
                Ok(ForwardStart::Connection { .. }) | Err(_) => {}
            }
        }
        self.watch_forwards(cx);
        self.sync_forward_section();
    }

    /// The port-forwarding dialog's content: the form when one is open, the
    /// search/filter bar, batch controls, and one card per saved tunnel (with
    /// stop/delete/exposure confirmation rendered directly beneath its target row).
    pub(super) fn forward_rows(&self, list: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let mut list = list;
        if let Some(error) = self.forwards.load_error() {
            list = list.child(
                components::banner(Tone::Danger, metrics)
                    .debug_selector(|| "forwards.load".to_owned())
                    .child(SharedString::from(error.to_owned())),
            );
        }
        if let Some(error) = self.forwards.action_error() {
            list = list.child(
                components::banner(Tone::Danger, metrics)
                    .debug_selector(|| "forwards.action_error".to_owned())
                    .justify_between()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_normal()
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(SharedString::from(error.to_owned())),
                    )
                    .child(
                        components::icon_button("×", metrics)
                            .id("forwards.action_error.dismiss")
                            .debug_selector(|| "forwards.action_error.dismiss".to_owned())
                            .on_click(cx.listener(|view, _event, _window, cx| {
                                view.forwards.clear_action_error();
                                cx.notify();
                            })),
                    ),
            );
        }
        if let Some(editor) = self.forwards.editor().cloned() {
            return list.child(self.forward_form(&editor, cx));
        }

        let all_saved = self.forwards.book().rules();
        let no_hosts = self.book.hosts().is_empty();
        if all_saved.is_empty() && self.pending_exposure.is_none() {
            // The shared empty state: what a tunnel is and the one way to make one
            // (R6). With no host there is nothing to tunnel through, so it says that
            // and offers nothing.
            let action = (!no_hosts).then(|| {
                self.icon_text_button(
                    SharedString::from("forwards.new"),
                    Icon::Plus,
                    ux.fwd_new,
                    Variant::Primary,
                    cx,
                    |view, cx| view.open_forward_editor(cx),
                )
                .h(metrics.control_height())
                .into_any_element()
            });
            return list.child(
                components::empty_state(
                    Icon::Forward,
                    ux.fwd_empty_title,
                    Some(SharedString::from(if no_hosts {
                        ux.fwd_no_hosts
                    } else {
                        ux.fwd_empty_body
                    })),
                    action,
                    metrics,
                )
                .debug_selector(|| "forwards.empty".to_owned()),
            );
        }

        // Top toolbar: search input + New tunnel button.
        let mut toolbar = div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(metrics.scale(spacing::TIGHT))
            .child(
                div()
                    .flex_1()
                    .min_w(metrics.scale(FORWARD_SEARCH_MIN_WIDTH))
                    .debug_selector(|| "forwards.search".to_owned())
                    .child(self.forward_search.clone()),
            );
        let dropped_count = self.forwards.dropped_rules().len();
        if dropped_count > 0 {
            toolbar = toolbar.child(
                self.icon_text_button(
                    SharedString::from("forwards.restart_dropped"),
                    Icon::Refresh,
                    copy::fill(
                        ux.fwd_restart_dropped,
                        &[("count", &dropped_count.to_string())],
                    ),
                    Variant::Secondary,
                    cx,
                    |view, cx| view.restart_dropped_forwards(cx),
                )
                .h(metrics.control_height()),
            );
        }
        if !no_hosts {
            toolbar = toolbar.child(
                self.icon_text_button(
                    SharedString::from("forwards.new"),
                    Icon::Plus,
                    ux.fwd_new,
                    Variant::Primary,
                    cx,
                    |view, cx| view.open_forward_editor(cx),
                )
                .h(metrics.control_height()),
            );
        }
        list = list.child(toolbar);

        // Status filter segments + host filter chips + per-host Start all / Stop all.
        let active_status = self.forwards.status_filter();
        let mut status_bar =
            components::segmented(metrics).debug_selector(|| "forwards.filters.status".to_owned());
        for filter in ForwardStatusFilter::ALL {
            let (selector, label) = match filter {
                ForwardStatusFilter::All => ("forwards.filter.all", ux.fwd_filter_all),
                ForwardStatusFilter::Active => ("forwards.filter.active", ux.fwd_filter_active),
                ForwardStatusFilter::Inactive => {
                    ("forwards.filter.inactive", ux.fwd_filter_inactive)
                }
                ForwardStatusFilter::Failed => ("forwards.filter.failed", ux.fwd_filter_failed),
            };
            let selected = active_status == filter;
            status_bar = status_bar.child(
                components::segment(label, selected, metrics)
                    .id(selector)
                    .debug_selector(move || selector.to_owned())
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        view.forwards.set_status_filter(filter);
                        cx.notify();
                    })),
            );
        }

        // Distinct hosts present in saved rules, preserving order.
        let mut rule_hosts: Vec<HostId> = Vec::new();
        for rule in all_saved {
            if !rule_hosts.contains(&rule.host()) {
                rule_hosts.push(rule.host());
            }
        }
        let target_batch_host = match self.forwards.scope() {
            ForwardScope::Host(id) => Some(id),
            ForwardScope::All if rule_hosts.len() == 1 => rule_hosts.first().copied(),
            ForwardScope::All => None,
        };

        let mut filter_row = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_wrap()
            .gap(metrics.scale(spacing::TIGHT))
            .child(status_bar);

        if let Some(host_id) = target_batch_host {
            let has_startable = !self.forwards.startable_ids_for_host(host_id).is_empty();
            let has_active = !self.forwards.active_ids_for_host(host_id).is_empty();
            if has_startable || has_active {
                let mut batch_actions = div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::HALF));
                if has_startable {
                    batch_actions = batch_actions.child(
                        self.icon_text_button(
                            SharedString::from("forwards.host.start_all"),
                            Icon::Play,
                            ux.fwd_start_all,
                            Variant::Secondary,
                            cx,
                            move |view, cx| view.start_host_forwards(host_id, cx),
                        )
                        .h(metrics.compact_control_height()),
                    );
                }
                if has_active {
                    batch_actions = batch_actions.child(
                        self.icon_text_button(
                            SharedString::from("forwards.host.stop_all"),
                            Icon::Stop,
                            ux.fwd_stop_all,
                            Variant::Ghost,
                            cx,
                            move |view, cx| view.stop_host_forwards(host_id, cx),
                        )
                        .h(metrics.compact_control_height()),
                    );
                }
                filter_row = filter_row.child(batch_actions);
            }
        }
        list = list.child(filter_row);

        // Host scope chips when rules span multiple hosts (or when scoped to one host).
        if rule_hosts.len() > 1 || matches!(self.forwards.scope(), ForwardScope::Host(_)) {
            let mut host_bar = components::segmented(metrics)
                .flex_wrap()
                .debug_selector(|| "forwards.filters.host".to_owned());
            let all_selected = self.forwards.scope() == ForwardScope::All;
            host_bar = host_bar.child(
                components::segment(ux.fwd_filter_all_hosts, all_selected, metrics)
                    .id("forwards.host_filter.all")
                    .debug_selector(|| "forwards.host_filter.all".to_owned())
                    .on_click(cx.listener(|view, _event, _window, cx| {
                        view.forwards.set_scope(ForwardScope::All);
                        cx.notify();
                    })),
            );
            for (h_index, host_id) in rule_hosts.iter().copied().enumerate() {
                let name = self
                    .book
                    .host(host_id)
                    .map_or_else(|| "?".to_owned(), |host| host.name().to_owned());
                let selected = self.forwards.scope() == ForwardScope::Host(host_id);
                host_bar = host_bar.child(
                    components::segment(name, selected, metrics)
                        .id(SharedString::from(format!(
                            "forwards.host_filter.{h_index}"
                        )))
                        .debug_selector(move || format!("forwards.host_filter.{h_index}"))
                        .on_click(cx.listener(move |view, _event, _window, cx| {
                            view.forwards.set_scope(ForwardScope::Host(host_id));
                            cx.notify();
                        })),
                );
            }
            list = list.child(host_bar);
        }

        let query = self.forward_search.read(cx).text().trim().to_lowercase();
        let rows: Vec<_> = self
            .forwards
            .rows()
            .into_iter()
            .filter(|row| active_status.matches(&row.phase))
            .filter(|row| {
                if query.is_empty() {
                    return true;
                }
                let host_name = self
                    .book
                    .host(row.rule.host())
                    .map(HostConfig::name)
                    .unwrap_or("");
                let host_addr = self
                    .book
                    .host(row.rule.host())
                    .map(HostConfig::address)
                    .unwrap_or("");
                let listen = displayed_listen(&row.rule, &row.phase);
                let target = row
                    .rule
                    .target()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "SOCKS".to_owned());
                let kind_text = kind_label(row.rule.kind(), strings);
                row.rule
                    .label()
                    .is_some_and(|label| label.to_lowercase().contains(&query))
                    || host_name.to_lowercase().contains(&query)
                    || host_addr.to_lowercase().contains(&query)
                    || listen.to_lowercase().contains(&query)
                    || target.to_lowercase().contains(&query)
                    || kind_text.to_lowercase().contains(&query)
                    || row.rule.kind().label().contains(&query)
            })
            .collect();

        // Fallback banners when the target rule of a pending exposure/confirm is not in the visible rows.
        let exposure_in_rows = self
            .pending_exposure
            .is_some_and(|id| rows.iter().any(|row| row.rule.id() == id));
        if let Some(id) = self.pending_exposure
            && !exposure_in_rows
        {
            list = list.child(self.exposure_question(id, cx));
        }
        let confirm_in_rows = self
            .forward_confirm
            .is_some_and(|(id, _)| rows.iter().any(|row| row.rule.id() == id));
        if let Some((id, question)) = self.forward_confirm
            && !confirm_in_rows
        {
            list = list.child(self.forward_question(id, question, cx));
        }

        if rows.is_empty() {
            let clear_action = Some(
                self.text_button(
                    SharedString::from("forwards.clear_filters"),
                    ux.fwd_clear_filters,
                    Variant::Secondary,
                    cx,
                    |view, cx| {
                        view.forward_search
                            .update(cx, |field, cx| field.set_text("", cx));
                        view.forwards.set_status_filter(ForwardStatusFilter::All);
                        view.forwards.set_scope(ForwardScope::All);
                        cx.notify();
                    },
                )
                .h(metrics.control_height())
                .into_any_element(),
            );
            return list.child(
                components::empty_state(
                    Icon::Search,
                    ux.fwd_filtered_empty_title,
                    Some(SharedString::from(ux.fwd_filtered_empty_body)),
                    clear_action,
                    metrics,
                )
                .debug_selector(|| "forwards.filtered_empty".to_owned()),
            );
        }

        let faults = self
            .forwards
            .book()
            .faults(|host| self.book.host(host).is_some());
        for (index, row) in rows.into_iter().enumerate() {
            let id = row.rule.id();
            let host_name = self
                .book
                .host(row.rule.host())
                .map(HostConfig::name)
                .unwrap_or("?");
            let kind = kind_label(row.rule.kind(), strings);
            let listen = displayed_listen(&row.rule, &row.phase);
            let target = row
                .rule
                .target()
                .map(ToString::to_string)
                .unwrap_or_else(|| "SOCKS".to_owned());
            let (status, tone, detail) = phase_text(&row.phase, row.attempt.as_ref(), strings);
            let active = row.phase.is_active();
            let listening = matches!(row.phase, ForwardPhase::Listening { .. });
            let fault = faults.iter().find_map(|fault| match fault {
                ForwardFault::MissingHost { rule, host, .. } if *rule == id => Some(copy::fill(
                    ux.fwd_missing_host,
                    &[("rule", &id.to_string()), ("host", &host.to_string())],
                )),
                _ => None,
            });
            let icon_tone = if fault.is_some() {
                Tone::Danger
            } else {
                match row.phase {
                    ForwardPhase::Listening { .. } => Tone::Positive,
                    ForwardPhase::Starting { .. } => Tone::Warning,
                    ForwardPhase::Failed { .. } => Tone::Danger,
                    ForwardPhase::Saved => Tone::Accent,
                    ForwardPhase::Stopped => Tone::Neutral,
                }
            };
            // Which side each endpoint is on is part of what a tunnel is: a local
            // forward listens here and reaches through the server, a remote one is the
            // other way round. Naming the sides makes the same "8080 -> 5432" text mean
            // the right thing in both rows (R6).
            let mapping = match row.rule.kind() {
                ForwardKind::Local => format!(
                    "{} {listen}  \u{2192}  {} {target}",
                    ux.fwd_here, ux.fwd_there
                ),
                ForwardKind::Remote => format!(
                    "{} {listen}  \u{2192}  {} {target}",
                    ux.fwd_there, ux.fwd_here
                ),
                ForwardKind::Dynamic => format!("{} {listen} · SOCKS", ux.fwd_here),
            };
            let title_row = if let Some(label) = row.rule.label() {
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .text_size(metrics.body_size())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .debug_selector(move || format!("forwards.label.{index}"))
                            .child(SharedString::from(label.to_owned())),
                    )
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(format!("{kind} · {host_name}"))),
                    )
                    .child(
                        components::chip(status, tone, metrics).when(listening, move |chip| {
                            chip.debug_selector(move || format!("forwards.listening.{index}"))
                        }),
                    )
            } else {
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .text_size(metrics.body_size())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(SharedString::from(format!("{kind} · {host_name}"))),
                    )
                    .child(
                        components::chip(status, tone, metrics).when(listening, move |chip| {
                            chip.debug_selector(move || format!("forwards.listening.{index}"))
                        }),
                    )
            };
            list = list.child(
                // A tunnel is one row of the shared list kind, so the list reads like
                // every other list of saved things (R6, ADR-2).
                // The row wraps: when the dialog is too narrow for the words and the
                // commands (the minimum window at 200% text), the commands take a line
                // of their own under the words instead of drawing over them.
                components::list_row_frame(ControlState::Resting, metrics)
                    .flex_wrap()
                    .debug_selector(move || format!("forwards.row.{index}"))
                    .child(components::leading_icon(Icon::Forward, icon_tone, metrics))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(metrics.scale(FORWARD_ROW_WORDS_MIN))
                            .gap(metrics.scale(spacing::HALF))
                            .child(title_row)
                            .child(
                                components::copy_on_click(
                                    div()
                                        .truncate()
                                        .font_family(crate::app::terminal::monospace_family())
                                        .text_size(metrics.label_size())
                                        .text_color(theme::swatch(palette::TEXT_SECONDARY)),
                                    listen.clone(),
                                )
                                .child(SharedString::from(mapping)),
                            )
                            .children(detail.map(|detail| {
                                div()
                                    .text_size(metrics.caption_size())
                                    .text_color(Tone::Danger.text())
                                    .child(SharedString::from(detail))
                            }))
                            .children(fault.map(|message| {
                                div()
                                    .text_size(metrics.caption_size())
                                    .text_color(Tone::Danger.text())
                                    .debug_selector(move || format!("forwards.fault.{index}"))
                                    .child(SharedString::from(message))
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(metrics.scale(spacing::HALF))
                            .child(
                                self.icon_text_button(
                                    SharedString::from(format!("forwards.start.{index}")),
                                    if active { Icon::Stop } else { Icon::Play },
                                    if active {
                                        strings.forward_stop
                                    } else {
                                        strings.forward_start
                                    },
                                    if active {
                                        Variant::Secondary
                                    } else {
                                        Variant::Primary
                                    },
                                    cx,
                                    move |view, cx| {
                                        if active {
                                            view.ask_forward(id, ForwardConfirm::Stop, cx);
                                        } else {
                                            view.start_forward(id, cx);
                                        }
                                    },
                                )
                                .mr(metrics.scale(spacing::HAIRLINE)),
                            )
                            .child(self.row_icon_button(
                                SharedString::from(format!("forwards.edit.{index}")),
                                Icon::Pencil,
                                strings.forward_edit,
                                false,
                                cx,
                                move |view, cx| view.edit_forward(id, cx),
                            ))
                            .child(self.row_icon_button(
                                SharedString::from(format!("forwards.copy.{index}")),
                                Icon::Copy,
                                ux.fwd_more,
                                false,
                                cx,
                                move |view, cx| view.copy_forward(id, cx),
                            ))
                            .child(self.row_icon_button(
                                SharedString::from(format!("forwards.delete.{index}")),
                                Icon::Trash,
                                strings.forward_delete,
                                true,
                                cx,
                                move |view, cx| view.ask_forward(id, ForwardConfirm::Delete, cx),
                            )),
                    ),
            );
            // Render exposure or stop/delete confirmation directly below the target row
            // so it stays in view regardless of scroll position.
            if self.pending_exposure == Some(id) {
                list = list.child(self.exposure_question(id, cx));
            }
            if let Some((confirm_id, question)) = self.forward_confirm
                && confirm_id == id
            {
                list = list.child(self.forward_question(id, question, cx));
            }
        }
        list
    }

    /// Puts a saved rule into the listening phase and returns the link's commands.
    ///
    /// The rule is inserted when it is not already in the book. Nothing is bound.
    #[cfg(test)]
    pub fn install_listening_forward_for_test(
        &mut self,
        session: SessionId,
        rule: ForwardRule,
        host: &str,
        port: u16,
    ) -> shellune_ssh::forwarding::ObservedForwards {
        let id = rule.id();
        if self.forwards.book().rule(id).is_none() {
            let mut book = self.forwards.book().clone();
            book.insert(rule).expect("the test rule is new");
            self.forwards.replace_book(book);
        }
        self.forwards
            .note_listening_for_test(id, session, host, port);
        self.forwards.attach_observed_for_test(session)
    }

    /// Whether the rule's phase is listening.
    #[cfg(test)]
    pub fn forward_listening_for_test(&self, id: ForwardId) -> bool {
        self.forwards.is_listening_for_test(id)
    }

    /// Status and listen text the row draws for one saved rule.
    #[cfg(test)]
    pub fn forward_row_summary_for_test(&self, index: usize) -> Option<String> {
        let row = self.forwards.rows().into_iter().nth(index)?;
        let (status, _, _) = phase_text(&row.phase, row.attempt.as_ref(), self.strings());
        let listen = displayed_listen(&row.rule, &row.phase);
        Some(format!("{status} {listen}"))
    }

    /// The missing-host fault the row draws, when this rule has one.
    #[cfg(test)]
    pub fn forward_fault_for_test(&self, index: usize) -> Option<String> {
        let id = self.forwards.rows().into_iter().nth(index)?.rule.id();
        let ux = copy::ux();
        self.forwards
            .book()
            .faults(|host| self.book.host(host).is_some())
            .into_iter()
            .find_map(|fault| match fault {
                ForwardFault::MissingHost { rule, host, .. } if rule == id => Some(copy::fill(
                    ux.fwd_missing_host,
                    &[("rule", &id.to_string()), ("host", &host.to_string())],
                )),
                _ => None,
            })
    }

    /// Opens the editor on one rule, for a test.
    #[cfg(test)]
    pub fn edit_forward_for_test(&mut self, id: ForwardId, cx: &mut Context<Self>) {
        self.edit_forward(id, cx);
    }

    /// Replaces the listen fields, for a test.
    #[cfg(test)]
    pub fn set_forward_listen_for_test(&mut self, host: &str, port: &str, cx: &mut Context<Self>) {
        self.forward_listen_host
            .update(cx, |field, cx| field.set_text(host, cx));
        self.forward_listen_port
            .update(cx, |field, cx| field.set_text(port, cx));
    }

    /// The stop or delete the user is being asked about, for a test.
    #[cfg(test)]
    pub fn forward_confirm_for_test(&self) -> Option<(ForwardId, ForwardConfirm)> {
        self.forward_confirm
    }

    /// The kind the open tunnel form has chosen, for a test.
    #[cfg(test)]
    pub fn forward_editor_kind_for_test(&self) -> Option<ForwardKind> {
        self.forwards.editor().map(|editor| editor.kind)
    }

    /// Saves the open forward form, for a test.
    #[cfg(test)]
    pub fn save_forward_for_test(&mut self, cx: &mut Context<Self>) {
        self.save_forward(cx);
    }

    /// Sets the label field on the forward form, for a test.
    #[cfg(test)]
    pub fn set_forward_label_for_test(&mut self, label: &str, cx: &mut Context<Self>) {
        self.forward_label
            .update(cx, |field, cx| field.set_text(label, cx));
    }

    /// Sets the target fields on the forward form, for a test.
    #[cfg(test)]
    pub fn set_forward_target_for_test(&mut self, host: &str, port: &str, cx: &mut Context<Self>) {
        self.forward_target_host
            .update(cx, |field, cx| field.set_text(host, cx));
        self.forward_target_port
            .update(cx, |field, cx| field.set_text(port, cx));
    }

    /// Sets the forwards search box text, for a test.
    #[cfg(test)]
    pub fn set_forward_search_for_test(&mut self, query: &str, cx: &mut Context<Self>) {
        self.forward_search
            .update(cx, |field, cx| field.set_text(query, cx));
        cx.notify();
    }

    /// The current list-level action error on the forwards page, for a test.
    #[cfg(test)]
    pub fn forward_action_error_for_test(&self) -> Option<String> {
        self.forwards.action_error().map(str::to_owned)
    }

    /// Marks one saved rule as carrier-lost, for a test.
    #[cfg(test)]
    pub fn note_forward_carrier_lost_for_test(&mut self, id: ForwardId, cx: &mut Context<Self>) {
        self.forwards.note_carrier_lost_for_test(id);
        cx.notify();
    }

    /// Number of forward rules matching the current scope, status filter, and search query, for a test.
    #[cfg(test)]
    pub fn forward_visible_count_for_test(&self, cx: &App) -> usize {
        let active_status = self.forwards.status_filter();
        let query = self.forward_search.read(cx).text().trim().to_lowercase();
        let strings = self.strings();
        self.forwards
            .rows()
            .into_iter()
            .filter(|row| active_status.matches(&row.phase))
            .filter(|row| {
                if query.is_empty() {
                    return true;
                }
                let host_name = self
                    .book
                    .host(row.rule.host())
                    .map(HostConfig::name)
                    .unwrap_or("");
                let host_addr = self
                    .book
                    .host(row.rule.host())
                    .map(HostConfig::address)
                    .unwrap_or("");
                let listen = displayed_listen(&row.rule, &row.phase);
                let target = row
                    .rule
                    .target()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "SOCKS".to_owned());
                let kind_text = kind_label(row.rule.kind(), strings);
                row.rule
                    .label()
                    .is_some_and(|label| label.to_lowercase().contains(&query))
                    || host_name.to_lowercase().contains(&query)
                    || host_addr.to_lowercase().contains(&query)
                    || listen.to_lowercase().contains(&query)
                    || target.to_lowercase().contains(&query)
                    || kind_text.to_lowercase().contains(&query)
                    || row.rule.kind().label().contains(&query)
            })
            .count()
    }

    /// Asks before a stop or a delete, without doing anything yet (R6).
    ///
    /// One click on a row used to close a listener or remove a rule. Both end something
    /// the user can see, so both are asked about, and the question names the tunnel, the
    /// side its port is on and what the answer will do.
    pub(super) fn ask_forward(
        &mut self,
        id: ForwardId,
        question: ForwardConfirm,
        cx: &mut Context<Self>,
    ) {
        self.forward_confirm = Some((id, question));
        cx.notify();
    }

    /// Puts the question away without stopping or deleting anything.
    pub(super) fn cancel_forward_confirm(&mut self, cx: &mut Context<Self>) {
        if self.forward_confirm.take().is_some() {
            cx.notify();
        }
    }

    /// Carries out the action the question asked about.
    pub(super) fn confirm_forward(&mut self, cx: &mut Context<Self>) {
        let Some((id, question)) = self.forward_confirm.take() else {
            return;
        };
        match question {
            ForwardConfirm::Stop => self.stop_forward(id, cx),
            ForwardConfirm::Delete => self.delete_forward(id, cx),
        }
        cx.notify();
    }

    /// The question before a stop or a delete: what it is, and what the answer does.
    fn forward_question(
        &self,
        id: ForwardId,
        question: ForwardConfirm,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let Some(rule) = self.forwards.book().rule(id) else {
            // The rule was removed while the question was on screen; the answer has
            // nothing left to name.
            return div();
        };
        let listen = self
            .forwards
            .rows()
            .into_iter()
            .find(|row| row.rule.id() == id)
            .map(|row| {
                let active = row.phase.is_active();
                (displayed_listen(&row.rule, &row.phase), active)
            })
            .unwrap_or_else(|| (rule.listen().to_string(), false));
        let host_name = self
            .book
            .host(rule.host())
            .map_or_else(|| "?".to_owned(), |host| host.name().to_owned());
        let endpoint = match rule.kind() {
            ForwardKind::Local => format!("{} {}", ux.fwd_here, listen.0),
            ForwardKind::Remote => format!("{} {}", ux.fwd_there, listen.0),
            ForwardKind::Dynamic => format!("{} {} (SOCKS)", ux.fwd_here, listen.0),
        };
        let (title, body, confirm_label, confirm_variant) = match question {
            ForwardConfirm::Stop => (
                ux.fwd_stop_title,
                copy::fill(ux.fwd_stop_body, &[("endpoint", &endpoint)]),
                ux.fwd_stop_action,
                Variant::Primary,
            ),
            ForwardConfirm::Delete => {
                let rule_text = format!(
                    "{} · {} · {endpoint}",
                    kind_label(rule.kind(), strings),
                    host_name
                );
                let running = if listen.1 {
                    ux.fwd_delete_running
                } else {
                    ux.fwd_delete_saved
                };
                (
                    ux.fwd_delete_title,
                    copy::fill(
                        ux.fwd_delete_body,
                        &[("rule", &rule_text), ("running", running)],
                    ),
                    ux.fwd_delete_action,
                    Variant::Danger,
                )
            }
        };
        // The question is an inline form above the rows it is about (ADR-7): stopping
        // asks in the warning tone, deleting in the danger tone.
        let (tone, icon) = match question {
            ForwardConfirm::Stop => (Tone::Warning, Icon::Stop),
            ForwardConfirm::Delete => (Tone::Danger, Icon::Trash),
        };
        components::inline_form(tone, metrics)
            .debug_selector(|| "forwards.confirm".to_owned())
            .child(components::form_heading(tone, icon, title, metrics))
            .child(
                div()
                    .whitespace_normal()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .debug_selector(|| "forwards.confirm.body".to_owned())
                    .child(SharedString::from(body)),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(self.text_button(
                        SharedString::from("forwards.confirm.no"),
                        strings.files_cancel,
                        Variant::Secondary,
                        cx,
                        |view, cx| view.cancel_forward_confirm(cx),
                    ))
                    .child(self.text_button(
                        SharedString::from("forwards.confirm.yes"),
                        confirm_label,
                        confirm_variant,
                        cx,
                        |view, cx| view.confirm_forward(cx),
                    )),
            )
    }
    fn exposure_question(&self, id: ForwardId, cx: &mut Context<Self>) -> gpui::Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let listen = self
            .forwards
            .book()
            .rule(id)
            .map(|rule| rule.listen().to_string())
            .unwrap_or_default();
        let body = self
            .forwards
            .book()
            .rule(id)
            .map(|rule| match rule.listen().bind_exposure() {
                BindExposure::AllInterfaces => strings.forward_exposure_all,
                _ => strings.forward_exposure_named,
            })
            .unwrap_or(strings.forward_exposure_named);
        components::inline_form(Tone::Warning, metrics)
            .debug_selector(|| "forwards.exposure".to_owned())
            .child(components::form_heading(
                Tone::Warning,
                Icon::Globe,
                strings.forward_exposure_title,
                metrics,
            ))
            .child(
                div()
                    .whitespace_normal()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(SharedString::from(format!("{listen} · {body}"))),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(self.text_button(
                        SharedString::from("forwards.exposure.cancel"),
                        strings.forward_exposure_cancel,
                        Variant::Secondary,
                        cx,
                        |view, cx| view.cancel_forward_exposure(cx),
                    ))
                    .child(self.text_button(
                        SharedString::from("forwards.exposure.confirm"),
                        strings.forward_exposure_confirm,
                        Variant::Danger,
                        cx,
                        |view, cx| view.confirm_forward_exposure(cx),
                    )),
            )
    }

    /// The tunnel form: what kind, through which host, what to listen on and where
    /// it leads, with a sentence that says what the result will do.
    fn forward_form(&self, editor: &ForwardEditor, cx: &mut Context<Self>) -> gpui::Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let kinds = [
            ForwardKind::Local,
            ForwardKind::Remote,
            ForwardKind::Dynamic,
        ];
        let kind_control = self.segments(
            "forwards.kind",
            kinds
                .iter()
                .map(|kind| (kind_label(*kind, strings).to_owned(), *kind == editor.kind))
                .collect(),
            cx,
            move |view, index, cx| {
                if let Some(editor) = view.forwards.editor_mut() {
                    editor.kind = kinds[index];
                }
                cx.notify();
            },
        );
        // A searchable list instead of one segment per host: a book of any size fits
        // the form (ADR-7).
        let host_control = self.forward_host_control(editor, cx);
        let hint = match editor.kind {
            ForwardKind::Local => ux.fwd_kind_local_hint,
            ForwardKind::Remote => ux.fwd_kind_remote_hint,
            ForwardKind::Dynamic => ux.fwd_kind_dynamic_hint,
        };
        let (listen_label, target_label) = match editor.kind {
            ForwardKind::Remote => (ux.fwd_listen_remote, ux.fwd_target_local),
            _ => (ux.fwd_listen_local, ux.fwd_target_remote),
        };
        let host_name = editor
            .host
            .and_then(|id| self.book.host(id))
            .map(HostConfig::name)
            .unwrap_or("—")
            .to_owned();

        let field_label = |text: &'static str| components::form_label(text, metrics);
        let pair = |label: &'static str,
                    address: gpui::Entity<crate::app::text_field::TextField>,
                    port: gpui::Entity<crate::app::text_field::TextField>,
                    selector: &'static str| {
            div()
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::NARROW))
                .debug_selector(move || selector.to_owned())
                .child(field_label(label))
                .child(
                    div()
                        .flex()
                        .gap(metrics.scale(spacing::TIGHT))
                        .child(div().flex_1().min_w_0().child(address))
                        .child(
                            div()
                                .w(metrics.scale(FORWARD_PORT_WIDTH))
                                .flex_none()
                                .child(port),
                        ),
                )
        };

        let listen_text = format!(
            "{}:{}",
            self.field_text_or(&self.forward_listen_host, "127.0.0.1", cx),
            self.field_text_or(&self.forward_listen_port, "8080", cx)
        );
        let target_text = format!(
            "{}:{}",
            self.field_text_or(&self.forward_target_host, "db.internal", cx),
            self.field_text_or(&self.forward_target_port, "5432", cx)
        );
        let preview = copy::fill(
            match editor.kind {
                ForwardKind::Local => ux.fwd_preview_local,
                ForwardKind::Remote => ux.fwd_preview_remote,
                ForwardKind::Dynamic => ux.fwd_preview_dynamic,
            },
            &[
                ("listen", &listen_text),
                ("target", &target_text),
                ("host", &host_name),
            ],
        );

        // The tunnel form is the dialog's inline form (ADR-7): a heading with the tunnel
        // icon, one labelled field per line, the sentence it will do and the answers at
        // its foot.
        let mut form = components::inline_form(Tone::Accent, metrics)
            .gap(metrics.scale(spacing::BASE))
            .p(metrics.scale(spacing::BASE))
            .debug_selector(|| "forwards.form".to_owned())
            .child(components::form_heading(
                Tone::Accent,
                Icon::Forward,
                if editor.id.is_some() {
                    ux.fwd_edit
                } else {
                    ux.fwd_new
                },
                metrics,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(spacing::NARROW))
                    .child(field_label(ux.fwd_type))
                    .child(div().flex().child(kind_control))
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(hint),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(spacing::NARROW))
                    .child(field_label(ux.fwd_through))
                    .child(host_control),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(spacing::NARROW))
                    .debug_selector(|| "forwards.label".to_owned())
                    .child(field_label(ux.fwd_label))
                    .child(self.forward_label.clone()),
            )
            .child(pair(
                listen_label,
                self.forward_listen_host.clone(),
                self.forward_listen_port.clone(),
                "forwards.listen",
            ));
        if editor.kind != ForwardKind::Dynamic {
            form = form.child(pair(
                target_label,
                self.forward_target_host.clone(),
                self.forward_target_port.clone(),
                "forwards.target",
            ));
        }
        form = form.child(
            components::banner(Tone::Accent, metrics)
                .debug_selector(|| "forwards.preview".to_owned())
                .text_size(metrics.label_size())
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .child(SharedString::from(preview)),
        );
        if let Some(error) = &editor.error {
            form = form.child(
                components::banner(Tone::Danger, metrics)
                    .debug_selector(|| "forwards.error".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(error.clone())),
            );
        }
        form.child(
            div()
                .flex()
                .justify_end()
                .flex_wrap()
                .gap(metrics.scale(spacing::TIGHT))
                .child(
                    self.text_button(
                        SharedString::from("forwards.form.close"),
                        ux.fwd_cancel,
                        Variant::Ghost,
                        cx,
                        |view, cx| {
                            view.forwards.close_editor();
                            cx.notify();
                        },
                    )
                    .h(metrics.control_height()),
                )
                .child(
                    self.text_button(
                        SharedString::from("forwards.save"),
                        strings.forward_save,
                        Variant::Secondary,
                        cx,
                        |view, cx| view.save_forward(cx),
                    )
                    .h(metrics.control_height()),
                )
                .child(
                    self.icon_text_button(
                        SharedString::from("forwards.save_start"),
                        Icon::Play,
                        ux.fwd_save_and_start,
                        Variant::Primary,
                        cx,
                        |view, cx| view.save_and_start_forward(cx),
                    )
                    .h(metrics.control_height()),
                ),
        )
    }

    /// A field's text, or the example it shows while empty.
    fn field_text_or(
        &self,
        field: &gpui::Entity<crate::app::text_field::TextField>,
        example: &str,
        cx: &Context<Self>,
    ) -> String {
        let text = field.read(cx).text().trim().to_owned();
        if text.is_empty() {
            example.to_owned()
        } else {
            text
        }
    }
}

fn displayed_listen(rule: &ForwardRule, phase: &ForwardPhase) -> String {
    match phase {
        ForwardPhase::Listening { host, port, .. } => {
            if host.contains(':') {
                format!("[{host}]:{port}")
            } else {
                format!("{host}:{port}")
            }
        }
        _ => rule.listen().to_string(),
    }
}

fn kind_label(kind: ForwardKind, _strings: &Strings) -> &'static str {
    let ux = copy::ux();
    match kind {
        ForwardKind::Local => ux.fwd_kind_local,
        ForwardKind::Remote => ux.fwd_kind_remote,
        ForwardKind::Dynamic => ux.fwd_kind_dynamic,
    }
}

fn phase_text(
    phase: &ForwardPhase,
    attempt: Option<&ForwardFailure>,
    strings: &Strings,
) -> (&'static str, Tone, Option<String>) {
    let (label, tone) = match phase {
        ForwardPhase::Saved => (strings.forward_saved, Tone::Neutral),
        ForwardPhase::Starting { .. } => (strings.forward_starting, Tone::Warning),
        ForwardPhase::Listening { .. } => (strings.forward_listening, Tone::Positive),
        ForwardPhase::Stopped => (strings.forward_stopped, Tone::Neutral),
        ForwardPhase::Failed { .. } => (strings.forward_failed, Tone::Danger),
    };
    let detail = match phase {
        ForwardPhase::Failed { failure, .. } => Some(failure_text(failure, strings)),
        _ => attempt.map(|failure| failure_text(failure, strings)),
    };
    (label, tone, detail)
}

/// Why a tunnel is not listening, in the interface language.
///
/// The transport words its own failures in English and has no idea which language the
/// window is drawn in. The reasons a user meets most - a port another program holds, a
/// port this account may not bind, a server that refused, a target that is not there -
/// are said here, with the endpoint filled in. A reason this table does not know is
/// printed as it came, so a new failure is never hidden (R6).
fn failure_text(failure: &ForwardFailure, strings: &Strings) -> String {
    let ux = copy::ux();
    match failure {
        ForwardFailure::CarrierLost => strings.forward_carrier_lost.to_owned(),
        ForwardFailure::PortInUse { host, port } => copy::fill(
            ux.fwd_failure_in_use,
            &[("host", host), ("port", &port.to_string())],
        ),
        ForwardFailure::PermissionDenied { host, port } => copy::fill(
            ux.fwd_failure_denied,
            &[("host", host), ("port", &port.to_string())],
        ),
        ForwardFailure::ServerRefused { detail } => {
            copy::fill(ux.fwd_failure_server, &[("detail", detail)])
        }
        ForwardFailure::TargetUnreachable { host, port, detail } => copy::fill(
            ux.fwd_failure_target,
            &[
                ("host", host),
                ("port", &port.to_string()),
                ("detail", detail),
            ],
        ),
        other => other.to_string(),
    }
}
