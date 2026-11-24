//! The dialog that creates or edits a saved host.
//!
//! A host is a few facts filled in together: where it is, who signs in, how they sign
//! in and what to run once connected. They used to sit in a card pushed into the hosts
//! list, which moved the list and mixed the form with the import panel. A dialog keeps
//! the form in one place over a dimmed window, with the sign-in choice deciding which
//! fields are asked for.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    App, Context, Div, Entity, FontWeight, KeyDownEvent, ScrollHandle, SharedString, Stateful,
    Window, canvas, div, px,
};
use shellune_core::{
    AuthMethodKind, GroupPath, HostId, ProtocolKind, RdpCertificatePolicy, SerialFlow,
    SerialParity, SerialStopBits, TransferProtocol, VncSecurity, X11Forwarding,
};

use super::shell::{ModalBehaviour, ModalContent};
use super::{HostFields, ShelluneView};
use crate::app::components::{self, Variant};
use crate::app::copy;
use crate::app::hosts::{
    ConnectionChoice, ConnectionField, ConnectionFormField, HostDraft, HostField,
    connection_problem_text,
};
use crate::app::icons::{self, Icon};
use crate::app::remote::MethodChoice;
use crate::app::text_field::TextField;
use crate::app::theme::{self, Metrics, Tone, palette, radius, spacing};

// A checkbox with its label is the shared component.
use crate::app::components::checkbox;

/// The modal both forms of the host editor are drawn in.
///
/// One dialog asks for a host, whichever protocol that host speaks: changing the
/// protocol changes what the dialog asks, not which dialog is up. The id is therefore
/// shared. GPUI keys the entrance animation by it, and a per-protocol id replayed the
/// entrance on every switch - the dialog went transparent and slid eight pixels, which
/// is seen as the whole window flashing (R7).
const HOST_EDITOR_ID: &str = "hosts.editor";

/// How many candidates the jump picker shows before its list scrolls.
const JUMP_PICKER_ROWS: f32 = 6.0;

/// How many group candidates the searchable group selector shows before scrolling (R12).
const GROUP_PICKER_ROWS: f32 = 5.0;

/// The widest a host's name is drawn in the jump picker before its address.
const JUMP_NAME_MAX_WIDTH: f32 = 200.0;

/// The least width of one override number of the host form.
const CONNECTION_FIELD_MIN_WIDTH: f32 = 120.0;

/// One row of the group dropdown.
enum GroupChoice {
    /// A group that is saved or that a saved record names.
    Saved(GroupPath),
    /// The typed path, which saving creates.
    New(String),
}

/// The jump host picker of the host dialog, while it is open (R3).
///
/// It is a search field over the saved SSH hosts that can still be added, with one
/// candidate marked for the keyboard. Each pick adds a hop and leaves the picker open
/// with an empty search, so a chain of several hops is built without reopening it.
pub(super) struct JumpPicker {
    search: Entity<TextField>,
    /// The search the candidates were last narrowed by: a new phrase moves the mark
    /// back to the first candidate, a caret move does not.
    phrase: String,
    /// The candidate Enter adds, as an index into the narrowed list.
    highlight: usize,
    scroll: ScrollHandle,
    /// Whether the dialog body still has to scroll the just-opened picker into view.
    reveal: Rc<Cell<bool>>,
}

impl ShelluneView {
    /// Opens the jump picker with an empty search that has the keyboard, or closes it
    /// when it is already open.
    pub(super) fn toggle_jump_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.jump_picker.is_some() {
            self.close_jump_picker(window, cx);
            return;
        }
        let placeholder = copy::ux_for(self.workspace.locale()).host_jump_search;
        let search = cx.new(|cx| {
            let mut field = TextField::new(placeholder, cx);
            field.set_leading_icon(Icon::Search, cx);
            field
        });
        // A new phrase marks the first candidate again; the list itself is read from
        // the field on every draw.
        cx.observe(&search, |this, search, cx| {
            let text = search.read(cx).text().to_owned();
            if let Some(picker) = this.jump_picker.as_mut()
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
        self.jump_picker = Some(JumpPicker {
            search,
            phrase: String::new(),
            highlight: 0,
            scroll: ScrollHandle::new(),
            reveal: Rc::new(Cell::new(true)),
        });
        cx.notify();
    }

    /// Puts the open picker's placeholder in the current language.
    pub(super) fn relabel_jump_picker(&self, cx: &mut Context<Self>) {
        if let Some(picker) = self.jump_picker.as_ref() {
            let placeholder = copy::ux_for(self.workspace.locale()).host_jump_search;
            picker
                .search
                .update(cx, |field, cx| field.set_placeholder(placeholder, cx));
        }
    }

    /// Closes the jump picker and gives the keyboard back to the dialog's route field.
    pub(super) fn close_jump_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.jump_picker.take().is_some() {
            if let Some(fields) = self.host_fields.as_ref() {
                let handle = fields.proxy.read(cx).focus_handle().clone();
                window.focus(&handle);
            } else {
                window.focus(&self.root_focus);
            }
            cx.notify();
        }
    }

    /// The hosts the open picker offers for what is typed in it, in saved order.
    fn jump_picker_candidates(&self, cx: &App) -> Vec<HostId> {
        let (Some(picker), Some(draft)) = (self.jump_picker.as_ref(), self.hosts.draft()) else {
            return Vec::new();
        };
        draft.jump_candidates(self.book.hosts(), picker.search.read(cx).text())
    }

    /// Adds a picked host to the end of the chain and readies the picker for the next
    /// hop: the search is emptied, the mark is on the first candidate and the field
    /// keeps the keyboard.
    fn pick_jump(&mut self, id: HostId, window: &mut Window, cx: &mut Context<Self>) {
        self.add_host_jump(id, cx);
        if let Some(picker) = self.jump_picker.as_mut() {
            picker.phrase.clear();
            picker.highlight = 0;
            picker.scroll.scroll_to_item(0);
            let search = picker.search.clone();
            search.update(cx, |field, cx| field.set_text("", cx));
            window.focus(search.read(cx).focus_handle());
        }
        cx.notify();
    }

    /// Up and Down move the mark through the candidates and Enter adds the marked one.
    ///
    /// Escape is not here: the window's own Escape closes what floats on top first,
    /// and the picker is above the dialog.
    fn jump_picker_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.modifiers.number_of_modifiers() != 0 {
            return;
        }
        let candidates = self.jump_picker_candidates(cx);
        let Some(picker) = self.jump_picker.as_mut() else {
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
                    self.pick_jump(id, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The search, the marked candidate and how many candidates the open jump picker
    /// shows, for the tests.
    #[cfg(test)]
    pub(crate) fn jump_picker_for_test(&self, cx: &App) -> Option<(String, usize, usize)> {
        let picker = self.jump_picker.as_ref()?;
        Some((
            picker.search.read(cx).text().to_owned(),
            picker.highlight,
            self.jump_picker_candidates(cx).len(),
        ))
    }

    /// Chooses how the host in the dialog signs in.
    fn set_host_auth(&mut self, kind: AuthMethodKind, cx: &mut Context<Self>) {
        if let Some(draft) = self.hosts.draft_mut()
            && draft.set_auth(kind)
        {
            self.clear_resolved_host_validation(cx);
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
            .gap(metrics.scale(spacing::NARROW))
            .debug_selector(|| "hosts.jump".to_owned())
            .child(components::form_label(strings.host_jump_title, metrics));
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
                    .bg(theme::swatch(palette::CHROME))
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
                            .min_w_0()
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
        column = column.child(self.jump_adder(cx));
        column = column
            .child(
                self.dialog_field(fields, HostField::ProxyCommand, None, 1.0)
                    .mt(metrics.scale(spacing::TIGHT)),
            )
            .child(components::form_hint(strings.host_proxy_note, metrics));
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

    /// "Add jump host" and, while it is open, the picker under it (R3).
    ///
    /// The picker sits in the form's flow rather than floating over it: the form is a
    /// scrolling dialog body, where a floating list would be cut at the body's edge at
    /// 720×480 and 200% text, while a list in the flow scrolls into view with the rest of
    /// the form. Its own list is capped at a few rows and scrolls, so a book of a hundred
    /// hosts makes it no taller than a book of six. A press anywhere outside the button
    /// and the picker closes it.
    fn jump_adder(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let open = self.jump_picker.is_some();
        let button = components::icon_label_button(
            Icon::Plus,
            self.strings().host_jump_add,
            Variant::Secondary,
            metrics,
        )
        .h(metrics.compact_control_height())
        .pl(metrics.scale(spacing::TIGHT))
        .pr(metrics.scale(spacing::TIGHT + 2.0))
        .text_size(metrics.caption_size())
        .when(open, |button| {
            button.bg(theme::swatch(palette::SURFACE_SELECTED))
        })
        .id("hosts.jump.add")
        .debug_selector(|| "hosts.jump.add".to_owned())
        .on_click(cx.listener(|this, _event, window, cx| {
            this.toggle_jump_picker(window, cx);
        }));
        let mut adder = div()
            .flex()
            .flex_col()
            .items_start()
            .gap(metrics.scale(spacing::HAIRLINE))
            .w_full()
            .min_w_0()
            .child(button);
        if open {
            adder = adder
                .on_mouse_down_out(cx.listener(|this, _event, window, cx| {
                    this.close_jump_picker(window, cx);
                }))
                .children(self.jump_picker_card(cx));
        }
        adder
    }

    /// The open jump picker: its search field over the candidates it narrows.
    fn jump_picker_card(&self, cx: &mut Context<Self>) -> Option<Div> {
        let picker = self.jump_picker.as_ref()?;
        let metrics = self.metrics();
        let ux = copy::ux();
        let candidates = self.jump_picker_candidates(cx);
        let marked = picker.highlight.min(candidates.len().saturating_sub(1));
        // The candidates are menu entries: one compact control high, like every menu.
        let row_height = metrics.compact_control_height();
        let narrowed = !picker.search.read(cx).text().trim().is_empty();
        let mut list = div()
            .id("hosts.jump.options")
            .debug_selector(|| "hosts.jump.options".to_owned())
            .flex()
            .flex_col()
            .gap(px(components::HAIRLINE_WIDTH))
            // Exactly the rows and the hairline gaps between them: a book of six fills
            // the list to the cap and a longer one scrolls at that same height.
            .max_h(
                row_height * JUMP_PICKER_ROWS
                    + px(components::HAIRLINE_WIDTH) * (JUMP_PICKER_ROWS - 1.0),
            )
            .overflow_y_scroll()
            .track_scroll(&picker.scroll);
        for (index, id) in candidates.iter().copied().enumerate() {
            let Some(host) = self.book.host(id) else {
                continue;
            };
            let selected = index == marked;
            let hover = theme::swatch(palette::SURFACE_HOVER);
            list = list.child(
                div()
                    .id(SharedString::from(format!("hosts.jump.option.{index}")))
                    .debug_selector(move || format!("hosts.jump.option.{index}"))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .h(row_height)
                    .px(metrics.scale(spacing::TIGHT))
                    .rounded(px(radius::SMALL))
                    .cursor_pointer()
                    .when(selected, |row| {
                        row.bg(theme::swatch(palette::SURFACE_SELECTED))
                    })
                    .when(!selected, |row| row.hover(move |style| style.bg(hover)))
                    .child(icons::icon(
                        Icon::Server,
                        metrics.small_icon_size(),
                        theme::swatch(if selected {
                            palette::ACCENT
                        } else {
                            palette::TEXT_MUTED
                        }),
                    ))
                    .child(
                        div()
                            .flex_none()
                            .max_w(metrics.scale(JUMP_NAME_MAX_WIDTH))
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
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        this.pick_jump(id, window, cx);
                    })),
            );
        }
        let none = candidates.is_empty().then(|| {
            div()
                .debug_selector(|| "hosts.jump.none".to_owned())
                .px(metrics.scale(spacing::TIGHT + 2.0))
                .py(metrics.scale(spacing::TIGHT))
                .whitespace_normal()
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .child(if narrowed {
                    ux.host_jump_no_match
                } else {
                    ux.host_jump_none_left
                })
        });
        // The first time the picker is laid out, the dialog body scrolls just enough to
        // show all of it, or its top when it is taller than the body: at 720×480 and 200%
        // text the button is near the body's bottom edge, and a list that opened below
        // the fold would look like nothing happened.
        let reveal = picker.reveal.clone();
        let body = self.host_body_scroll.clone();
        let shell = cx.entity_id();
        // The card's edge and a little room stay in view as well.
        let margin = metrics.scale(spacing::HAIRLINE);
        let reveal = canvas(
            move |bounds, _window, cx| {
                if !reveal.replace(false) {
                    return;
                }
                let view = body.bounds();
                let below = bounds.bottom() + margin - view.bottom();
                let shift = below.min(bounds.top() - margin - view.top());
                if shift > px(0.0) {
                    let mut offset = body.offset();
                    offset.y -= shift;
                    body.set_offset(offset);
                    // The new offset is drawn by the next frame.
                    cx.defer(move |cx| cx.notify(shell));
                }
            },
            |_bounds, (), _window, _cx| {},
        )
        .absolute()
        .inset_0();
        Some(
            components::menu_card("hosts.jump.picker", metrics)
                .relative()
                .w_full()
                .min_w_0()
                .child(reveal)
                .gap(metrics.scale(spacing::HAIRLINE))
                // The keys the picker answers are taken before the field sees them:
                // the field would read Enter as a submit of its own.
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    this.jump_picker_key(event, window, cx);
                }))
                .child(
                    div()
                        .flex()
                        .w_full()
                        .min_w_0()
                        .debug_selector(|| "hosts.jump.search".to_owned())
                        .child(picker.search.clone()),
                )
                .children(none)
                .when(!candidates.is_empty(), |card| {
                    card.child(list).child(
                        div()
                            .px(metrics.scale(spacing::TIGHT))
                            .py(metrics.scale(spacing::HAIRLINE))
                            .border_t_1()
                            .border_color(theme::swatch(palette::BORDER))
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(ux.host_jump_keys),
                    )
                }),
        )
    }

    /// Saves the host and, when it was saved, connects to it at once.
    fn save_and_connect(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.save_host_for_connect(cx) else {
            return;
        };
        if let Some(host) = self.book.host(id).cloned() {
            self.open_remote(&host, cx);
        }
    }

    /// Saves the non-SSH connection and, when it was saved, connects to it at once.
    fn save_and_connect_connection(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.save_connection(cx) else {
            return;
        };
        self.connect_saved_connection(id, cx);
    }

    /// One labelled field of the dialog.
    fn dialog_field(
        &self,
        fields: &HostFields,
        field: HostField,
        label: Option<&'static str>,
        grow: f32,
    ) -> Div {
        self.dialog_field_with(fields, field, label, grow, None)
    }

    /// A field of the host form with something that floats under its input - the
    /// group field's dropdown.
    fn dialog_field_with(
        &self,
        fields: &HostFields,
        field: HostField,
        label: Option<&'static str>,
        grow: f32,
        attached: Option<gpui::AnyElement>,
    ) -> Div {
        let metrics = self.metrics();
        let label = label.unwrap_or_else(|| field.label(self.strings()));
        // The port, a short field beside wide ones, keeps a short floor; the others
        // keep the form's floor, so a narrow dialog stacks them instead of squeezing.
        let floor = if matches!(field, HostField::Port) {
            components::FORM_SHORT_FIELD_MIN_WIDTH
        } else {
            components::FORM_FIELD_MIN_WIDTH
        };
        let mut cell = components::form_cell(grow, floor, metrics)
            .child(
                components::form_label(label, metrics)
                    .debug_selector(move || format!("{}.label", field.debug_selector())),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .w_full()
                    .min_w_0()
                    .debug_selector(move || field.debug_selector().to_owned())
                    .child(fields.get(field).clone())
                    .children(attached),
            );
        let problem = self
            .hosts
            .message()
            .filter(|message| message.failed())
            .and_then(|_| self.hosts.draft())
            .and_then(|draft| {
                let saved = draft.editing().and_then(|id| self.book.host(id));
                draft.first_problem(saved)
            })
            .and_then(|(which, reason)| match which {
                Ok(problem_field) if problem_field == field => Some(reason),
                _ => None,
            });
        if let Some(reason) = problem {
            cell = cell.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::DANGER))
                    .debug_selector(move || format!("{}.problem", field.debug_selector()))
                    .child(SharedString::from(reason)),
            );
        }
        cell
    }

    /// The fields the chosen sign-in method asks for.
    ///
    /// Four choices, not five: a key with a passphrase is a key file whose box says it
    /// has one, which is how people think of it. "Automatic" is first and the default;
    /// its password is optional, the last thing tried after what the computer has.
    fn sign_in_fields(
        &self,
        fields: &HostFields,
        draft: &HostDraft,
        cx: &mut Context<Self>,
    ) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let auth = draft.auth();
        let mut column = div().flex().flex_col().gap(metrics.scale(spacing::SNUG));

        let mut methods = components::segmented(metrics).debug_selector(|| "hosts.auth".to_owned());
        for choice in MethodChoice::ALL
            .into_iter()
            .filter(|choice| *choice != MethodChoice::EncryptedKeyFile)
        {
            let kind = choice.kind();
            let chosen = kind == auth
                || (choice == MethodChoice::KeyFile && auth == AuthMethodKind::EncryptedKeyFile);
            methods = methods.child(
                components::segment(choice.label(strings), chosen, metrics)
                    .id(choice.debug_selector())
                    .debug_selector(move || format!("hosts.auth.{}", choice.debug_selector()))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        // Choosing "key file" again keeps a passphrase that was ticked.
                        let already_key = this
                            .hosts
                            .draft()
                            .is_some_and(|draft| draft.auth().needs_key_path());
                        if !(kind == AuthMethodKind::KeyFile && already_key) {
                            this.set_host_auth(kind, cx);
                            // The vault's method starts on a key the host can
                            // really sign in with, not on a nameless choice that
                            // could never dial (R1, ADR-7).
                            if kind == AuthMethodKind::ManagedKey
                                && let Some(draft) = this.hosts.draft_mut()
                                && draft.managed_key_id().is_none()
                                && let Some(first) = this.security.content().and_then(|vault| {
                                    vault
                                        .keys()
                                        .find(|record| record.meta().is_live())
                                        .map(|record| record.meta().id())
                                })
                            {
                                draft.set_managed_key(Some(first));
                            }
                        }
                    })),
            );
        }
        column = column.child(
            div()
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::NARROW))
                .child(div().flex().child(methods))
                .child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .debug_selector(|| "hosts.auth.hint".to_owned())
                        .child(MethodChoice::from_kind(auth).hint(strings)),
                ),
        );

        // The vault's method names the key the host signs in with: a picker of
        // the keys the vault holds, each with the fingerprint a person checks
        // (R1, ADR-7). An empty vault says so, instead of a chooser with
        // nothing in it.
        if auth == AuthMethodKind::ManagedKey {
            let security = copy::security_for(self.workspace.locale());
            let chosen = draft.managed_key_id();
            let mut picker = div()
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::HAIRLINE))
                .debug_selector(|| "hosts.auth.managed.picker".to_owned())
                .child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .child(security.host_key_pick),
                );
            if self.vault_locked() {
                let locked_note = if chosen.is_some() {
                    security.host_vault_locked_bound_key
                } else {
                    security.host_vault_locked_key
                };
                picker = picker.child(
                    components::banner(Tone::Warning, metrics)
                        .debug_selector(|| "hosts.auth.vault.locked".to_owned())
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap(metrics.scale(spacing::TIGHT))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_normal()
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                .child(locked_note),
                        )
                        .child(
                            components::compact_button(
                                if self.security_verifying() {
                                    security.lock_state_verifying
                                } else {
                                    security.lock_unlock
                                },
                                components::Variant::Primary,
                                metrics,
                            )
                            .id("hosts.auth.vault.unlock")
                            .debug_selector(|| "hosts.auth.vault.unlock".to_owned())
                            .on_click(cx.listener(
                                |this, _event, _window, cx| {
                                    this.security_begin(Vec::new(), cx);
                                },
                            )),
                        ),
                );
            } else {
                if chosen.is_some_and(|id| self.security_vault_key_missing(id)) {
                    picker = picker.child(
                        components::banner(Tone::Warning, metrics)
                            .debug_selector(|| "hosts.auth.managed.missing".to_owned())
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(security.vault_key_missing_tip),
                    );
                }
                let mut keys = self
                    .security
                    .content()
                    .map(|vault| {
                        vault
                            .keys()
                            .filter(|record| record.meta().is_live())
                            .map(|record| record.meta().clone())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                keys.sort_by(|a, b| a.name().cmp(b.name()));
                if keys.is_empty() {
                    picker = picker.child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .debug_selector(|| "hosts.auth.managed.none".to_owned())
                            .child(security.method_none),
                    );
                }
                for (index, meta) in keys.iter().enumerate() {
                    let id = meta.id();
                    let selected = chosen == Some(id);
                    let label = format!("{} · {}", meta.name(), meta.fingerprint());
                    let selector = format!("hosts.auth.managed.key.{index}");
                    let reserved = components::compact_label_reserved_width(&label, metrics);
                    picker = picker.child(
                        components::compact_button(
                            SharedString::from(label.clone()),
                            if selected {
                                components::Variant::Primary
                            } else {
                                components::Variant::Secondary
                            },
                            metrics,
                        )
                        .min_w(reserved)
                        .max_w_full()
                        .flex_shrink()
                        .whitespace_normal()
                        .h(gpui::auto())
                        .min_h(metrics.compact_control_height())
                        .id(SharedString::from(selector.clone()))
                        .debug_selector({
                            let selector = selector.clone();
                            move || selector.clone()
                        })
                        .on_click(cx.listener(
                            move |this, _event, _window, cx| {
                                if let Some(draft) = this.hosts.draft_mut() {
                                    draft.set_managed_key(Some(id));
                                }
                                cx.notify();
                            },
                        )),
                    );
                }
                let inline_actions = div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        components::compact_button(
                            security.generate_key,
                            components::Variant::Secondary,
                            metrics,
                        )
                        .id("hosts.auth.managed.generate")
                        .debug_selector(|| "hosts.auth.managed.generate".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.security_open_inline(
                                    super::security::SecurityAction::Generate,
                                    super::security::PendingVaultBind::SshManagedKey,
                                    cx,
                                );
                            },
                        )),
                    )
                    .child(
                        components::compact_button(
                            security.import_key,
                            components::Variant::Secondary,
                            metrics,
                        )
                        .id("hosts.auth.managed.import")
                        .debug_selector(|| "hosts.auth.managed.import".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.security_open_inline(
                                    super::security::SecurityAction::Import,
                                    super::security::PendingVaultBind::SshManagedKey,
                                    cx,
                                );
                            },
                        )),
                    );
                picker = picker.child(inline_actions);
            }
            column = column.child(picker);
        }

        // A password sign-in may name a shared credential of the vault instead
        // of a password of its own: the record's password is the one every host
        // that names it uses, and it is edited in one place (R1).
        if auth == AuthMethodKind::Password {
            let security = copy::security_for(self.workspace.locale());
            let chosen = draft.credential_id();
            let mut picker = div()
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::HAIRLINE))
                .debug_selector(|| "hosts.auth.credential.picker".to_owned());
            if self.vault_locked() {
                let locked_note = if chosen.is_some() {
                    security.host_vault_locked_bound_credential
                } else {
                    security.host_vault_locked_credential
                };
                picker = picker.child(
                    components::banner(Tone::Warning, metrics)
                        .debug_selector(|| "hosts.auth.vault.locked".to_owned())
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap(metrics.scale(spacing::TIGHT))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_normal()
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                .child(locked_note),
                        )
                        .child(
                            components::compact_button(
                                if self.security_verifying() {
                                    security.lock_state_verifying
                                } else {
                                    security.lock_unlock
                                },
                                components::Variant::Primary,
                                metrics,
                            )
                            .id("hosts.auth.vault.unlock")
                            .debug_selector(|| "hosts.auth.vault.unlock".to_owned())
                            .on_click(cx.listener(
                                |this, _event, _window, cx| {
                                    this.security_begin(Vec::new(), cx);
                                },
                            )),
                        ),
                );
            } else {
                if chosen.is_some_and(|id| self.security_vault_credential_missing(id)) {
                    picker = picker.child(
                        components::banner(Tone::Warning, metrics)
                            .debug_selector(|| "hosts.auth.credential.missing".to_owned())
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(security.vault_credential_missing_tip),
                    );
                }
                let mut shared = self
                    .security
                    .content()
                    .map(|vault| {
                        vault
                            .credentials()
                            .filter(|record| record.meta().is_live())
                            .map(|record| record.meta().clone())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                shared.sort_by(|a, b| a.name().cmp(b.name()));
                if !shared.is_empty() {
                    picker = picker.child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(security.host_credential_pick),
                    );
                }
                for (index, meta) in shared.iter().enumerate() {
                    let id = meta.id();
                    let selected = chosen == Some(id);
                    let label = match meta.user() {
                        Some(user) => format!("{} · {}", meta.name(), user),
                        None => meta.name().to_owned(),
                    };
                    let selector = format!("hosts.auth.credential.{index}");
                    let reserved = components::compact_label_reserved_width(&label, metrics);
                    picker = picker.child(
                        components::compact_button(
                            SharedString::from(label.clone()),
                            if selected {
                                components::Variant::Primary
                            } else {
                                components::Variant::Secondary
                            },
                            metrics,
                        )
                        .min_w(reserved)
                        .max_w_full()
                        .flex_shrink()
                        .whitespace_normal()
                        .h(gpui::auto())
                        .min_h(metrics.compact_control_height())
                        .id(SharedString::from(selector.clone()))
                        .debug_selector({
                            let selector = selector.clone();
                            move || selector.clone()
                        })
                        .on_click(cx.listener(
                            move |this, _event, _window, cx| {
                                if let Some(draft) = this.hosts.draft_mut() {
                                    // Choosing a record again goes back to a typed
                                    // password, which is the other way this method
                                    // signs in.
                                    let next = (draft.credential_id() != Some(id)).then_some(id);
                                    draft.set_credential(next);
                                }
                                cx.notify();
                            },
                        )),
                    );
                }
                picker = picker.child(
                    div().flex().items_center().child(
                        components::compact_button(
                            security.new_password,
                            components::Variant::Secondary,
                            metrics,
                        )
                        .id("hosts.auth.credential.new")
                        .debug_selector(|| "hosts.auth.credential.new".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.security_open_inline(
                                    super::security::SecurityAction::NewCredential,
                                    super::security::PendingVaultBind::SshSharedCredential,
                                    cx,
                                );
                            },
                        )),
                    ),
                );
            }
            if chosen.is_some() {
                picker = picker.child(
                    components::compact_button(
                        security.host_credential_clear,
                        components::Variant::Ghost,
                        metrics,
                    )
                    .id("hosts.auth.credential.clear")
                    .debug_selector(|| "hosts.auth.credential.clear".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        if let Some(draft) = this.hosts.draft_mut() {
                            draft.set_credential(None);
                        }
                        cx.notify();
                    })),
                );
            }
            column = column.child(picker);
        }

        if auth.needs_key_path() {
            let has_passphrase = auth.needs_passphrase();
            let identity_path = draft.text(HostField::IdentityFile).trim();
            let missing_key = !identity_path.is_empty() && self.key_presence.missing(identity_path);
            column = column
                .child(self.dialog_field(fields, HostField::IdentityFile, None, 1.0))
                .when(missing_key, |col| {
                    let backup = copy::backup_for(self.workspace.locale());
                    let tip = copy::fill(backup.host_key_missing_tip, &[("path", identity_path)]);
                    col.child(
                        div()
                            .mt(metrics.scale(-(spacing::HAIRLINE)))
                            .text_size(metrics.caption_size())
                            .text_color(Tone::Warning.text())
                            .debug_selector(|| "hosts.identity.missing".to_owned())
                            .child(SharedString::from(tip)),
                    )
                })
                .child(
                    div()
                        .mt(metrics.scale(-(spacing::HAIRLINE)))
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .child(strings.host_identity_note),
                )
                .child(
                    checkbox(
                        "hosts.key.passphrase",
                        ux.host_key_has_passphrase,
                        has_passphrase,
                        metrics,
                    )
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.set_host_auth(
                            if has_passphrase {
                                AuthMethodKind::KeyFile
                            } else {
                                AuthMethodKind::EncryptedKeyFile
                            },
                            cx,
                        );
                    })),
                );
        }
        if draft.needs_secret() {
            let (label, remember_label) = if auth.needs_passphrase() {
                (
                    strings.host_secret_passphrase,
                    strings.host_remember_passphrase,
                )
            } else if auth == AuthMethodKind::Auto {
                (ux.host_password_optional, strings.host_remember_password)
            } else {
                (strings.auth_password, strings.host_remember_password)
            };
            column = column
                .child(self.dialog_field(fields, HostField::Secret, Some(label), 1.0))
                .child(
                    checkbox("hosts.remember", remember_label, draft.remember(), metrics)
                        .tooltip(components::tooltip(strings.host_remember_note))
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.toggle_remember(cx);
                        })),
                );
        }
        column
    }

    /// The folded section's head: what is inside it, and whether anything there is in use.
    fn advanced_toggle(&self, draft: &HostDraft, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let ux = copy::ux();
        let open = self.host_advanced;
        components::disclosure(open, ux.host_advanced_title, ux.host_advanced_hint, metrics)
            .id("hosts.advanced")
            .debug_selector(|| "hosts.advanced".to_owned())
            .when(draft.has_advanced(), |row| {
                row.child(components::chip(
                    ux.host_advanced_in_use,
                    Tone::Accent,
                    metrics,
                ))
            })
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.host_advanced = !this.host_advanced;
                // The jump picker is inside the section; folding it away closes it.
                if !this.host_advanced {
                    this.jump_picker = None;
                }
                cx.notify();
            }))
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
            .gap(metrics.scale(spacing::NARROW))
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
            .gap(metrics.scale(spacing::NARROW))
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

    /// The host's own keepalive choice: inherit the global default, switch the probe off,
    /// or state its own numbers (R20).
    ///
    /// The three states are not two: "inherit" follows a default that can change later,
    /// "off" keeps that default from applying to this host at all, and an override states
    /// what it does. The note under the switch says which of the three is in force, and
    /// how long a silent connection would take to be noticed.
    fn connection_section(
        &self,
        draft: &HostDraft,
        metrics: Metrics,
        cx: &mut Context<Self>,
    ) -> Div {
        let ux = copy::ux();
        let connection = draft.connection();
        let choice = connection.choice();
        let defaults = self.connection_defaults();
        let mut row =
            components::segmented(metrics).debug_selector(|| "hosts.editor.connection".to_owned());
        for candidate in ConnectionChoice::ALL {
            let selector = format!("hosts.editor.connection.{}", candidate.id());
            row = row.child(
                components::segment(candidate.label(ux), candidate == choice, metrics)
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        view.set_host_connection_choice(candidate, cx);
                    })),
            );
        }
        let inherited = copy::fill(
            ux.connection_summary,
            &[
                ("interval", &defaults.keepalive_interval_secs().to_string()),
                ("misses", &defaults.keepalive_max_misses().to_string()),
                ("timeout", &defaults.connect_timeout_secs().to_string()),
            ],
        );
        let note = match choice {
            // With keepalive off everywhere there is no "probes every 0s" to promise;
            // the sentence names the timeout that still applies (R20).
            ConnectionChoice::Inherit if defaults.keepalive_interval_secs() == 0 => copy::fill(
                ux.host_connection_inherits_off,
                &[("timeout", &defaults.connect_timeout_secs().to_string())],
            ),
            ConnectionChoice::Inherit => {
                copy::fill(ux.host_connection_inherits, &[("summary", &inherited)])
            }
            ConnectionChoice::Off => ux.host_connection_off_note.to_owned(),
            ConnectionChoice::Override => ux.host_connection_override_note.to_owned(),
        };
        let mut column = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::NARROW))
            .child(div().flex().child(row))
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "hosts.connection.note".to_owned())
                    .child(SharedString::from(note)),
            );
        if choice == ConnectionChoice::Override {
            column = column.child(
                div()
                    .flex()
                    .flex_wrap()
                    .w_full()
                    .min_w_0()
                    .gap(metrics.scale(spacing::SNUG))
                    .child(self.host_connection_field(
                        draft,
                        ConnectionField::KeepaliveInterval,
                        metrics,
                    ))
                    .child(self.host_connection_field(
                        draft,
                        ConnectionField::KeepaliveMisses,
                        metrics,
                    ))
                    .child(self.host_connection_field(
                        draft,
                        ConnectionField::ConnectTimeout,
                        metrics,
                    )),
            );
        }
        // The sentence a person needs is about time, not probe counts: how long the
        // connection may go without a reply before it is treated as lost. Inheriting shows
        // the window the defaults add up to, so the three states are comparable at a
        // glance (R20).
        let window = match choice {
            ConnectionChoice::Off => None,
            ConnectionChoice::Inherit => {
                if defaults.keepalive_interval_secs() == 0 {
                    None
                } else {
                    Some(crate::app::hosts::detection_window_secs(
                        defaults.keepalive_interval_secs(),
                        defaults.keepalive_max_misses(),
                    ))
                }
            }
            ConnectionChoice::Override => connection.detection_window_secs(),
        };
        let window = match window {
            Some(seconds) => copy::fill(ux.connection_window, &[("seconds", &seconds.to_string())]),
            None => ux.connection_window_off.to_owned(),
        };
        column.child(
            div()
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .debug_selector(|| "hosts.connection.window".to_owned())
                .child(SharedString::from(window)),
        )
    }

    /// One override number of the host form, with the reason it was refused.
    fn host_connection_field(
        &self,
        draft: &HostDraft,
        field: ConnectionField,
        metrics: Metrics,
    ) -> Div {
        let locale = self.workspace.locale();
        let ux = copy::ux();
        let Some(fields) = self.host_connection_fields.as_ref() else {
            return div();
        };
        let mut column = div()
            .flex()
            .flex_col()
            .flex_grow()
            .flex_basis(gpui::relative(0.33))
            .min_w(metrics.scale(CONNECTION_FIELD_MIN_WIDTH))
            .gap(metrics.scale(spacing::NARROW))
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(field.label(ux)),
            )
            .child(
                div()
                    .flex()
                    .w_full()
                    .min_w_0()
                    .debug_selector(move || format!("hosts.connection.{}", field.id()))
                    .child(fields.get(field).clone()),
            );
        if let Some((refused, problem)) = draft.connection().error()
            && refused == field
        {
            column = column.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(Tone::Danger.text())
                    .debug_selector(move || format!("hosts.connection.error.{}", field.id()))
                    .child(SharedString::from(connection_problem_text(problem, locale))),
            );
        }
        column
    }

    /// Opens the group dropdown again after it was put away.
    pub(super) fn reopen_group_picker(&mut self) {
        self.group_picker_closed = false;
        self.group_picker_highlight = 0;
        self.group_picker_scroll.scroll_to_item(0);
    }

    /// Whether the group dropdown is on screen in either form.
    pub(super) fn group_picker_shown(&self, window: &Window, cx: &App) -> bool {
        !self.group_picker_closed
            && [true, false].into_iter().any(|for_ssh| {
                self.group_field_focused(for_ssh, window, cx)
                    && !self
                        .group_choices(&self.group_field_text(for_ssh))
                        .is_empty()
            })
    }

    /// Whether the open form's Group field holds the keyboard.
    fn group_field_focused(&self, for_ssh: bool, window: &Window, cx: &App) -> bool {
        if for_ssh {
            self.host_fields
                .as_ref()
                .is_some_and(|f| f.group.read(cx).focus_handle().is_focused(window))
        } else {
            self.protocol_fields
                .as_ref()
                .and_then(|f| f.get(ConnectionFormField::Group))
                .is_some_and(|f| f.read(cx).focus_handle().is_focused(window))
        }
    }

    /// The text of the open form's Group field.
    fn group_field_text(&self, for_ssh: bool) -> String {
        if for_ssh {
            self.hosts
                .draft()
                .map(|d| d.text(HostField::Group).to_owned())
                .unwrap_or_default()
        } else {
            self.hosts
                .protocol_draft()
                .map(|d| d.text(ConnectionFormField::Group).to_owned())
                .unwrap_or_default()
        }
    }

    /// What the group dropdown offers for what was typed (R12).
    fn group_choices(&self, current: &str) -> Vec<GroupChoice> {
        let typed = current.trim();
        let paths = crate::app::hosts::filtered_group_paths(&self.book, &self.groups, typed);
        let exact = paths
            .iter()
            .any(|path| path.to_canonical().eq_ignore_ascii_case(typed));
        let mut choices: Vec<GroupChoice> = paths.into_iter().map(GroupChoice::Saved).collect();
        // A path nobody saved yet is still a valid answer: the save creates it. The
        // row says so, instead of a list that only ever shows what already exists.
        if !typed.is_empty() && !exact && GroupPath::parse(typed).is_ok() {
            choices.push(GroupChoice::New(typed.to_owned()));
        }
        choices
    }

    /// The group dropdown under the form's Group field, while that field is focused
    /// (R12).
    ///
    /// It is a combo box, not a list beside the form: it opens under the field being
    /// typed in, floats over what follows instead of pushing it down, narrows and
    /// ranks the saved groups by every word typed, marks why each one matched, counts
    /// what each group holds, and offers the typed path as a new group when none has
    /// it. Up and Down move, Enter picks, Escape puts it away; a click picks too.
    fn group_dropdown(
        &self,
        prefix: &'static str,
        for_ssh: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if self.group_picker_closed || !self.group_field_focused(for_ssh, window, cx) {
            return None;
        }
        let current = self.group_field_text(for_ssh);
        let choices = self.group_choices(&current);
        if choices.is_empty() {
            return None;
        }
        let metrics = self.metrics();
        let ux = copy::ux();
        let typed = current.trim().to_owned();
        let row_height = metrics.compact_control_height();
        let highlighted = self.group_picker_highlight.min(choices.len() - 1);
        let hover = theme::swatch(palette::SURFACE_HOVER);

        let mut list = div()
            .id(SharedString::from(format!(
                "{prefix}.group.suggestions.scroll"
            )))
            .flex()
            .flex_col()
            .gap(px(components::HAIRLINE_WIDTH))
            .w_full()
            .p(metrics.scale(spacing::HAIRLINE))
            .max_h(
                row_height * GROUP_PICKER_ROWS
                    + px(components::HAIRLINE_WIDTH) * (GROUP_PICKER_ROWS - 1.0)
                    + metrics.scale(spacing::HAIRLINE) * 2.0,
            )
            .overflow_y_scroll()
            .track_scroll(&self.group_picker_scroll);

        for (index, choice) in choices.into_iter().enumerate() {
            let active = index == highlighted;
            let id = format!("{prefix}.group.pick.{index}");
            let selector = id.clone();
            let (icon, label, count, chosen, value) = match &choice {
                GroupChoice::Saved(path) => {
                    let text = path.to_canonical();
                    let chosen = text == typed;
                    (
                        Icon::Folder,
                        text.clone(),
                        Some(crate::app::hosts::records_under(&self.book, path)),
                        chosen,
                        text,
                    )
                }
                GroupChoice::New(text) => (
                    Icon::Plus,
                    copy::fill(ux.hosts_group_picker_create, &[("name", text)]),
                    None,
                    false,
                    text.clone(),
                ),
            };
            let marks = match &choice {
                GroupChoice::Saved(_) if !typed.is_empty() => {
                    crate::app::palette::highlights(&label, &typed)
                }
                _ => Vec::new(),
            };
            let ink = if matches!(choice, GroupChoice::New(_)) {
                palette::ACCENT
            } else {
                palette::TEXT_PRIMARY
            };
            list = list.child(
                div()
                    .id(SharedString::from(id))
                    .debug_selector(move || selector.clone())
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .h(row_height)
                    .px(metrics.scale(spacing::TIGHT))
                    .rounded(px(radius::SMALL))
                    .cursor_pointer()
                    .when(active, |row| {
                        row.bg(theme::swatch(palette::SURFACE_SELECTED))
                    })
                    .when(!active, |row| row.hover(move |style| style.bg(hover)))
                    .child(icons::icon(
                        icon,
                        metrics.small_icon_size(),
                        theme::swatch(if chosen || matches!(choice, GroupChoice::New(_)) {
                            palette::ACCENT
                        } else {
                            palette::TEXT_MUTED
                        }),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(metrics.label_size())
                            .text_color(theme::swatch(ink))
                            .child(components::highlighted_text(label, marks)),
                    )
                    .children(count.map(|count| {
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(count.to_string()))
                    }))
                    .when(chosen, |row| {
                        row.child(icons::icon(
                            Icon::Check,
                            metrics.small_icon_size(),
                            theme::swatch(palette::ACCENT),
                        ))
                    })
                    // The press is taken before the field loses the keyboard, so the
                    // pick lands and the caret stays where the person was typing.
                    .on_mouse_down(gpui::MouseButton::Left, |_event, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.pick_dialog_group(for_ssh, value.clone(), cx);
                    })),
            );
        }

        let panel = components::popover_surface()
            .occlude()
            .flex()
            .flex_col()
            .w_full()
            .overflow_hidden()
            .debug_selector(move || format!("{prefix}.group.suggestions"))
            .child(
                div()
                    .relative()
                    .w_full()
                    .child(list)
                    .child(crate::app::scrollbar::bar(
                        SharedString::from(format!("{prefix}.group.suggestions.bar")),
                        self.group_picker_scroll.clone(),
                    )),
            );
        Some(
            gpui::deferred(
                div()
                    .absolute()
                    .top_full()
                    .left_0()
                    .right_0()
                    .mt(metrics.scale(spacing::HALF))
                    .child(panel),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// Puts a picked group into the field and closes the dropdown.
    fn pick_dialog_group(&mut self, for_ssh: bool, value: String, cx: &mut Context<Self>) {
        self.apply_dialog_group_text(for_ssh, value, cx);
        self.group_picker_closed = true;
        cx.notify();
    }

    /// Writes `next` into the open SSH or non-SSH dialog's Group field and draft.
    fn apply_dialog_group_text(&mut self, for_ssh: bool, next: String, cx: &mut Context<Self>) {
        self.group_picker_highlight = 0;
        self.group_picker_scroll.scroll_to_item(0);
        if for_ssh {
            if let Some(draft) = self.hosts.draft_mut() {
                draft.set_text(HostField::Group, next.clone());
            }
            if let Some(fields) = self.host_fields.as_ref() {
                let group_field = fields.group.clone();
                group_field.update(cx, |f, cx| f.set_text(next, cx));
            }
        } else {
            if let Some(draft) = self.hosts.protocol_draft_mut() {
                draft.set_text(ConnectionFormField::Group, next.clone());
            }
            if let Some(fields) = self.protocol_fields.as_ref()
                && let Some(group_field) = fields.get(ConnectionFormField::Group).cloned()
            {
                group_field.update(cx, |f, cx| f.set_text(next, cx));
            }
        }
        cx.notify();
    }

    /// Handles Up/Down/Enter while the dialog's Group field is focused (R12).
    fn handle_group_picker_key(
        &mut self,
        for_ssh: bool,
        event: &KeyDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if event.keystroke.modifiers.number_of_modifiers() != 0
            || !self.group_field_focused(for_ssh, window, cx)
        {
            return false;
        }
        let current = self.group_field_text(for_ssh);
        let choices = self.group_choices(&current);
        if choices.is_empty() {
            return false;
        }
        let last = choices.len() - 1;
        let open = !self.group_picker_closed;
        match event.keystroke.key.as_str() {
            "down" if !open => {
                self.reopen_group_picker();
                cx.notify();
                true
            }
            "down" => {
                self.group_picker_highlight = (self.group_picker_highlight + 1).min(last);
                self.group_picker_scroll
                    .scroll_to_item(self.group_picker_highlight);
                cx.notify();
                true
            }
            "up" if open => {
                self.group_picker_highlight =
                    self.group_picker_highlight.saturating_sub(1).min(last);
                self.group_picker_scroll
                    .scroll_to_item(self.group_picker_highlight);
                cx.notify();
                true
            }
            // Enter on an open dropdown picks; on a closed one it is the form's own
            // Enter, which saves.
            "enter" if open => {
                let value = match &choices[self.group_picker_highlight.min(last)] {
                    GroupChoice::Saved(path) => path.to_canonical(),
                    GroupChoice::New(text) => text.clone(),
                };
                self.pick_dialog_group(for_ssh, value, cx);
                true
            }
            _ => false,
        }
    }

    /// The dialog over a dimmed window, while a host is being created or edited.
    ///
    /// # Keepalive
    ///
    /// The keepalive choice sits beside the X11 mode: both are facts about how this host
    /// is reached, and both apply to the next connection rather than to a live one. The
    /// numbers are the same three the settings page shows, resolved against the defaults
    /// in force, so an override never starts from a number nobody chose.
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
        let editing = draft.editing().is_some();

        // Two fields side by side that stack when the dialog is too narrow (ADR-7).
        let line = |left: Div, right: Div| components::form_row(metrics).child(left).child(right);
        let ux = copy::ux();
        let group = |icon: Icon, title: &'static str| components::form_group(icon, title, metrics);

        // What a person needs to reach a server comes first - where, who, how - and
        // only that is required. Each part is a titled group of its own (ADR-7):
        // where and who, then how to sign in; everything a first host never needs
        // folds away under one line that says what is inside, and opens as a group
        // per subject - the route, the startup command, transfers, X11, keepalive.
        // The dialog's own body scrolls; this column only lays the form out.
        let mut body = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .py(metrics.scale(spacing::BASE))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "enter"
                    && event.keystroke.modifiers.number_of_modifiers() == 1
                    && event.keystroke.modifiers.secondary()
                {
                    cx.stop_propagation();
                    this.save_and_connect(cx);
                    return;
                }
                if this.handle_group_picker_key(true, event, window, cx) {
                    cx.stop_propagation();
                }
            }));
        // A new record may be of another protocol. The chooser is here so the form the
        // user sees is always the one the chosen protocol really has (R2, R18); a saved
        // host keeps the protocol it was saved with, so editing never shows it.
        if !editing {
            body = body.child(self.protocol_chooser(ProtocolKind::Ssh, cx));
        }
        body = body
            .child(
                group(Icon::Server, ux.host_group_basic)
                    .debug_selector(|| "hosts.group.basic".to_owned())
                    // Where and who on one line, the way it is written - user, host,
                    // port - and stacked when the dialog is too narrow for the three.
                    .child(
                        components::form_row(metrics)
                            .child(self.dialog_field(fields, HostField::Address, None, 0.5))
                            .child(self.dialog_field(fields, HostField::Port, None, 0.18))
                            .child(self.dialog_field(fields, HostField::User, None, 0.32)),
                    )
                    .child(line(
                        self.dialog_field(
                            fields,
                            HostField::Name,
                            Some(ux.host_name_optional),
                            0.5,
                        ),
                        self.dialog_field_with(
                            fields,
                            HostField::Group,
                            Some(ux.host_group_optional),
                            0.5,
                            self.group_dropdown("hosts", true, window, cx),
                        ),
                    )),
            )
            .child(
                group(Icon::Key, strings.host_auth_title)
                    .debug_selector(|| "hosts.group.auth".to_owned())
                    .child(self.sign_in_fields(fields, &draft, cx)),
            )
            .child(self.advanced_toggle(&draft, cx));

        if self.host_advanced {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(spacing::SNUG))
                    .debug_selector(|| "hosts.advanced.body".to_owned())
                    .child(
                        group(Icon::Forward, ux.host_group_route)
                            .debug_selector(|| "hosts.group.route".to_owned())
                            .child(self.jump_section(fields, &draft, cx)),
                    )
                    .child(
                        group(Icon::Terminal, strings.host_startup_label)
                            .debug_selector(|| "hosts.group.startup".to_owned())
                            .child(
                                div()
                                    .flex()
                                    .w_full()
                                    .debug_selector(|| {
                                        HostField::StartupCommand.debug_selector().to_owned()
                                    })
                                    .child(fields.startup.clone()),
                            )
                            .child(components::form_hint(strings.host_startup_note, metrics)),
                    )
                    .child(
                        group(Icon::Transfer, ux.host_group_transfer)
                            .debug_selector(|| "hosts.group.transfer".to_owned())
                            .child(self.copy_protocol_section(&draft, metrics, strings, cx)),
                    )
                    .child(
                        group(Icon::Monitor, strings.x11_label)
                            .debug_selector(|| "hosts.group.x11".to_owned())
                            .child(self.x11_section(&draft, metrics, strings, cx)),
                    )
                    .child(
                        group(Icon::Clock, ux.host_connection_title)
                            .debug_selector(|| "hosts.group.keepalive".to_owned())
                            .child(self.connection_section(&draft, metrics, cx)),
                    ),
            );
        }

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

        let (header, subtitle) = self.modal_header(
            Some(Icon::Server),
            if editing {
                strings.host_editor_edit
            } else {
                strings.host_editor_new
            }
            .to_owned(),
            Some(ux.host_dialog_intro.to_owned()),
            "hosts.editor.close",
            cx,
            |this, cx| this.close_host_editor(cx),
        );

        // The refusal takes a line of its own above the actions; the actions stay on
        // the right, the one that connects last and filled.
        let save_connect_shortcut =
            if crate::app::Platform::current() == crate::app::Platform::MacOs {
                "⌘↵"
            } else {
                "Ctrl+Enter"
            };
        let footer = components::modal_footer(metrics)
            .children(refusal.map(|refusal| refusal.w_full()))
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
                .h(metrics.control_height())
                .tooltip(components::tooltip(SharedString::from(format!(
                    "{} (Enter)",
                    strings.host_save
                )))),
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
                .h(metrics.control_height())
                .tooltip(components::tooltip(SharedString::from(format!(
                    "{} ({save_connect_shortcut})",
                    strings.host_save_connect
                )))),
            );

        Some(self.modal_with(
            window,
            HOST_EDITOR_ID,
            640.0,
            ModalContent {
                header,
                subtitle,
                body: body.into_any_element(),
                footer: Some(footer),
            },
            ModalBehaviour {
                scroll: Some(self.host_body_scroll.clone()),
                dismiss: None,
                ..Default::default()
            },
        ))
    }

    // -----------------------------------------------------------------------
    // The form of a connection that is not an SSH host (R2, R18)
    // -----------------------------------------------------------------------

    /// The protocol of a new connection.
    ///
    /// Shown only while creating: a saved connection keeps the protocol it was saved
    /// with, because changing it would be a different record rather than an edit.
    fn protocol_chooser(&self, current: ProtocolKind, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let mut choices = components::segmented(metrics);
        for protocol in ProtocolKind::ALL {
            // The track spans the form, so its four choices share it equally rather
            // than bunching at its left end beside an empty groove.
            choices = choices.child(
                components::segment(protocol.label(), protocol == current, metrics)
                    .flex_1()
                    .justify_center()
                    .id(SharedString::from(format!(
                        "connection.protocol.{}",
                        protocol.wire_name()
                    )))
                    .debug_selector(move || format!("connection.protocol.{}", protocol.wire_name()))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.switch_form_protocol(protocol, cx);
                    })),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::NARROW))
            .child(components::form_label(
                strings.connection_field_protocol,
                metrics,
            ))
            .child(choices)
    }

    /// One typed field of the protocol form.
    fn protocol_field(&self, field: ConnectionFormField) -> Div {
        self.protocol_field_sized(field, 0.5, None)
    }

    /// One typed field of the protocol form, taking the share of the row it is given
    /// with unified short-field floor and optional label support (R12).
    fn protocol_field_sized(
        &self,
        field: ConnectionFormField,
        grow: f32,
        label: Option<&'static str>,
    ) -> Div {
        self.protocol_field_with(field, grow, label, None)
    }

    /// A field of a protocol form with something that floats under its input.
    fn protocol_field_with(
        &self,
        field: ConnectionFormField,
        grow: f32,
        label: Option<&'static str>,
        attached: Option<gpui::AnyElement>,
    ) -> Div {
        let metrics = self.metrics();
        let label = label.unwrap_or_else(|| field.label(self.strings()));
        let entity = self
            .protocol_fields
            .as_ref()
            .and_then(|fields| fields.get(field))
            .cloned();
        let floor = if matches!(
            field,
            ConnectionFormField::Port | ConnectionFormField::Baud | ConnectionFormField::DataBits
        ) {
            components::FORM_SHORT_FIELD_MIN_WIDTH
        } else {
            components::FORM_FIELD_MIN_WIDTH
        };
        let mut cell = components::form_cell(grow, floor, metrics)
            .child(components::form_label(label, metrics));
        if let Some(entity) = entity {
            cell = cell.child(
                div()
                    .relative()
                    .flex()
                    .w_full()
                    .min_w_0()
                    .debug_selector(move || field.debug_selector().to_owned())
                    .child(entity)
                    .children(attached),
            );
        }
        // A refusal is shown under the field that produced it, and only once the form
        // has really been asked to save: a form still being filled is not an error.
        let problem = self
            .hosts
            .message()
            .filter(|message| message.failed())
            .and_then(|_| {
                self.hosts
                    .protocol_draft()
                    .and_then(|draft| draft.build().err())
            })
            .filter(|problem| problem.field() == field);
        if let Some(problem) = problem {
            cell = cell.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::DANGER))
                    .debug_selector(move || format!("{}.problem", field.debug_selector()))
                    .child(SharedString::from(problem.reason().to_owned())),
            );
        }
        cell
    }

    /// The form of a saved desktop or serial connection.
    pub(super) fn connection_dialog_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let draft = self.hosts.protocol_draft()?.clone();
        let metrics = self.metrics();
        let strings = self.strings();
        let editing = draft.editing().is_some();

        // Two fields side by side that stack when the dialog is too narrow (ADR-7).
        let line = |left: Div, right: Div| components::form_row(metrics).child(left).child(right);
        // The dialog's own body scrolls; this column only lays the form out.
        let mut body = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .py(metrics.scale(spacing::BASE))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "enter"
                    && event.keystroke.modifiers.number_of_modifiers() == 1
                    && event.keystroke.modifiers.secondary()
                {
                    cx.stop_propagation();
                    this.save_and_connect_connection(cx);
                    return;
                }
                if this.handle_group_picker_key(false, event, window, cx) {
                    cx.stop_propagation();
                }
            }));

        if !editing {
            body = body.child(self.protocol_chooser(draft.protocol(), cx));
        }

        // Two titled groups (ADR-7): where and who, then what this protocol lets the
        // user choose. Only the fields this protocol has: no identity file, no secret, no jump chain
        // and no forwarding appears for a connection that cannot use them.
        let ux = copy::ux();
        let mut basic = components::form_group(Icon::Server, ux.host_group_basic, metrics)
            .debug_selector(|| "connection.group.basic".to_owned());
        let mut options = components::form_group(Icon::Settings, ux.host_group_options, metrics)
            .debug_selector(|| "connection.group.options".to_owned());
        let mut has_options = false;
        for field in draft.fields() {
            let field = *field;
            match field {
                ConnectionFormField::Address => {
                    basic = basic.child(line(
                        self.protocol_field_sized(ConnectionFormField::Address, 0.72, None),
                        self.protocol_field_sized(ConnectionFormField::Port, 0.28, None),
                    ));
                }
                ConnectionFormField::Port => {}
                // VNC asks for a user name only on some servers: it stands alone, since a
                // domain has no place in it.
                ConnectionFormField::User
                    if !draft.fields().contains(&ConnectionFormField::Domain) =>
                {
                    basic = basic.child(self.protocol_field_sized(
                        ConnectionFormField::User,
                        1.0,
                        Some(copy::ux().host_user_optional),
                    ));
                }
                ConnectionFormField::User => {
                    basic = basic.child(line(
                        self.protocol_field(ConnectionFormField::User),
                        self.protocol_field(ConnectionFormField::Domain),
                    ));
                }
                ConnectionFormField::Domain => {}
                ConnectionFormField::Secret => {
                    // A desktop sign-in is a password. The user may type one, or name
                    // a shared credential of the vault the way the host form's
                    // password method does: the record keeps the reference, and every
                    // connection that names it shares the one password (R1). A chosen
                    // credential takes the typed field and its box away - the vault
                    // holds the password, so there is nothing to type and nothing to
                    // keep in the system store.
                    let security = copy::security_for(self.workspace.locale());
                    let chosen = draft.credential_id();
                    let mut picker = div()
                        .flex()
                        .flex_col()
                        .gap(metrics.scale(spacing::HAIRLINE))
                        .debug_selector(|| "connection.auth.credential.picker".to_owned());
                    if draft.needs_secret() {
                        // The password a RDP or VNC server asks for, typed here
                        // because the record has nowhere to keep it and the attempt
                        // starts after the form closes. The box decides whether it is
                        // kept in the store.
                        let remembered = draft.remember();
                        basic = basic.child(
                            self.protocol_field_sized(ConnectionFormField::Secret, 1.0, None)
                                .child(
                                    checkbox(
                                        "connection.field.remember",
                                        strings.host_remember_password,
                                        remembered,
                                        metrics,
                                    )
                                    .tooltip(components::tooltip(strings.host_remember_note))
                                    .on_click(cx.listener(
                                        move |this, _event, _window, cx| {
                                            let next = this
                                                .hosts
                                                .protocol_draft()
                                                .is_some_and(|draft| !draft.remember());
                                            if let Some(draft) = this.hosts.protocol_draft_mut() {
                                                draft.set_remember(next);
                                            }
                                            cx.notify();
                                        },
                                    )),
                                ),
                        );
                    }
                    if self.vault_locked() {
                        let locked_note = if chosen.is_some() {
                            security.host_vault_locked_bound_credential
                        } else {
                            security.host_vault_locked_credential
                        };
                        picker = picker.child(
                            components::banner(Tone::Warning, metrics)
                                .debug_selector(|| "connection.auth.vault.locked".to_owned())
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .justify_between()
                                .gap(metrics.scale(spacing::TIGHT))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .whitespace_normal()
                                        .text_size(metrics.caption_size())
                                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                        .child(locked_note),
                                )
                                .child(
                                    components::compact_button(
                                        if self.security_verifying() {
                                            security.lock_state_verifying
                                        } else {
                                            security.lock_unlock
                                        },
                                        components::Variant::Primary,
                                        metrics,
                                    )
                                    .id("connection.auth.vault.unlock")
                                    .debug_selector(|| "connection.auth.vault.unlock".to_owned())
                                    .on_click(cx.listener(
                                        |this, _event, _window, cx| {
                                            this.security_begin(Vec::new(), cx);
                                        },
                                    )),
                                ),
                        );
                    } else {
                        if chosen.is_some_and(|id| self.security_vault_credential_missing(id)) {
                            picker = picker.child(
                                components::banner(Tone::Warning, metrics)
                                    .debug_selector(|| {
                                        "connection.auth.credential.missing".to_owned()
                                    })
                                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                    .child(security.vault_credential_missing_tip),
                            );
                        }
                        let mut shared = self
                            .security
                            .content()
                            .map(|vault| {
                                vault
                                    .credentials()
                                    .filter(|record| record.meta().is_live())
                                    .map(|record| record.meta().clone())
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        shared.sort_by(|a, b| a.name().cmp(b.name()));
                        if !shared.is_empty() {
                            picker = picker.child(
                                div()
                                    .text_size(metrics.caption_size())
                                    .text_color(theme::swatch(palette::TEXT_MUTED))
                                    .child(security.host_credential_pick),
                            );
                        }
                        for (index, meta) in shared.iter().enumerate() {
                            let id = meta.id();
                            let selected = chosen == Some(id);
                            let label = match meta.user() {
                                Some(user) => format!("{} · {}", meta.name(), user),
                                None => meta.name().to_owned(),
                            };
                            let selector = format!("connection.auth.credential.{index}");
                            let reserved =
                                components::compact_label_reserved_width(&label, metrics);
                            picker = picker.child(
                                components::compact_button(
                                    SharedString::from(label),
                                    if selected {
                                        components::Variant::Primary
                                    } else {
                                        components::Variant::Secondary
                                    },
                                    metrics,
                                )
                                .min_w(reserved)
                                .max_w_full()
                                .flex_shrink()
                                .whitespace_normal()
                                .h(gpui::auto())
                                .min_h(metrics.compact_control_height())
                                .id(SharedString::from(selector.clone()))
                                .debug_selector({
                                    let selector = selector.clone();
                                    move || selector.clone()
                                })
                                .on_click(cx.listener(
                                    move |this, _event, _window, cx| {
                                        if let Some(draft) = this.hosts.protocol_draft_mut() {
                                            // Choosing a record again goes back to a
                                            // typed password, which is the other way a
                                            // desktop connection signs in.
                                            let next =
                                                (draft.credential_id() != Some(id)).then_some(id);
                                            draft.set_credential(next);
                                        }
                                        cx.notify();
                                    },
                                )),
                            );
                        }
                        picker = picker.child(
                            div().flex().items_center().child(
                                components::compact_button(
                                    security.new_password,
                                    components::Variant::Secondary,
                                    metrics,
                                )
                                .id("connection.auth.credential.new")
                                .debug_selector(|| "connection.auth.credential.new".to_owned())
                                .on_click(cx.listener(
                                    |this, _event, _window, cx| {
                                        this.security_open_inline(
                                        super::security::SecurityAction::NewCredential,
                                        super::security::PendingVaultBind::DesktopSharedCredential,
                                        cx,
                                    );
                                    },
                                )),
                            ),
                        );
                    }
                    if chosen.is_some() {
                        picker = picker.child(
                            components::compact_button(
                                security.host_credential_clear,
                                components::Variant::Ghost,
                                metrics,
                            )
                            .id("connection.auth.credential.clear")
                            .debug_selector(|| "connection.auth.credential.clear".to_owned())
                            .on_click(cx.listener(
                                |this, _event, _window, cx| {
                                    if let Some(draft) = this.hosts.protocol_draft_mut() {
                                        draft.set_credential(None);
                                    }
                                    cx.notify();
                                },
                            )),
                        );
                    }
                    basic = basic.child(picker);
                }
                ConnectionFormField::Certificate => {
                    let mut choices = components::segmented(metrics);
                    for policy in RdpCertificatePolicy::ALL {
                        choices = choices.child(
                            components::segment(
                                policy.label(),
                                draft.certificate() == policy,
                                metrics,
                            )
                            .id(SharedString::from(format!(
                                "connection.certificate.{}",
                                certificate_id(policy)
                            )))
                            .debug_selector(move || {
                                format!("connection.certificate.{}", certificate_id(policy))
                            })
                            .on_click(cx.listener(
                                move |this, _event, _window, cx| {
                                    if let Some(draft) = this.hosts.protocol_draft_mut() {
                                        draft.set_certificate(policy);
                                    }
                                    cx.notify();
                                },
                            )),
                        );
                    }
                    has_options = true;
                    options = options.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(metrics.scale(spacing::NARROW))
                            .debug_selector(|| "connection.field.certificate".to_owned())
                            .child(
                                div()
                                    .text_size(metrics.caption_size())
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                    .child(strings.connection_field_certificate),
                            )
                            .child(choices),
                    );
                }
                ConnectionFormField::Security => {
                    let mut choices = components::segmented(metrics);
                    for security in VncSecurity::ALL {
                        choices = choices.child(
                            components::segment(
                                security.label(),
                                draft.security() == security,
                                metrics,
                            )
                            .id(SharedString::from(format!(
                                "connection.security.{}",
                                security_id(security)
                            )))
                            .debug_selector(move || {
                                format!("connection.security.{}", security_id(security))
                            })
                            .on_click(cx.listener(
                                move |this, _event, _window, cx| {
                                    if let Some(draft) = this.hosts.protocol_draft_mut() {
                                        draft.set_security(security);
                                    }
                                    cx.notify();
                                },
                            )),
                        );
                    }
                    has_options = true;
                    options = options.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(metrics.scale(spacing::NARROW))
                            .debug_selector(|| "connection.field.security".to_owned())
                            .child(
                                div()
                                    .text_size(metrics.caption_size())
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                    .child(strings.connection_field_security),
                            )
                            .child(choices),
                    );
                }
                ConnectionFormField::Clipboard => {
                    // On unless the user turns it off: copy and paste between this
                    // computer and the remote desktop is what a session is expected
                    // to do (R6).
                    has_options = true;
                    options = options.child(
                        checkbox(
                            "connection.clipboard",
                            strings.connection_field_clipboard,
                            draft.clipboard(),
                            metrics,
                        )
                        .debug_selector(|| "connection.field.clipboard".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                let next = this
                                    .hosts
                                    .protocol_draft()
                                    .is_some_and(|draft| !draft.clipboard());
                                if let Some(draft) = this.hosts.protocol_draft_mut() {
                                    draft.set_clipboard(next);
                                }
                                cx.notify();
                            },
                        )),
                    );
                }
                ConnectionFormField::DriveFolder => {
                    // A blank folder is the mapping turned off, so the field is the
                    // switch and the choice at once; the note says what the remote
                    // side gets and that it gets nothing more (R5).
                    has_options = true;
                    options = options.child(
                        self.protocol_field(ConnectionFormField::DriveFolder).child(
                            div()
                                .whitespace_normal()
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED))
                                .debug_selector(|| "connection.drive_folder.note".to_owned())
                                .child(strings.connection_drive_folder_note),
                        ),
                    );
                }
                ConnectionFormField::Parity => {
                    let mut choices = components::segmented(metrics);
                    for parity in SerialParity::ALL {
                        choices = choices.child(
                            components::segment(parity.label(), draft.parity() == parity, metrics)
                                .id(SharedString::from(format!(
                                    "connection.parity.{}",
                                    parity_id(parity)
                                )))
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    if let Some(draft) = this.hosts.protocol_draft_mut() {
                                        draft.set_parity(parity);
                                    }
                                    cx.notify();
                                })),
                        );
                    }
                    has_options = true;
                    options = options.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(metrics.scale(spacing::NARROW))
                            .debug_selector(|| "connection.field.parity".to_owned())
                            .child(
                                div()
                                    .text_size(metrics.caption_size())
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                    .child(strings.connection_field_parity),
                            )
                            .child(choices),
                    );
                }
                ConnectionFormField::StopBits => {
                    let mut choices = components::segmented(metrics);
                    for stop_bits in SerialStopBits::ALL {
                        choices = choices.child(
                            components::segment(
                                stop_bits.label(),
                                draft.stop_bits() == stop_bits,
                                metrics,
                            )
                            .id(SharedString::from(format!(
                                "connection.stop_bits.{}",
                                stop_bits.label()
                            )))
                            .on_click(cx.listener(
                                move |this, _event, _window, cx| {
                                    if let Some(draft) = this.hosts.protocol_draft_mut() {
                                        draft.set_stop_bits(stop_bits);
                                    }
                                    cx.notify();
                                },
                            )),
                        );
                    }
                    has_options = true;
                    options = options.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(metrics.scale(spacing::NARROW))
                            .debug_selector(|| "connection.field.stop_bits".to_owned())
                            .child(
                                div()
                                    .text_size(metrics.caption_size())
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                    .child(strings.connection_field_stop_bits),
                            )
                            .child(choices),
                    );
                }
                ConnectionFormField::Flow => {
                    let mut choices = components::segmented(metrics);
                    for flow in SerialFlow::ALL {
                        choices = choices.child(
                            components::segment(flow.label(), draft.flow() == flow, metrics)
                                .id(SharedString::from(format!(
                                    "connection.flow.{}",
                                    flow_id(flow)
                                )))
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    if let Some(draft) = this.hosts.protocol_draft_mut() {
                                        draft.set_flow(flow);
                                    }
                                    cx.notify();
                                })),
                        );
                    }
                    has_options = true;
                    options = options.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(metrics.scale(spacing::NARROW))
                            .debug_selector(|| "connection.field.flow".to_owned())
                            .child(
                                div()
                                    .text_size(metrics.caption_size())
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                    .child(strings.connection_field_flow),
                            )
                            .child(choices),
                    );
                }
                ConnectionFormField::LocalEcho => {
                    let echo = draft.local_echo();
                    has_options = true;
                    options = options.child(
                        checkbox(
                            "connection.local_echo",
                            strings.connection_field_local_echo,
                            echo,
                            metrics,
                        )
                        .debug_selector(|| "connection.field.local_echo".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                let next = this
                                    .hosts
                                    .protocol_draft()
                                    .is_some_and(|draft| !draft.local_echo());
                                if let Some(draft) = this.hosts.protocol_draft_mut() {
                                    draft.set_local_echo(next);
                                }
                                cx.notify();
                            },
                        )),
                    );
                }
                ConnectionFormField::Device => {
                    basic = basic.child(line(
                        self.protocol_field_sized(ConnectionFormField::Device, 0.68, None),
                        self.protocol_field_sized(ConnectionFormField::Baud, 0.32, None),
                    ));
                }
                ConnectionFormField::Baud => {}
                ConnectionFormField::DataBits => {
                    has_options = true;
                    options = options.child(self.protocol_field_sized(
                        ConnectionFormField::DataBits,
                        0.32,
                        None,
                    ));
                }
                ConnectionFormField::Name => {
                    basic = basic.child(line(
                        self.protocol_field_sized(
                            ConnectionFormField::Name,
                            0.5,
                            Some(ux.host_name_optional),
                        ),
                        self.protocol_field_with(
                            ConnectionFormField::Group,
                            0.5,
                            Some(ux.host_group_optional),
                            self.group_dropdown("connection", false, window, cx),
                        ),
                    ));
                }
                ConnectionFormField::Group => {}
            }
        }
        body = body.child(basic);
        if has_options {
            body = body.child(options);
        }

        body = body.child(
            div()
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .debug_selector(|| "connection.not_started".to_owned())
                .child(strings.connection_not_started_note),
        );

        let refusal = self
            .hosts
            .message()
            .filter(|message| message.failed())
            .map(|message| {
                components::banner(Tone::Danger, metrics)
                    .debug_selector(|| "connection.message".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(message.text().to_owned()))
            });

        let (header, subtitle) = self.modal_header(
            Some(Icon::Plug),
            if editing {
                strings.connection_editor_edit
            } else {
                strings.connection_editor_new
            }
            .to_owned(),
            Some(draft.protocol().label().to_owned()),
            "connection.editor.close",
            cx,
            |this, cx| this.close_connection_editor(cx),
        );

        let save_connect_shortcut =
            if crate::app::Platform::current() == crate::app::Platform::MacOs {
                "⌘↵"
            } else {
                "Ctrl+Enter"
            };
        let footer = components::modal_footer(metrics)
            .children(refusal.map(|refusal| refusal.w_full()))
            .child(
                self.text_button(
                    SharedString::from("connection.cancel"),
                    strings.host_cancel,
                    Variant::Ghost,
                    cx,
                    |view, cx| view.close_connection_editor(cx),
                )
                .h(metrics.control_height()),
            )
            .child(
                self.text_button(
                    SharedString::from("connection.save"),
                    strings.host_save,
                    Variant::Secondary,
                    cx,
                    |view, cx| {
                        let _ = view.save_connection(cx);
                    },
                )
                .h(metrics.control_height())
                .tooltip(components::tooltip(SharedString::from(format!(
                    "{} (Enter)",
                    strings.host_save
                )))),
            )
            .child(
                self.icon_text_button(
                    SharedString::from("connection.save_connect"),
                    Icon::Plug,
                    strings.host_save_connect,
                    Variant::Primary,
                    cx,
                    |view, cx| view.save_and_connect_connection(cx),
                )
                .h(metrics.control_height())
                .tooltip(components::tooltip(SharedString::from(format!(
                    "{} ({save_connect_shortcut})",
                    strings.host_save_connect
                )))),
            );

        Some(self.modal_with(
            window,
            HOST_EDITOR_ID,
            640.0,
            ModalContent {
                header,
                subtitle,
                body: body.into_any_element(),
                footer: Some(footer),
            },
            ModalBehaviour {
                scroll: Some(self.host_body_scroll.clone()),
                dismiss: None,
                ..Default::default()
            },
        ))
    }
}

/// Stable identifier of a certificate policy, for a debug selector.
fn certificate_id(policy: RdpCertificatePolicy) -> &'static str {
    match policy {
        RdpCertificatePolicy::Ask => "ask",
        RdpCertificatePolicy::Reject => "reject",
    }
}

/// Stable identifier of a VNC security choice, for a debug selector.
fn security_id(security: VncSecurity) -> &'static str {
    match security {
        VncSecurity::Automatic => "automatic",
        VncSecurity::EncryptedOnly => "encrypted",
    }
}

/// Stable identifier of a parity choice, for a debug selector.
fn parity_id(parity: SerialParity) -> &'static str {
    match parity {
        SerialParity::None => "none",
        SerialParity::Even => "even",
        SerialParity::Odd => "odd",
        SerialParity::Mark => "mark",
        SerialParity::Space => "space",
    }
}

/// Stable identifier of a flow-control choice, for a debug selector.
fn flow_id(flow: SerialFlow) -> &'static str {
    match flow {
        SerialFlow::None => "none",
        SerialFlow::Software => "software",
        SerialFlow::Hardware => "hardware",
    }
}
