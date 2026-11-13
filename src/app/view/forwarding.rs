//! The forwarding page: saved rules, their real runtime, and the form (R11).

use gpui::prelude::*;
use gpui::{Context, FontWeight, SharedString, Window, div, px};
use shellune_core::{
    BindExposure, ForwardFault, ForwardId, ForwardKind, ForwardRule, HostConfig, HostId, SessionId,
    SessionKind, SessionState,
};
use shellune_ssh::{ForwardCommand, ForwardFailure};
use shellune_storage::ForwardStore;

use super::ShelluneView;
use crate::app::components::{self, Variant};
use crate::app::copy::{self, Strings};
use crate::app::forwarding::{
    ForwardConfirm, ForwardEditor, ForwardPhase, ForwardScope, ForwardStart,
};
use crate::app::icons::{self, Icon};
use crate::app::theme::{self, Tone, palette, radius, spacing};

impl ShelluneView {
    pub(super) fn reload_forwards(&mut self) {
        let store = ForwardStore::at(self.store.root());
        match store.load() {
            Ok(book) => {
                self.forwards.replace_book(book);
                self.forwards.clear_load_error();
                self.sync_forward_section();
            }
            Err(error) => self.forwards.note_load_error(error.to_string()),
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
        self.fill_forward_fields("", "127.0.0.1", "", "", "", cx);
        cx.notify();
    }

    fn fill_forward_fields(
        &mut self,
        _label: &str,
        listen_host: &str,
        listen_port: &str,
        target_host: &str,
        target_port: &str,
        cx: &mut Context<Self>,
    ) {
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
        let target = rule.target();
        self.fill_forward_fields(
            "",
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

    pub(super) fn save_forward(&mut self, cx: &mut Context<Self>) {
        let listen_host = self.forward_listen_host.read(cx).text().to_owned();
        let listen_port = self.forward_listen_port.read(cx).text().to_owned();
        let target_host = self.forward_target_host.read(cx).text().to_owned();
        let target_port = self.forward_target_port.read(cx).text().to_owned();
        let book =
            match self
                .forwards
                .preview(&listen_host, &listen_port, &target_host, &target_port)
            {
                Ok(book) => book,
                Err(error) => {
                    self.forwards.fail_editor(error);
                    cx.notify();
                    return;
                }
            };
        let store = ForwardStore::at(self.store.root());
        if let Err(error) = store.save(&book) {
            self.forwards.fail_editor(error.to_string());
            cx.notify();
            return;
        }
        self.forwards.adopt(book);
        self.sync_forward_section();
        cx.notify();
    }

    pub(super) fn copy_forward(&mut self, id: ForwardId, cx: &mut Context<Self>) {
        let Some(rule) = self.forwards.copy_of(id) else {
            return;
        };
        let mut book = self.forwards.book().clone();
        if book.insert(rule).is_err() {
            return;
        }
        let store = ForwardStore::at(self.store.root());
        if store.save(&book).is_err() {
            return;
        }
        self.forwards.adopt(book);
        self.sync_forward_section();
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
        let Ok(book) = self.forwards.without(id) else {
            return;
        };
        if ForwardStore::at(self.store.root()).save(&book).is_err() {
            return;
        }
        self.forwards.adopt(book);
        self.sync_forward_section();
        cx.notify();
    }

    pub(super) fn start_forward(&mut self, id: ForwardId, cx: &mut Context<Self>) {
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
                    self.forwards
                        .fail_editor(format!("host {host} is not saved. Nothing was connected."));
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
            Err(error) => self.forwards.fail_editor(error),
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
        self.forward_only_dial = true;
        self.pending_forward_rules = vec![id];
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
    /// exposure question, and one card per saved tunnel.
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
        if let Some(id) = self.pending_exposure {
            list = list.child(self.exposure_question(id, cx));
        }
        if let Some((id, question)) = self.forward_confirm {
            list = list.child(self.forward_question(id, question, cx));
        }
        if let Some(editor) = self.forwards.editor().cloned() {
            return list.child(self.forward_form(&editor, cx));
        }

        let rows = self.forwards.rows();
        let no_hosts = self.book.hosts().is_empty();
        if rows.is_empty() && self.pending_exposure.is_none() {
            return list.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .py(metrics.scale(spacing::LOOSE))
                    .debug_selector(|| "forwards.empty".to_owned())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(metrics.scale(48.0))
                            .rounded(px(radius::XLARGE))
                            .bg(theme::swatch(palette::ACCENT_TINT))
                            .child(icons::icon(
                                Icon::Forward,
                                metrics.scale(22.0),
                                theme::swatch(palette::ACCENT),
                            )),
                    )
                    .child(
                        div()
                            .text_size(metrics.title_size())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(ux.fwd_empty_title),
                    )
                    .child(
                        div()
                            .max_w(metrics.scale(440.0))
                            .text_center()
                            .text_size(metrics.label_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(if no_hosts {
                                ux.fwd_no_hosts
                            } else {
                                ux.fwd_empty_body
                            }),
                    )
                    .when(!no_hosts, |column| {
                        column.child(
                            div().pt(metrics.scale(spacing::HAIRLINE)).child(
                                self.icon_text_button(
                                    SharedString::from("forwards.new"),
                                    Icon::Plus,
                                    ux.fwd_new,
                                    Variant::Primary,
                                    cx,
                                    |view, cx| view.open_forward_editor(cx),
                                )
                                .h(metrics.control_height()),
                            ),
                        )
                    }),
            );
        }

        if !no_hosts {
            list = list.child(div().flex().justify_end().child(self.icon_text_button(
                SharedString::from("forwards.new"),
                Icon::Plus,
                ux.fwd_new,
                Variant::Primary,
                cx,
                |view, cx| view.open_forward_editor(cx),
            )));
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
                ForwardFault::MissingHost { rule, message, .. } if *rule == id => {
                    Some(message.clone())
                }
                _ => None,
            });
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
            list = list.child(
                components_row(self, index, cx)
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(metrics.scale(36.0))
                            .rounded(px(radius::MEDIUM))
                            .bg(theme::swatch(if active {
                                palette::POSITIVE_TINT
                            } else {
                                palette::ACCENT_TINT
                            }))
                            .child(icons::icon(
                                Icon::Forward,
                                metrics.icon_size(),
                                theme::swatch(if active {
                                    palette::POSITIVE
                                } else {
                                    palette::ACCENT
                                }),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.0))
                            .gap(metrics.scale(3.0))
                            .child(
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
                                            .child(SharedString::from(format!(
                                                "{kind} · {host_name}"
                                            ))),
                                    )
                                    .child(components::chip(status, tone, metrics).when(
                                        listening,
                                        move |chip| {
                                            chip.debug_selector(move || {
                                                format!("forwards.listening.{index}")
                                            })
                                        },
                                    )),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .font_family(crate::app::terminal::monospace_family())
                                    .text_size(metrics.label_size())
                                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
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
                            .gap(metrics.scale(2.0))
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
        self.forwards
            .book()
            .faults(|host| self.book.host(host).is_some())
            .into_iter()
            .find_map(|fault| match fault {
                ForwardFault::MissingHost { rule, message, .. } if rule == id => Some(message),
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
        components::banner(Tone::Warning, metrics)
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(|| "forwards.confirm".to_owned())
            .child(SharedString::from(title))
            .child(
                div()
                    .whitespace_normal()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .debug_selector(|| "forwards.confirm.body".to_owned())
                    .child(SharedString::from(body)),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::HAIRLINE))
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
        crate::app::components::banner(Tone::Warning, metrics)
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(|| "forwards.exposure".to_owned())
            .child(strings.forward_exposure_title)
            .child(SharedString::from(format!("{listen} · {body}")))
            .child(
                div()
                    .flex()
                    .gap(metrics.scale(spacing::HAIRLINE))
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
        let hosts: Vec<(HostId, String)> = self
            .book
            .hosts()
            .iter()
            .map(|host| (host.id(), host.name().to_owned()))
            .collect();
        let ids: Vec<HostId> = hosts.iter().map(|(id, _)| *id).collect();
        let host_control = self.segments(
            "forwards.host",
            hosts
                .iter()
                .map(|(id, name)| (name.clone(), Some(*id) == editor.host))
                .collect(),
            cx,
            move |view, index, cx| {
                if let Some(editor) = view.forwards.editor_mut() {
                    editor.host = ids.get(index).copied();
                }
                cx.notify();
            },
        );
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

        let field_label = |text: &'static str| {
            div()
                .text_size(metrics.caption_size())
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .child(text)
        };
        let pair = |label: &'static str,
                    address: gpui::Entity<crate::app::text_field::TextField>,
                    port: gpui::Entity<crate::app::text_field::TextField>,
                    selector: &'static str| {
            div()
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::HAIRLINE + 2.0))
                .debug_selector(move || selector.to_owned())
                .child(field_label(label))
                .child(
                    div()
                        .flex()
                        .gap(metrics.scale(spacing::TIGHT))
                        .child(div().flex_1().min_w(px(0.0)).child(address))
                        .child(div().w(metrics.scale(120.0)).flex_none().child(port)),
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

        let mut form = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::BASE))
            .p(metrics.scale(spacing::BASE))
            .rounded(px(radius::LARGE))
            .bg(theme::swatch(palette::CANVAS))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(|| "forwards.form".to_owned())
            .child(
                div()
                    .text_size(metrics.title_size())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(if editor.id.is_some() {
                        ux.fwd_edit
                    } else {
                        ux.fwd_new
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(spacing::HAIRLINE + 2.0))
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
                    .gap(metrics.scale(spacing::HAIRLINE + 2.0))
                    .child(field_label(ux.fwd_through))
                    .child(div().flex().child(host_control)),
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
                        Variant::Primary,
                        cx,
                        |view, cx| view.save_forward(cx),
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

fn components_row(view: &ShelluneView, index: usize, _cx: &mut Context<ShelluneView>) -> gpui::Div {
    let metrics = view.metrics();
    div()
        .flex()
        .items_center()
        .gap(metrics.scale(spacing::SNUG))
        .p(metrics.scale(spacing::SNUG))
        .rounded(px(radius::LARGE))
        .bg(theme::swatch(palette::CANVAS))
        .border_1()
        .border_color(theme::swatch(palette::BORDER))
        .debug_selector(move || format!("forwards.row.{index}"))
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
