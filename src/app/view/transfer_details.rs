use super::*;
use shellune_core::{TransferFailure, TransferId, TransferState};

// None names the global list. Each session remembers its own keyboard position.
type Scope = Option<SessionId>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecoveryAction {
    Resume,
    Restart,
}

#[derive(Clone, Copy)]
struct Cursor {
    id: TransferId,
    expanded: bool,
    action: Option<RecoveryAction>,
}

impl Cursor {
    fn row(id: TransferId) -> Self {
        Self {
            id,
            expanded: false,
            action: None,
        }
    }
}

pub(super) struct Inspection {
    cursors: HashMap<Scope, Cursor>,
    pub(super) host_names: HashMap<SessionId, String>,
    pub(super) center_focus: FocusHandle,
    pub(super) center_scroll: gpui::ScrollHandle,
    center_anchor: gpui::ScrollAnchor,
    strip_anchor: gpui::ScrollAnchor,
}

impl Inspection {
    pub(super) fn new(cx: &mut Context<ShelluneView>, strip: gpui::ScrollHandle) -> Self {
        let center_scroll = gpui::ScrollHandle::new();
        Self {
            cursors: HashMap::new(),
            host_names: HashMap::new(),
            center_focus: cx.focus_handle(),
            center_anchor: gpui::ScrollAnchor::for_handle(center_scroll.clone()),
            center_scroll,
            strip_anchor: gpui::ScrollAnchor::for_handle(strip),
        }
    }

    pub(super) fn sync(&mut self, items: &[Transfer], workspace: &Workspace) {
        let sessions: BTreeSet<_> = items.iter().map(Transfer::session).collect();
        self.host_names.retain(|id, _| sessions.contains(id));
        for id in &sessions {
            if let Some(session) = workspace.session(*id).filter(|s| !s.title().is_empty()) {
                self.host_names.insert(*id, session.title().to_owned());
            }
        }
        self.cursors.retain(|scope, cursor| {
            if items
                .iter()
                .any(|task| task.id() == cursor.id && selectable(task))
            {
                return true;
            }
            if let Some(next) = items
                .iter()
                .find(|task| scope.is_none_or(|id| id == task.session()) && selectable(task))
            {
                *cursor = Cursor::row(next.id());
                return true;
            }
            false
        });
    }

    fn anchor(&self, scope: Scope) -> gpui::ScrollAnchor {
        if scope.is_some() {
            &self.strip_anchor
        } else {
            &self.center_anchor
        }
        .clone()
    }
}

pub(super) fn inspectable(task: &Transfer) -> bool {
    matches!(
        task.state(),
        TransferState::Failed { .. } | TransferState::Cancelled { .. }
    )
}

fn selectable(task: &Transfer) -> bool {
    inspectable(task) || (task.is_active() && !task.state().is_awaiting_overwrite())
}

fn can_resume(task: &Transfer) -> bool {
    task.resume_hint().is_some()
        && !matches!(
            task.state().failure(),
            Some(TransferFailure::ResumeRefused { .. })
        )
}

impl ShelluneView {
    fn transfer_list_focus(&self, scope: Scope) -> &FocusHandle {
        if scope.is_some() {
            &self.files_transfer_focus
        } else {
            &self.transfer_inspection.center_focus
        }
    }

    pub(crate) fn inspected_transfer(&self, scope: Option<SessionId>) -> Option<TransferId> {
        self.transfer_inspection
            .cursors
            .get(&scope)
            .filter(|c| c.expanded && self.transfers.get(c.id).is_some_and(inspectable))
            .map(|c| c.id)
    }

    #[cfg(test)]
    pub(crate) fn transfer_list_focused_for_test(
        &self,
        scope: Option<SessionId>,
        window: &Window,
    ) -> bool {
        self.transfer_list_focus(scope).is_focused(window)
    }

    fn place_transfer_cursor(
        &mut self,
        scope: Scope,
        cursor: Cursor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transfer_inspection.cursors.insert(scope, cursor);
        window.focus(self.transfer_list_focus(scope));
        let anchor = self.transfer_inspection.anchor(scope);
        // The updated row and the action it marks are laid out on the next draw.
        window.on_next_frame(move |window, cx| anchor.scroll_to(window, cx));
        cx.notify();
    }

    pub(super) fn close_transfer_detail(
        &mut self,
        scope: Scope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(cursor) = self
            .transfer_inspection
            .cursors
            .get(&scope)
            .copied()
            .filter(|c| c.expanded)
        {
            self.place_transfer_cursor(scope, Cursor::row(cursor.id), window, cx);
            return true;
        }
        false
    }

    fn toggle_transfer_detail(
        &mut self,
        scope: Scope,
        id: TransferId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let expanded = !self
            .transfer_inspection
            .cursors
            .get(&scope)
            .is_some_and(|c| c.id == id && c.expanded);
        self.place_transfer_cursor(
            scope,
            Cursor {
                expanded,
                ..Cursor::row(id)
            },
            window,
            cx,
        );
    }

    fn run_transfer_action(
        &mut self,
        scope: Scope,
        id: TransferId,
        action: RecoveryAction,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self.transfers.get(id) else {
            return;
        };
        if !inspectable(task)
            || scope.is_some_and(|session| session != task.session())
            || !self.transfers.can_transfer(task.session())
        {
            return;
        }
        match action {
            RecoveryAction::Resume if can_resume(task) => self.transfer_retry(id, cx),
            RecoveryAction::Restart => self.transfer_restart(id, cx),
            RecoveryAction::Resume => {}
        }
    }

    pub(super) fn transfer_list_key(
        &mut self,
        scope: Scope,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if modifiers.control
            || modifiers.alt
            || modifiers.platform
            || (modifiers.shift && key != "tab")
        {
            return false;
        }
        if event.is_held && matches!(key, "enter" | "space") {
            return true;
        }
        if key == "escape" {
            return self.close_transfer_detail(scope, window, cx);
        }
        if scope.is_none() && matches!(key, "pageup" | "pagedown" | "home" | "end") {
            scroll_by_key(&self.transfer_inspection.center_scroll, key);
            cx.notify();
            return true;
        }
        if !matches!(key, "up" | "down" | "space" | "enter" | "tab") {
            return false;
        }
        let ids: Vec<_> = self
            .transfers
            .transfers()
            .iter()
            .filter(|t| scope.is_none_or(|s| s == t.session()) && selectable(t))
            .map(Transfer::id)
            .collect();
        let Some(first) = ids.first().copied() else {
            return false;
        };
        let previous = self.transfer_inspection.cursors.get(&scope).copied();
        let mut cursor = previous.unwrap_or_else(|| Cursor::row(first));
        let Some(index) = ids.iter().position(|id| *id == cursor.id) else {
            self.place_transfer_cursor(scope, Cursor::row(first), window, cx);
            return true;
        };
        match key {
            "up" | "down" => {
                let next = if previous.is_none() {
                    0
                } else if key == "up" {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(ids.len() - 1)
                };
                cursor = Cursor::row(ids[next]);
            }
            "enter" | "space" => {
                let active = self
                    .transfers
                    .get(cursor.id)
                    .is_some_and(Transfer::is_active);
                if active && cursor.expanded {
                    self.transfer_cancel(cursor.id, cx);
                } else if let Some(action) = cursor.action {
                    self.run_transfer_action(scope, cursor.id, action, cx);
                } else {
                    self.toggle_transfer_detail(scope, cursor.id, window, cx);
                }
                return true;
            }
            "tab" if cursor.expanded => {
                let task = self
                    .transfers
                    .get(cursor.id)
                    .expect("the cursor is in the list");
                if !inspectable(task) || !self.transfers.can_transfer(task.session()) {
                    return false;
                }
                cursor.action = match (cursor.action, modifiers.shift, can_resume(task)) {
                    (None, false, true) | (Some(RecoveryAction::Restart), true, true) => {
                        Some(RecoveryAction::Resume)
                    }
                    (None, _, _) | (Some(RecoveryAction::Resume), false, _) => {
                        Some(RecoveryAction::Restart)
                    }
                    _ => None,
                };
            }
            _ => return false,
        }
        self.place_transfer_cursor(scope, cursor, window, cx);
        true
    }

    pub(super) fn transfer_cancel_button(
        &self,
        scope: Scope,
        prefix: &'static str,
        index: usize,
        task: &Transfer,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id = task.id();
        let selected = self
            .transfer_inspection
            .cursors
            .get(&scope)
            .filter(|c| c.id == id);
        components::button(
            self.strings().transfers_cancel,
            Variant::Ghost,
            self.metrics(),
        )
        .id(SharedString::from(format!("{prefix}.cancel.{id}")))
        .debug_selector(move || format!("{prefix}.cancel.{index}"))
        .when_some(selected, |button, cursor| {
            button
                .border_1()
                .border_color(theme::swatch(palette::ACCENT))
                .when(cursor.expanded, |b| {
                    b.border_2().bg(theme::swatch(palette::ACCENT_TINT))
                })
                .anchor_scroll(Some(self.transfer_inspection.anchor(scope)))
        })
        .on_click(cx.listener(move |view, _, _, cx| {
            if view
                .transfers
                .get(id)
                .is_some_and(|t| t.is_active() && !t.state().is_awaiting_overwrite())
            {
                view.transfer_cancel(id, cx);
            }
        }))
    }

    pub(super) fn failed_transfer_row(
        &self,
        scope: Scope,
        index: usize,
        task: &Transfer,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let id = task.id();
        let prefix = if scope.is_some() {
            "transfers"
        } else {
            "transfers.center"
        };
        let cursor = self
            .transfer_inspection
            .cursors
            .get(&scope)
            .filter(|c| c.id == id);
        let expanded = cursor.is_some_and(|c| c.expanded);
        let marked_action = cursor.and_then(|c| c.action);
        let header_marked = cursor.is_some() && marked_action.is_none();
        let tone = Tone::for_transfer_state(task.state());
        let name = file_name(task.source()).unwrap_or(task.source());
        let host = self.transfer_host_label(task);
        let state = copy::transfer_state_label(task.state(), self.workspace.locale());
        let reason = task.state().failure().map_or_else(
            || state.to_owned(),
            |failure| format!("{state} · {}", self.transfer_failure_text(failure)),
        );
        let title = div()
            .id(SharedString::from(format!("{prefix}.details.{id}")))
            .debug_selector(move || format!("{prefix}.details.{index}"))
            .flex()
            .flex_1()
            .min_w(metrics.scale(FILES_SELECTION_WORDS_MIN))
            .items_center()
            .gap(metrics.scale(spacing::HAIRLINE))
            .cursor_pointer()
            .tooltip(components::tooltip(strings.transfer_details))
            .child(icons::icon(
                if expanded {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                },
                metrics.small_icon_size(),
                theme::swatch(palette::TEXT_SECONDARY),
            ))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(metrics.label_size())
                    .font_weight(metrics.medium_weight())
                    .debug_selector(move || format!("{prefix}.name.{index}"))
                    .child(SharedString::from(name.to_owned())),
            )
            .on_click(cx.listener(move |view, _, window, cx| {
                view.toggle_transfer_detail(scope, id, window, cx)
            }));
        let mut actions = div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(metrics.scale(spacing::HAIRLINE));
        if self.transfers.can_transfer(task.session()) {
            for action in [RecoveryAction::Resume, RecoveryAction::Restart] {
                if action == RecoveryAction::Resume && !can_resume(task) {
                    continue;
                }
                let (selector, label) = match action {
                    RecoveryAction::Resume => ("resume", strings.transfers_resume),
                    RecoveryAction::Restart
                        if can_resume(task)
                            || matches!(
                                task.state().failure(),
                                Some(TransferFailure::ResumeRefused { .. })
                            ) =>
                    {
                        ("restart", strings.transfers_restart)
                    }
                    RecoveryAction::Restart => ("retry", strings.transfers_restart),
                };
                let marked = marked_action == Some(action);
                let button = components::button_sized(
                    label,
                    if action == RecoveryAction::Restart && can_resume(task) {
                        Variant::Ghost
                    } else {
                        Variant::Secondary
                    },
                    components::ButtonSize::Compact,
                    metrics,
                )
                .id(SharedString::from(format!("{prefix}.{selector}.{id}")))
                .debug_selector(move || format!("{prefix}.{selector}.{index}"))
                .when(marked, |b| {
                    b.border_2()
                        .border_color(theme::swatch(palette::ACCENT))
                        .anchor_scroll(Some(self.transfer_inspection.anchor(scope)))
                })
                .on_click(cx.listener(move |view, _, window, cx| {
                    let expanded = view
                        .transfer_inspection
                        .cursors
                        .get(&scope)
                        .is_some_and(|c| c.id == id && c.expanded);
                    view.transfer_inspection.cursors.insert(
                        scope,
                        Cursor {
                            expanded,
                            ..Cursor::row(id)
                        },
                    );
                    window.focus(view.transfer_list_focus(scope));
                    view.run_transfer_action(scope, id, action, cx)
                }));
                actions = actions.child(button);
            }
        }
        if scope.is_some() {
            actions = actions.child(self.row_icon_button(
                SharedString::from(format!("transfers.remove.{index}")),
                Icon::Close,
                strings.transfers_remove,
                false,
                cx,
                move |view, cx| {
                    if view.transfers.remove_finished(id) {
                        view.sync_transfer_section();
                        cx.notify();
                    }
                },
            ));
        }
        let mut row = div()
            .id(SharedString::from(format!("{prefix}.failed.{id}")))
            .debug_selector(move || format!("{prefix}.row.{index}"))
            .flex()
            .flex_col()
            .flex_none()
            .min_w_0()
            .gap(metrics.scale(spacing::HAIRLINE))
            .px(metrics.scale(spacing::TIGHT))
            .py(metrics.scale(spacing::HAIRLINE))
            .rounded(px(radius::MEDIUM))
            .border_1()
            .border_color(if header_marked {
                theme::swatch(palette::ACCENT)
            } else {
                theme::clear()
            })
            .when(header_marked && !expanded, |r| {
                r.anchor_scroll(Some(self.transfer_inspection.anchor(scope)))
            })
            .text_size(metrics.caption_size())
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .min_w_0()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(title)
                    .when(task.protocol() != TransferProtocol::Sftp, |header| {
                        header.child(
                            components::tag(task.protocol().label(), metrics)
                                .debug_selector(move || format!("{prefix}.protocol.{index}")),
                        )
                    })
                    .child(actions),
            )
            .child(
                div()
                    .min_w_0()
                    .whitespace_normal()
                    .text_color(tone.text())
                    .when(!expanded, |summary| summary.line_clamp(2))
                    .debug_selector(move || format!("{prefix}.state.{index}"))
                    .child(SharedString::from(reason)),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .debug_selector(move || format!("{prefix}.summary.{index}"))
                    .child(SharedString::from(format!(
                        "{} · {host}",
                        self.direction_label(task.direction())
                    ))),
            );
        if !self.transfers.can_transfer(task.session()) {
            row = row.child(
                div()
                    .whitespace_normal()
                    .debug_selector(move || format!("{prefix}.offline.{index}"))
                    .child(strings.transfers_problem_no_connection),
            );
        }
        if expanded {
            let detail = |label: &str, value: &str| {
                div()
                    .min_w_0()
                    .whitespace_normal()
                    .child(SharedString::from(format!("{label}  {value}")))
            };
            let mut paths = div()
                .id(SharedString::from(format!("{prefix}.paths.{id}")))
                .debug_selector(move || format!("{prefix}.paths.{index}"))
                .flex()
                .when(header_marked, |r| {
                    r.anchor_scroll(Some(self.transfer_inspection.anchor(scope)))
                })
                .flex_col()
                .min_w_0()
                .py(metrics.scale(spacing::HAIRLINE))
                .gap(metrics.scale(spacing::HAIRLINE))
                .child(detail(strings.transfer_source, task.source()))
                .child(detail(strings.transfer_destination, task.destination()))
                .child(detail(strings.transfers_host, &host));
            if let Some(path) = task
                .state()
                .residue()
                .or_else(|| task.resume_hint().map(|h| h.partial.as_str()))
            {
                paths = paths.child(detail(strings.transfers_residue, path));
            }
            if let Some(hint) = task.resume_hint() {
                paths = paths.child(detail(
                    strings.transfer_checkpoint,
                    &format!("{} B", hint.offset),
                ));
            }
            row = row.child(paths);
        }
        row
    }

    #[cfg(feature = "ui-capture")]
    pub fn capture_failed_transfer_details(
        &mut self,
        id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(task) = self
            .transfers
            .for_session(id)
            .find(|task| inspectable(task))
            .map(Transfer::id)
        else {
            return false;
        };
        if !self
            .transfer_inspection
            .cursors
            .get(&Some(id))
            .is_some_and(|c| c.id == task && c.expanded)
        {
            self.toggle_transfer_detail(Some(id), task, window, cx);
        }
        true
    }
}
