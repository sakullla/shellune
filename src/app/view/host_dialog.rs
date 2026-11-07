//! The dialog that creates or edits a saved host.
//!
//! A host is a few facts filled in together: where it is, who signs in, how they sign
//! in and what to run once connected. They used to sit in a card pushed into the hosts
//! list, which moved the list and mixed the form with the import panel. A dialog keeps
//! the form in one place over a dimmed window, with the sign-in choice deciding which
//! fields are asked for.

use gpui::prelude::*;
use gpui::{Context, Div, FontWeight, MouseButton, Point, SharedString, Stateful, Window, div, px};
use shellune_core::AuthMethodKind;

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

    /// The dialog over a dimmed window, while a host is being created or edited.
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
            .rounded(px(radius::LARGE + 2.0))
            .bg(theme::swatch(palette::SURFACE))
            .border_1()
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .shadow_lg()
            .overflow_hidden()
            .occlude()
            .child(header)
            .child(body)
            .child(footer);

        Some(
            div().child(
                gpui::deferred(
                    gpui::anchored().position(Point::default()).child(
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
                .with_priority(1),
            ),
        )
    }
}
