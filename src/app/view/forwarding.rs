//! The forwarding page: saved rules, their real runtime, and the form (R11).

use gpui::prelude::*;
use gpui::{Context, SharedString, Window, div, px};
use shellune_core::{
    BindExposure, ForwardId, ForwardKind, ForwardRule, HostConfig, HostId, SessionId, SessionKind,
    SessionState,
};
use shellune_ssh::{ForwardCommand, ForwardFailure};
use shellune_storage::ForwardStore;

use super::ShelluneView;
use crate::app::components::Variant;
use crate::app::copy::Strings;
use crate::app::forwarding::{ForwardEditor, ForwardPhase, ForwardScope, ForwardStart};
use crate::app::nav::Destination;
use crate::app::theme::{self, Tone, palette, spacing};

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
    pub(super) fn open_global_forwards(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.forwards.set_scope(ForwardScope::All);
        self.show_forwards_scope(ForwardScope::All, cx);
        self.select(Destination::Forwards, window, cx);
    }

    /// Opens the rules of one host or session. Nothing listens.
    pub(super) fn show_forwards_scope(&mut self, scope: ForwardScope, cx: &mut Context<Self>) {
        self.forwards.set_scope(scope);
        self.sync_forward_section();
        self.workspace.select(Destination::Forwards);
        self.workspace
            .set_context(shellune_core::PanelSection::Forwards);
        self.showing_connection_form = false;
        self.session_terminal = false;
        cx.notify();
    }

    /// Shows the global page after a pure-forward connection is up. Nothing new is bound here.
    pub(super) fn present_forwards(&mut self, cx: &mut Context<Self>) {
        self.show_forwards_scope(ForwardScope::All, cx);
    }

    pub(super) fn open_forward_editor(&mut self, cx: &mut Context<Self>) {
        let host = self.book.hosts().first().map(HostConfig::id);
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
        let book = match self
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
                ForwardPhase::Listening { session, .. } | ForwardPhase::Starting { session, .. } => {
                    Some(session)
                }
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
        self.workspace.sessions().items().iter().find_map(|session| {
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
            let dead = self
                .forward_carriers
                .get_mut(&id)
                .is_some_and(|link| {
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

    pub(super) fn forward_rows(&self, list: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let mut list = list;
        if let Some(error) = self.forwards.load_error() {
            list = list.child(
                crate::app::components::banner(Tone::Danger, metrics)
                    .debug_selector(|| "forwards.load".to_owned())
                    .child(SharedString::from(error.to_owned())),
            );
        }
        if self.forwards.rows().is_empty()
            && self.forwards.editor().is_none()
            && self.pending_exposure.is_none()
        {
            list = list.child(self.section_card(Destination::Forwards, crate::app::workspace::SectionState::Empty));
        }
        if let Some(id) = self.pending_exposure {
            list = list.child(self.exposure_question(id, cx));
        }
        if let Some(editor) = self.forwards.editor().cloned() {
            list = list.child(self.forward_form(&editor, cx));
        }
        for (index, row) in self.forwards.rows().into_iter().enumerate() {
            let id = row.rule.id();
            let host_name = self
                .book
                .host(row.rule.host())
                .map(HostConfig::name)
                .unwrap_or("?");
            let kind = kind_label(row.rule.kind(), strings);
            let listen = row.rule.listen().to_string();
            let target = row
                .rule
                .target()
                .map(ToString::to_string)
                .unwrap_or_else(|| "SOCKS".to_owned());
            let (status, tone, detail) = phase_text(&row.phase, row.attempt.as_ref(), strings);
            let active = row.phase.is_active();
            list = list.child(
                components_row(self, index, cx)
                    .child(div().flex().flex_col().flex_1().min_w(px(0.0)).child(
                        div()
                            .text_size(metrics.label_size())
                            .child(SharedString::from(format!(
                                "{kind} · {host_name} · {status}"
                            ))),
                    ).child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(SharedString::from(format!(
                                "{} {listen} · {} {target}",
                                strings.forward_listen, strings.forward_target
                            ))),
                    ).children(detail.map(|detail| {
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::DANGER))
                            .child(SharedString::from(detail))
                    })))
                    .child(
                        div()
                            .flex()
                            .gap(metrics.scale(spacing::HAIRLINE))
                            .child(self.text_button(
                                SharedString::from(format!("forwards.start.{index}")),
                                if active {
                                    strings.forward_stop
                                } else {
                                    strings.forward_start
                                },
                                if active {
                                    Variant::Danger
                                } else {
                                    Variant::Primary
                                },
                                cx,
                                move |view, cx| {
                                    if active {
                                        view.stop_forward(id, cx);
                                    } else {
                                        view.start_forward(id, cx);
                                    }
                                },
                            ))
                            .child(self.text_button(
                                SharedString::from(format!("forwards.edit.{index}")),
                                strings.forward_edit,
                                Variant::Ghost,
                                cx,
                                move |view, cx| view.edit_forward(id, cx),
                            ))
                            .child(self.text_button(
                                SharedString::from(format!("forwards.copy.{index}")),
                                strings.forward_copy,
                                Variant::Ghost,
                                cx,
                                move |view, cx| view.copy_forward(id, cx),
                            ))
                            .child(self.text_button(
                                SharedString::from(format!("forwards.delete.{index}")),
                                strings.forward_delete,
                                Variant::Ghost,
                                cx,
                                move |view, cx| view.delete_forward(id, cx),
                            )),
                    ),
            );
            let _ = tone;
        }
        list
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

    fn forward_form(&self, editor: &ForwardEditor, cx: &mut Context<Self>) -> gpui::Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let kind = kind_label(editor.kind, strings);
        let host = editor
            .host
            .and_then(|id| self.book.host(id))
            .map(HostConfig::name)
            .unwrap_or("—");
        let mut form = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "forwards.form".to_owned())
            .child(SharedString::from(format!(
                "{} · {kind} · {} {host}",
                strings.forward_new, strings.forward_host
            )))
            .child(self.forward_listen_host.clone())
            .child(self.forward_listen_port.clone())
            .child(self.forward_target_host.clone())
            .child(self.forward_target_port.clone());
        if let Some(error) = &editor.error {
            form = form.child(
                div()
                    .text_color(theme::swatch(palette::DANGER))
                    .child(SharedString::from(error.clone())),
            );
        }
        let hosts: Vec<HostId> = self.book.hosts().iter().map(HostConfig::id).collect();
        form.child(
            div()
                .flex()
                .gap(metrics.scale(spacing::HAIRLINE))
                .child(self.text_button(
                    SharedString::from("forwards.kind"),
                    kind,
                    Variant::Ghost,
                    cx,
                    |view, cx| {
                        if let Some(editor) = view.forwards.editor_mut() {
                            editor.kind = match editor.kind {
                                ForwardKind::Local => ForwardKind::Remote,
                                ForwardKind::Remote => ForwardKind::Dynamic,
                                ForwardKind::Dynamic => ForwardKind::Local,
                            };
                        }
                        cx.notify();
                    },
                ))
                .child(self.text_button(
                    SharedString::from("forwards.host"),
                    strings.forward_host,
                    Variant::Ghost,
                    cx,
                    move |view, cx| {
                        if let Some(editor) = view.forwards.editor_mut() {
                            let current = editor.host;
                            let next = hosts
                                .iter()
                                .cycle()
                                .skip_while(|id| Some(**id) != current)
                                .nth(1)
                                .copied();
                            editor.host = next.or(current);
                        }
                        cx.notify();
                    },
                ))
                .child(self.text_button(
                    SharedString::from("forwards.save"),
                    strings.forward_save,
                    Variant::Primary,
                    cx,
                    |view, cx| view.save_forward(cx),
                ))
                .child(self.text_button(
                    SharedString::from("forwards.form.close"),
                    strings.forward_exposure_cancel,
                    Variant::Ghost,
                    cx,
                    |view, cx| {
                        view.forwards.close_editor();
                        cx.notify();
                    },
                )),
        )
    }
}

fn components_row(
    view: &ShelluneView,
    index: usize,
    _cx: &mut Context<ShelluneView>,
) -> gpui::Div {
    crate::app::components::row(view.metrics())
        .debug_selector(move || format!("forwards.row.{index}"))
}

fn kind_label(kind: ForwardKind, strings: &Strings) -> &'static str {
    match kind {
        ForwardKind::Local => strings.forward_local,
        ForwardKind::Remote => strings.forward_remote,
        ForwardKind::Dynamic => strings.forward_dynamic,
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

fn failure_text(failure: &ForwardFailure, strings: &Strings) -> String {
    match failure {
        ForwardFailure::CarrierLost => strings.forward_carrier_lost.to_owned(),
        other => other.to_string(),
    }
}
