//! The dialog that creates or edits a saved host.
//!
//! A host is a few facts filled in together: where it is, who signs in, how they sign
//! in and what to run once connected. They used to sit in a card pushed into the hosts
//! list, which moved the list and mixed the form with the import panel. A dialog keeps
//! the form in one place over a dimmed window, with the sign-in choice deciding which
//! fields are asked for.

use gpui::prelude::*;
use gpui::{Context, Div, FontWeight, MouseButton, SharedString, Stateful, Window, div, px};
use shellune_core::{AuthMethodKind, HostId, TransferProtocol, X11Forwarding};

use super::{HostFields, ShelluneView};
use crate::app::components::{self, Variant};
use crate::app::hosts::{HostDraft, HostField};
use crate::app::icons::{self, Icon};
use crate::app::remote::MethodChoice;
use crate::app::theme::{self, Metrics, Tone, palette, radius, spacing};

/// A checkbox with its label: a box that fills with the accent when checked.
fn checkbox(
    id: &'static str,
    label: &'static str,
    checked: bool,
    metrics: Metrics,
) -> Stateful<Div> {
    let accent = theme::swatch(palette::ACCENT);
    let hover = theme::swatch(palette::TEXT_PRIMARY);
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex()
        .flex_none()
        .items_center()
        .gap(metrics.scale(spacing::TIGHT))
        .cursor_pointer()
        .text_size(metrics.label_size())
        .text_color(theme::swatch(palette::TEXT_SECONDARY))
        .hover(move |style| style.text_color(hover))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(metrics.scale(16.0))
                .rounded(px(radius::SMALL - 2.0))
                .border_1()
                .when(checked, |box_| {
                    box_.bg(accent).border_color(accent).child(icons::icon(
                        Icon::Check,
                        metrics.scale(12.0),
                        theme::swatch(palette::ON_ACCENT),
                    ))
                })
                .when(!checked, |box_| {
                    box_.bg(theme::swatch(palette::CANVAS))
                        .border_color(theme::swatch(palette::BORDER_STRONG))
                }),
        )
        .child(label)
}

impl ShelluneView {
    /// Chooses how the host in the dialog signs in.
    fn set_host_auth(&mut self, kind: AuthMethodKind, cx: &mut Context<Self>) {
        if let Some(draft) = self.hosts.draft_mut()
            && draft.set_auth(kind)
        {
            cx.notify();
        }
    }

    fn toggle_remember(&mut self, cx: &mut Context<Self>) {
        if let Some(draft) = self.hosts.draft_mut() {
            let remember = !draft.remember();
            draft.set_remember(remember);
            cx.notify();
        }
    }

    fn add_host_jump(&mut self, id: HostId, cx: &mut Context<Self>) {
        if let Some(draft) = self.hosts.draft_mut() {
            draft.add_jump(id);
        }
        cx.notify();
    }

    fn remove_host_jump(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(draft) = self.hosts.draft_mut() {
            draft.remove_jump(index);
        }
        cx.notify();
    }

    fn move_host_jump(&mut self, index: usize, up: bool, cx: &mut Context<Self>) {
        if let Some(draft) = self.hosts.draft_mut() {
            draft.move_jump(index, up);
        }
        cx.notify();
    }

    /// The ordered jump list and the ProxyCommand. Neither is executed from here.
    fn jump_section(&self, fields: &HostFields, draft: &HostDraft, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let mut column = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(|| "hosts.jump".to_owned())
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.host_jump_title),
            );
        if draft.jumps().is_empty() {
            column = column.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.host_jump_empty),
            );
        }
        // The chain in the order it is travelled, numbered, with its reordering as
        // small arrows: a first hop cannot go up, nor a last one down.
        let count = draft.jumps().len();
        for (index, id) in draft.jumps().iter().copied().enumerate() {
            let name = self
                .book
                .host(id)
                .map(|host| host.name().to_owned())
                .unwrap_or_else(|| id.to_string());
            column = column.child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .pl(metrics.scale(spacing::TIGHT))
                    .rounded(px(radius::MEDIUM))
                    .border_1()
                    .border_color(theme::swatch(palette::BORDER))
                    .bg(theme::swatch(palette::SURFACE))
                    .debug_selector({
                        let selector = format!("hosts.jump.{index}");
                        move || selector
                    })
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(format!("{}", index + 1))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .text_size(metrics.label_size())
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(SharedString::from(name)),
                    )
                    .when(index > 0, |row| {
                        row.child(self.row_icon_button(
                            SharedString::from(format!("hosts.jump.up.{index}")),
                            Icon::ChevronUp,
                            strings.host_jump_up,
                            false,
                            cx,
                            move |view, cx| view.move_host_jump(index, true, cx),
                        ))
                    })
                    .when(index + 1 < count, |row| {
                        row.child(self.row_icon_button(
                            SharedString::from(format!("hosts.jump.down.{index}")),
                            Icon::ChevronDown,
                            strings.host_jump_down,
                            false,
                            cx,
                            move |view, cx| view.move_host_jump(index, false, cx),
                        ))
                    })
                    .child(self.row_icon_button(
                        SharedString::from(format!("hosts.jump.remove.{index}")),
                        Icon::Close,
                        strings.host_jump_remove,
                        true,
                        cx,
                        move |view, cx| view.remove_host_jump(index, cx),
                    )),
            );
        }
        let editing = draft.editing();
        let present = draft.jumps().to_vec();
        let additions: Vec<(HostId, String)> = self
            .book
            .hosts()
            .iter()
            .filter(|host| editing != Some(host.id()) && !present.contains(&host.id()))
            .map(|host| (host.id(), host.name().to_owned()))
            .collect();
        // The saved hosts that can be added, as small buttons that wrap, so a long
        // host book does not push the rest of the form out of reach.
        if !additions.is_empty() {
            let mut choices = div()
                .flex()
                .flex_wrap()
                .gap(metrics.scale(spacing::HAIRLINE));
            for (index, (id, name)) in additions.into_iter().enumerate() {
                choices = choices.child(
                    self.icon_text_button(
                        SharedString::from(format!("hosts.jump.add.{index}")),
                        Icon::Plus,
                        name,
                        Variant::Secondary,
                        cx,
                        move |view, cx| view.add_host_jump(id, cx),
                    )
                    .max_w(metrics.scale(220.0))
                    .tooltip(components::tooltip(strings.host_jump_add)),
                );
            }
            column = column.child(choices);
        }
        column = column
            .child(self.dialog_field(fields, HostField::ProxyCommand, None, 1.0))
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.host_proxy_note),
            );
        if draft.proxy_conflicts() {
            column = column.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(Tone::Warning.text())
                    .debug_selector(|| "hosts.proxy.conflict".to_owned())
                    .child(strings.host_proxy_conflict),
            );
        }
        column
    }

    /// Saves the host and, when it was saved, connects to it at once.
    fn save_and_connect(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.save_host(cx) else {
            return;
        };
        if let Some(host) = self.book.host(id).cloned() {
            self.open_remote(&host, cx);
        }
    }

    /// One labelled field of the dialog.
    fn dialog_field(
        &self,
        fields: &HostFields,
        field: HostField,
        label: Option<&'static str>,
        grow: f32,
    ) -> Div {
        let metrics = self.metrics();
        let label = label.unwrap_or_else(|| field.label(self.strings()));
        div()
            .flex()
            .flex_col()
            .flex_grow()
            .flex_basis(gpui::relative(grow))
            .min_w(px(0.0))
            .gap(metrics.scale(spacing::HAIRLINE + 2.0))
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(label),
            )
            .child(
                div()
                    .flex()
                    .w_full()
                    .min_w(px(0.0))
                    .debug_selector(move || field.debug_selector().to_owned())
                    .child(fields.get(field).clone()),
            )
    }

    /// The fields the chosen sign-in method asks for.
    fn sign_in_fields(
        &self,
        fields: &HostFields,
        draft: &HostDraft,
        cx: &mut Context<Self>,
    ) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let auth = draft.auth();
        let mut column = div().flex().flex_col().gap(metrics.scale(spacing::SNUG));

        let mut methods = components::segmented(metrics).debug_selector(|| "hosts.auth".to_owned());
        for choice in MethodChoice::ALL {
            let kind = choice.kind();
            methods = methods.child(
                components::segment(choice.label(strings), kind == auth, metrics)
                    .id(choice.debug_selector())
                    .debug_selector(move || format!("hosts.auth.{}", choice.debug_selector()))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.set_host_auth(kind, cx);
                    })),
            );
        }
        column = column.child(
            div()
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::HAIRLINE + 2.0))
                .child(
                    div()
                        .text_size(metrics.caption_size())
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .child(strings.host_auth_title),
                )
                .child(div().flex().child(methods)),
        );

        if auth.needs_key_path() {
            column = column
                .child(self.dialog_field(fields, HostField::IdentityFile, None, 1.0))
                .child(
                    div()
                        .mt(metrics.scale(-(spacing::HAIRLINE)))
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .child(strings.host_identity_note),
                );
        }
        if draft.needs_secret() {
            let (label, remember_label) = if auth.needs_passphrase() {
                (
                    strings.host_secret_passphrase,
                    strings.host_remember_passphrase,
                )
            } else {
                (strings.auth_password, strings.host_remember_password)
            };
            column = column
                .child(self.dialog_field(fields, HostField::Secret, Some(label), 1.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(metrics.scale(spacing::HAIRLINE))
                        .child(
                            checkbox("hosts.remember", remember_label, draft.remember(), metrics)
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    this.toggle_remember(cx);
                                })),
                        )
                        .child(
                            div()
                                .pl(metrics.scale(24.0))
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED))
                                .child(strings.host_remember_note),
                        ),
                );
        }
        if auth == AuthMethodKind::Agent {
            column = column.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.host_agent_note),
            );
        }
        column
    }

    /// The choice of the protocol this host's transfers start with (R10).
    ///
    /// SFTP stays the default; SCP is the explicit alternative for a server without an
    /// SFTP subsystem. The switch changes this host's record only - a transfer started
    /// from it may still choose the other protocol for that one task.
    fn copy_protocol_section(
        &self,
        draft: &HostDraft,
        metrics: Metrics,
        strings: &'static crate::app::copy::Strings,
        cx: &mut Context<Self>,
    ) -> Div {
        let chosen = draft.protocol();
        let mut row =
            components::segmented(metrics).debug_selector(|| "hosts.editor.protocol".to_owned());
        for (protocol, label, selector) in [
            (
                TransferProtocol::Sftp,
                strings.transfers_protocol_sftp,
                "hosts.editor.protocol.sftp",
            ),
            (
                TransferProtocol::Scp,
                strings.transfers_protocol_scp,
                "hosts.editor.protocol.scp",
            ),
        ] {
            row = row.child(
                components::segment(label, protocol == chosen, metrics)
                    .id(selector)
                    .debug_selector(move || selector.to_owned())
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        if let Some(draft) = view.hosts.draft_mut() {
                            draft.set_protocol(protocol);
                        }
                        cx.notify();
                    })),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE + 2.0))
            .child(
                div()
                    .pt(metrics.scale(spacing::HAIRLINE))
                    .text_size(metrics.caption_size())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.transfers_protocol),
            )
            .child(row)
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.transfers_protocol_note),
            )
    }

    /// The host's default X11 forwarding mode (R11).
    ///
    /// Off stays the default: asking a server for a display channel is a decision with
    /// consequences for this desktop, so it is made here, saved with the host, and can
    /// still be changed for one connection in the connection form. The trusted mode is
    /// not treated as an ordinary preference: its risk sentence is drawn beside it, and
    /// the connection form asks again before every attempt that would use it.
    fn x11_section(
        &self,
        draft: &HostDraft,
        metrics: Metrics,
        strings: &'static crate::app::copy::Strings,
        cx: &mut Context<Self>,
    ) -> Div {
        let chosen = draft.x11();
        let mut row =
            components::segmented(metrics).debug_selector(|| "hosts.editor.x11".to_owned());
        for (mode, label, selector) in [
            (
                X11Forwarding::Off,
                strings.x11_off_label,
                "hosts.editor.x11.off",
            ),
            (
                X11Forwarding::Restricted,
                strings.x11_restricted_label,
                "hosts.editor.x11.restricted",
            ),
            (
                X11Forwarding::Trusted,
                strings.x11_trusted_label,
                "hosts.editor.x11.trusted",
            ),
        ] {
            row = row.child(
                components::segment(label, mode == chosen, metrics)
                    .id(selector)
                    .debug_selector(move || selector.to_owned())
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        if let Some(draft) = view.hosts.draft_mut() {
                            draft.set_x11(mode);
                        }
                        cx.notify();
                    })),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE + 2.0))
            .child(
                div()
                    .pt(metrics.scale(spacing::HAIRLINE))
                    .text_size(metrics.caption_size())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.x11_label),
            )
            .child(row)
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.x11_note),
            )
            .when(chosen.needs_risk_confirmation(), |section| {
                section.child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(Tone::Warning.text())
                        .debug_selector(|| "hosts.editor.x11.warning".to_owned())
                        .child(strings.x11_trusted_body),
                )
            })
    }

    /// The dialog over a dimmed window, while a host is being created or edited.
    ///
    /// # The copy protocol
    ///
    /// The protocol a host's transfers start with is part of the host, not of a single
    /// transfer: a server that offers no SFTP subsystem is asked for SCP once and keeps
    /// it. It is a preference with a consequence, so the note beside the switch says what
    /// choosing SCP changes - no directory listing and no resume.
    pub(super) fn host_dialog_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let fields = self.host_fields.as_ref()?;
        let draft = self.hosts.draft()?.clone();
        let metrics = self.metrics();
        let strings = self.strings();
        let viewport = window.viewport_size();
        let editing = draft.editing().is_some();
        let width = px(600.0).min(viewport.width - metrics.scale(spacing::LOOSE) * 2.0);
        let max_height = viewport.height - metrics.scale(spacing::LOOSE) * 2.0;

        let line = |left: Div, right: Div| {
            div()
                .flex()
                .w_full()
                .gap(metrics.scale(spacing::SNUG))
                .child(left)
                .child(right)
        };
        let section = |title: &'static str| {
            div()
                .pt(metrics.scale(spacing::HAIRLINE))
                .text_size(metrics.caption_size())
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .child(title)
        };

        let body = div()
            .id("hosts.editor.body")
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG + 2.0))
            .px(metrics.scale(spacing::LOOSE))
            .py(metrics.scale(spacing::BASE))
            .overflow_y_scroll()
            .child(line(
                self.dialog_field(fields, HostField::Name, None, 0.5),
                self.dialog_field(fields, HostField::Group, None, 0.5),
            ))
            .child(line(
                self.dialog_field(fields, HostField::Address, None, 0.75),
                self.dialog_field(fields, HostField::Port, None, 0.25),
            ))
            .child(self.dialog_field(fields, HostField::User, None, 1.0))
            .child(div().h(px(1.0)).bg(theme::swatch(palette::BORDER)))
            .child(self.sign_in_fields(fields, &draft, cx))
            .child(div().h(px(1.0)).bg(theme::swatch(palette::BORDER)))
            .child(self.jump_section(fields, &draft, cx))
            .child(div().h(px(1.0)).bg(theme::swatch(palette::BORDER)))
            .child(self.copy_protocol_section(&draft, metrics, strings, cx))
            .child(div().h(px(1.0)).bg(theme::swatch(palette::BORDER)))
            .child(self.x11_section(&draft, metrics, strings, cx))
            .child(div().h(px(1.0)).bg(theme::swatch(palette::BORDER)))
            .child(section(strings.host_startup_label))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(spacing::HAIRLINE + 2.0))
                    .child(
                        div()
                            .flex()
                            .w_full()
                            .debug_selector(|| {
                                HostField::StartupCommand.debug_selector().to_owned()
                            })
                            .child(fields.startup.clone()),
                    )
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(strings.host_startup_note),
                    ),
            );

        let refusal = self
            .hosts
            .message()
            .filter(|message| message.failed())
            .map(|message| {
                components::banner(Tone::Danger, metrics)
                    .debug_selector(|| "hosts.message".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(message.text().to_owned()))
            });

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .px(metrics.scale(spacing::LOOSE))
            .pt(metrics.scale(spacing::BASE + 2.0))
            .child(
                div()
                    .text_size(metrics.heading_size())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(if editing {
                        strings.host_editor_edit
                    } else {
                        strings.host_editor_new
                    }),
            )
            .child(
                components::icon_only_button(Icon::Close, metrics)
                    .id("hosts.editor.close")
                    .debug_selector(|| "hosts.editor.close".to_owned())
                    .tooltip(components::tooltip(strings.host_dialog_close))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.close_host_editor(cx);
                    })),
            );

        let footer = div()
            .flex()
            .flex_none()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .py(metrics.scale(spacing::BASE))
            .border_t_1()
            .border_color(theme::swatch(palette::BORDER))
            .children(refusal)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        self.text_button(
                            SharedString::from("hosts.cancel"),
                            strings.host_cancel,
                            Variant::Ghost,
                            cx,
                            |view, cx| view.close_host_editor(cx),
                        )
                        .h(metrics.control_height()),
                    )
                    .child(
                        self.text_button(
                            SharedString::from("hosts.save"),
                            strings.host_save,
                            Variant::Secondary,
                            cx,
                            |view, cx| {
                                let _ = view.save_host(cx);
                            },
                        )
                        .h(metrics.control_height()),
                    )
                    .child(
                        self.icon_text_button(
                            SharedString::from("hosts.save_connect"),
                            Icon::Plug,
                            strings.host_save_connect,
                            Variant::Primary,
                            cx,
                            |view, cx| view.save_and_connect(cx),
                        )
                        .h(metrics.control_height()),
                    ),
            );

        let dialog = div()
            .id("hosts.editor")
            .debug_selector(|| "hosts.editor".to_owned())
            .w(width)
            .max_h(max_height)
            .flex()
            .flex_col()
            .rounded(px(radius::XLARGE))
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .border_1()
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .shadow_2xl()
            .overflow_hidden()
            .occlude()
            .child(header)
            .child(body)
            .child(footer);

        Some(
            super::overlay_layer().child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(viewport.width)
                    .h(viewport.height)
                    .child(
                        div()
                            .id("hosts.editor.backdrop")
                            .absolute()
                            .inset_0()
                            .bg(gpui::rgba(0x0000_0080))
                            .occlude()
                            // A press on the dimmed window does not discard what
                            // was typed; the dialog closes with its buttons or Esc.
                            .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                                cx.stop_propagation();
                            }),
                    )
                    .child(dialog),
            ),
        )
    }
}
