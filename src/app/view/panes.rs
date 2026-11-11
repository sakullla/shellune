//! Nested panes, tab placement and the saved layout (R4).
//!
//! A leaf names a session or a target that has not been started. Restoring a
//! snapshot installs pending leaves only: no shell, connection, ProxyCommand or
//! forward is started here. Input is given to the active pane. Closing one leaf
//! leaves the others in place.

use gpui::prelude::*;
use gpui::{
    Context, DispatchPhase, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Point,
    SharedString, Window, div, px,
};
use shellune_core::{
    HostConfig, HostId, LocalShell, PaneId, PaneTarget, PanelSection, SessionId, SessionKind,
    SessionState, SplitAxis, TabId, TerminalMode, WorkspaceSnapshot,
};
use shellune_storage::WorkspaceStore;

use crate::app::components::Variant;
use crate::app::icons::Icon;
use crate::app::layout::{Direction, LayoutNotice, Node};
use crate::app::nav::Destination;
use crate::app::theme::{self, Tone, palette, radius, spacing};
use crate::app::view::{ShelluneView, SplitDrag};

impl ShelluneView {
    /// Reads `workspace.json` into pending panes.
    ///
    /// A missing file is a first run and leaves the page the caller selected.
    /// A damaged file is reported and left unchanged. Nothing in the snapshot is
    /// started.
    pub(super) fn restore_workspace_layout(&mut self) {
        let store = WorkspaceStore::at(self.store.root());
        match store.load() {
            Err(error) => {
                self.layout_persist = false;
                self.layout_error = Some(error.to_string());
            }
            Ok(snapshot) => {
                self.layout_persist = true;
                if snapshot_is_blank(&snapshot) {
                    return;
                }
                let ids: Vec<HostId> = self.book.hosts().iter().map(HostConfig::id).collect();
                let restored = snapshot.restore(|id| ids.contains(&id));
                let section = restored.panel().section();
                let visible = restored.panel().visible();
                self.workspace.install_restored(restored);
                if visible {
                    match section {
                        PanelSection::Hosts => {
                            self.workspace.select(Destination::Hosts);
                        }
                        PanelSection::Transfers => {
                            self.workspace.select(Destination::Transfers);
                        }
                        PanelSection::Forwards => {
                            self.workspace.select(Destination::Forwards);
                            self.workspace.set_context(PanelSection::Forwards);
                        }
                        PanelSection::Files | PanelSection::None => {}
                    }
                }
                if self.workspace.layout().has_tabs() {
                    self.session_terminal = true;
                    self.active = self.workspace.layout().active_session();
                }
            }
        }
    }

    /// Writes the layout when a user edit is allowed to.
    ///
    /// Window shutdown turns persistence off first, so closing the window cannot
    /// replace the saved tree with an empty one. The snapshot has no secrets,
    /// sockets or process handles.
    pub(super) fn checkpoint_layout(&mut self) {
        if !self.layout_persist {
            return;
        }
        let Ok(snapshot) = self.workspace.capture_snapshot() else {
            return;
        };
        if let Err(error) = WorkspaceStore::at(self.store.root()).save(&snapshot) {
            self.layout_error = Some(error.to_string());
        }
    }

    /// Points the screen at the pane the layout says is active.
    pub(super) fn sync_layout_focus(&mut self, cx: &mut Context<Self>) {
        self.active = self.workspace.layout().active_session();
        if let Some(id) = self.active {
            self.focus_pane(id, cx);
        }
        cx.notify();
    }

    /// Shows the tab the layout just selected.
    pub(super) fn show_layout_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.workspace.layout().active_session() {
            if self.pane_for(id).is_some() {
                self.activate_in(id, window, cx);
                return;
            }
            if self.attempt_index(id).is_some() {
                self.focus_attempt(id, cx);
                return;
            }
            self.active = Some(id);
            self.session_terminal = true;
            self.showing_connection_form = false;
            cx.notify();
            return;
        }
        self.active = None;
        self.session_terminal = true;
        self.showing_connection_form = false;
        if self.workspace.destination() != Destination::LocalShells {
            self.workspace.select(Destination::LocalShells);
        }
        let handle = self.destination_focus(self.workspace.destination()).clone();
        window.focus(&handle);
        cx.notify();
    }

    /// A tab is a cycle stop when it has a terminal or a pane that is still waiting.
    ///
    /// A session that is only connecting has no terminal yet, so the cycle steps
    /// over it instead of landing on an empty form.
    pub(super) fn layout_tab_is_shown(&self, index: usize) -> bool {
        let Some(tab) = self.workspace.layout().tabs().get(index) else {
            return false;
        };
        if node_has_pending(tab.root()) {
            return true;
        }
        sessions_in_node(tab.root())
            .iter()
            .any(|id| self.pane_for(*id).is_some())
    }

    /// Splits the active pane. A local split starts the same shell. A remote split
    /// waits: it does not dial, so it cannot run a ProxyCommand.
    pub(super) fn split_active(
        &mut self,
        axis: SplitAxis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.workspace.layout().active_pane() else {
            return;
        };
        let Some(leaf) = self
            .workspace
            .layout()
            .leaves()
            .into_iter()
            .find(|leaf| leaf.id() == pane)
            .cloned()
        else {
            return;
        };
        if let Some(id) = leaf.session() {
            let Some(session) = self.workspace.session(id) else {
                return;
            };
            match session.kind().clone() {
                SessionKind::Local { shell } => {
                    let Some(shell) = self.shell_for_split(shell) else {
                        return;
                    };
                    if !self.workspace.arm_split(axis) {
                        return;
                    }
                    self.start_shell(shell, Some(window), cx);
                }
                SessionKind::Remote { host } => {
                    self.workspace
                        .layout_mut()
                        .split_pending(axis, PaneTarget::remote(host));
                    self.present_active_pane(window, cx);
                    self.checkpoint_layout();
                }
            }
            return;
        }
        let target = leaf.target().clone();
        self.workspace.layout_mut().split_pending(axis, target);
        self.present_active_pane(window, cx);
        self.checkpoint_layout();
    }

    fn shell_for_split(&self, program: Option<String>) -> Option<LocalShell> {
        if let Some(program) = program.filter(|program| !program.trim().is_empty()) {
            let name = self
                .workspace
                .shells()
                .iter()
                .find(|shell| shell.program() == program)
                .map(|shell| shell.name().to_owned())
                .unwrap_or_else(|| program.clone());
            return Some(LocalShell::new(name, program));
        }
        self.workspace.selected_shell().cloned()
    }

    /// Moves keyboard focus to the neighbour and gives that pane the keys.
    pub(super) fn focus_direction(
        &mut self,
        direction: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.layout_mut().focus_neighbor(direction) {
            self.present_active_pane(window, cx);
        }
    }

    /// Grows or shrinks only the split that directly holds the active pane.
    pub(super) fn resize_direction(
        &mut self,
        direction: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.layout_mut().resize_active(direction, 500) {
            self.present_active_pane(window, cx);
            self.checkpoint_layout();
        }
    }

    /// Swaps the active pane with its neighbour. The keys follow the pane.
    pub(super) fn move_direction(
        &mut self,
        direction: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.layout_mut().move_active(direction) {
            self.present_active_pane(window, cx);
            self.checkpoint_layout();
        }
    }

    pub(super) fn toggle_active_zoom(&mut self, cx: &mut Context<Self>) {
        if self.workspace.layout_mut().toggle_zoom() {
            cx.notify();
        }
    }

    /// Moves the active tab by one place and remembers the order.
    pub(super) fn reorder_active_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.workspace.layout_mut().reorder_active(delta) {
            self.sync_layout_focus(cx);
            self.checkpoint_layout();
        }
    }

    /// Moves the tab that holds `id`.
    pub(super) fn reorder_tab(&mut self, id: SessionId, delta: isize, cx: &mut Context<Self>) {
        if self.workspace.layout_mut().focus_session(id) {
            self.reorder_active_tab(delta, cx);
        }
    }

    fn present_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.workspace.layout().active_session();
        self.active = session;
        if self.workspace.layout().has_tabs() {
            self.session_terminal = true;
        }
        if let Some(id) = session
            && self.pane_for(id).is_some()
        {
            self.showing_connection_form = false;
            // Moving the keyboard to a pane is looking at that pane's tab, so whatever
            // the tab was marked for has been seen — also when the tab was already on
            // screen and only the focused pane changed (R7).
            self.clear_viewed_attention(cx);
            let handle = self.pane_for(id).unwrap().read(cx).focus_handle().clone();
            window.focus(&handle);
            cx.notify();
            return;
        }
        if let Some(id) = session
            && self.attempt_index(id).is_some()
        {
            self.focus_attempt(id, cx);
            return;
        }
        self.showing_connection_form = false;
        self.clear_viewed_attention(cx);
        let handle = self.destination_focus(self.workspace.destination()).clone();
        window.focus(&handle);
        cx.notify();
    }

    /// Closes the whole tab that holds `id`, including panes that have no session yet.
    pub(super) fn request_close_tab_anchor(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(index) = self.tab_index_of(id) else {
            self.request_close_session(id, cx);
            return;
        };
        self.close_tab_indexes(cx, |candidate| candidate == index);
    }

    /// Closes every tab except the one that holds `id`.
    pub(super) fn close_other_tabs(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let keep = self.tab_index_of(id);
        self.close_tab_indexes(cx, |index| keep != Some(index));
    }

    /// Closes tabs to the right of the one that holds `id`.
    pub(super) fn close_tabs_after(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let keep = self.tab_index_of(id);
        self.close_tab_indexes(cx, |index| keep.is_some_and(|keep| index > keep));
    }

    fn tab_index_of(&self, id: SessionId) -> Option<usize> {
        self.workspace
            .layout()
            .tabs()
            .iter()
            .position(|tab| sessions_in_node(tab.root()).contains(&id))
    }

    fn close_tab_indexes(&mut self, cx: &mut Context<Self>, drop_index: impl Fn(usize) -> bool) {
        let tabs: Vec<(TabId, Vec<SessionId>)> = self
            .workspace
            .layout()
            .tabs()
            .iter()
            .enumerate()
            .filter(|(index, _)| drop_index(*index))
            .map(|(_, tab)| (tab.id(), sessions_in_node(tab.root())))
            .collect();
        let mut sessions = Vec::new();
        for (tab, in_tab) in tabs {
            // A transfer or a forward still has to be confirmed, so that session's
            // tab stays until the user answers. Waiting panes on the named tab do
            // not stay with it.
            let busy = in_tab.iter().any(|id| self.close_needs_confirmation(*id));
            if busy {
                self.workspace.layout_mut().drop_pending_on_tab(tab);
                sessions.extend(in_tab);
            } else {
                sessions.extend(self.workspace.layout_mut().take_tab(tab));
            }
        }
        if sessions.is_empty() {
            self.sync_layout_focus(cx);
            self.checkpoint_layout();
            cx.notify();
            return;
        }
        self.sync_layout_focus(cx);
        self.checkpoint_layout();
        self.close_sessions(sessions, cx);
    }

    /// The active tab, or the pending layout, drawn as a tree.
    pub(super) fn layout_terminal(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let metrics = self.metrics();
        let mut column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .bg(theme::swatch(palette::CANVAS))
            .relative();
        if let Some(reason) = &self.local_error {
            column = column.child(
                div()
                    .flex_none()
                    .px(metrics.scale(spacing::SNUG))
                    .pt(metrics.scale(spacing::TIGHT))
                    .child(self.notice_line(reason.clone(), Tone::Danger)),
            );
        }
        column = column.children(self.layout_status_banners());
        let session_id = if self.workspace.layout().active_tab().is_some() {
            self.workspace.layout().active_session()
        } else {
            self.active
        };
        if session_id.is_some_and(|id| self.workspace.session(id).is_some()) {
            column = column.child(self.session_bar(session_id, window, cx));
            if let Some(notice) = self.active_failure_notice(session_id, cx) {
                column = column.child(
                    div()
                        .flex_none()
                        .px(metrics.scale(spacing::SNUG))
                        .pt(metrics.scale(spacing::SNUG))
                        .child(notice),
                );
            }
        }
        let files = session_id.filter(|id| self.files.is_open(*id));
        if let Some(id) = files {
            column = column.child(self.files_view(id, window, cx));
        } else if let Some(tab) = self.workspace.layout().active_tab() {
            let zoomed = tab.zoomed();
            let tree = if let Some(zoomed) = zoomed {
                self.zoomed_leaf(tab.root(), zoomed, cx)
            } else {
                self.layout_node(tab.root(), true, cx)
            };
            column = column.child(tree);
        }
        if self.split_drag.is_some() {
            column = column.child(self.split_drag_canvas(cx));
        }
        column
    }

    fn active_failure_notice(
        &self,
        id: Option<SessionId>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let id = id?;
        let session = self.workspace.session(id)?;
        match session.state() {
            SessionState::Disconnected(failure) | SessionState::Failed(failure) => {
                Some(self.failure_notice(failure, crate::app::view::may_reconnect(session)))
            }
            _ => self.pane_for(id).and_then(|pane| {
                pane.read(cx)
                    .state()
                    .failure()
                    .map(|failure| self.failure_notice(&failure, false))
            }),
        }
    }

    fn layout_status_banners(&self) -> Vec<gpui::Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let mut banners = Vec::new();
        if let Some(reason) = &self.layout_error {
            banners.push(
                components_banner(Tone::Danger, metrics).child(SharedString::from(reason.clone())),
            );
        }
        for notice in self.workspace.layout().notices() {
            let text = match notice {
                LayoutNotice::DanglingTab { .. } => strings.workspace_dangling_tab,
                LayoutNotice::DanglingPane { .. } => strings.workspace_dangling_pane,
            };
            banners.push(
                components_banner(Tone::Warning, metrics)
                    .debug_selector(|| "workspace.notice".to_owned())
                    .child(SharedString::from(text.to_owned())),
            );
        }
        banners
    }

    fn zoomed_leaf(&self, root: &Node, pane: PaneId, cx: &mut Context<Self>) -> gpui::Div {
        match find_node(root, pane) {
            Some(node) => self.layout_node(node, true, cx),
            None => self.layout_node(root, true, cx),
        }
    }

    fn layout_node(&self, node: &Node, active_tree: bool, cx: &mut Context<Self>) -> gpui::Div {
        match node {
            Node::Leaf(leaf) => self.leaf_view(leaf, active_tree, cx),
            Node::Split {
                id,
                axis,
                ratio,
                first,
                second,
            } => self.split_view(*id, *axis, *ratio, first, second, cx),
        }
    }

    fn split_view(
        &self,
        id: u64,
        axis: SplitAxis,
        ratio: u16,
        first: &Node,
        second: &Node,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let metrics = self.metrics();
        let fraction = f32::from(ratio) / 10_000.0;
        let across = matches!(axis, SplitAxis::LeftRight);
        let mut row = div().flex().flex_1().min_w(px(0.0)).min_h(px(0.0));
        row = if across { row } else { row.flex_col() };
        row.child(self.sized_child(self.layout_node(first, true, cx), fraction, across))
            .child(self.split_divider(id, axis, cx, metrics))
            .child(self.sized_child(self.layout_node(second, true, cx), 1.0 - fraction, across))
    }

    fn sized_child(&self, child: gpui::Div, fraction: f32, across: bool) -> gpui::Div {
        let fraction = fraction.clamp(0.01, 0.99);
        let child = child
            .flex_basis(gpui::relative(fraction))
            .flex_shrink()
            .min_w(px(0.0))
            .min_h(px(0.0));
        if across {
            child.min_w(px(0.0))
        } else {
            child.min_h(px(0.0))
        }
    }

    fn split_divider(
        &self,
        id: u64,
        axis: SplitAxis,
        cx: &mut Context<Self>,
        metrics: crate::app::theme::Metrics,
    ) -> gpui::Stateful<gpui::Div> {
        let across = matches!(axis, SplitAxis::LeftRight);
        let strings = self.strings();
        let grow = if across {
            Direction::Right
        } else {
            Direction::Down
        };
        let shrink = if across {
            Direction::Left
        } else {
            Direction::Up
        };
        // The band keeps its 12 thickness: it is a continuous drag strip, not a
        // button, and a wider strip would take the space of the panes on both sides.
        // The two nudge buttons it carries are pointer targets in their own right and
        // are drawn over the neighbouring pane's edge instead (R4).
        div()
            .id(SharedString::from(format!("pane.divider.{id}")))
            // The id is per split; the debug name stays one string so a test can
            // measure the band without knowing which split it belongs to.
            .debug_selector(|| "pane.divider".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(metrics.scale(1.0))
            .when(across, |bar| {
                bar.w(metrics.scale(12.0))
                    .h_full()
                    .flex_col()
                    .cursor_col_resize()
            })
            .when(!across, |bar| {
                bar.h(metrics.scale(12.0)).w_full().cursor_row_resize()
            })
            .bg(theme::swatch(palette::SURFACE))
            .hover(|bar| bar.bg(theme::swatch(palette::SURFACE_HOVER)))
            .border_color(theme::swatch(palette::BORDER))
            .when(across, |bar| bar.border_l_1().border_r_1())
            .when(!across, |bar| bar.border_t_1().border_b_1())
            .child(self.resize_button(id, shrink, strings.pane_resize_less, "less", cx))
            .child(self.resize_button(id, grow, strings.pane_resize_more, "more", cx))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    this.begin_split_drag(id, axis, event.position, cx);
                }),
            )
    }

    fn resize_button(
        &self,
        split: u64,
        direction: Direction,
        label: &'static str,
        name: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let metrics = self.metrics();
        let selector = format!("pane.resize.{name}");
        // The chevron points the way the divider moves.
        let icon = match direction {
            Direction::Left => Icon::ChevronLeft,
            Direction::Right => Icon::ChevronRight,
            Direction::Up => Icon::ChevronUp,
            Direction::Down => Icon::ChevronDown,
        };
        // The whole band stays 12 thick, so this target is wider than the band it sits
        // in and is drawn over the neighbouring pane's edge instead of being shrunk:
        // the size comes from `icon_only_button` (`Metrics::hit_target()`), never from a
        // local override, because 24×24 is what the pointer can hit (R4). The band left
        // above and below the two buttons stays draggable.
        crate::app::components::icon_only_button(icon, metrics)
            .id(SharedString::from(format!("pane.resize.{split}.{name}")))
            .debug_selector(move || selector.clone())
            .tooltip(crate::app::components::tooltip(label))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                cx.stop_propagation();
                this.nudge_split(split, direction, cx);
            }))
    }

    fn nudge_split(&mut self, split: u64, direction: Direction, cx: &mut Context<Self>) {
        let Some(ratio) = self
            .workspace
            .layout()
            .active_tab()
            .and_then(|tab| split_ratio(tab.root(), split))
        else {
            return;
        };
        let next = match direction {
            Direction::Right | Direction::Down => ratio.saturating_add(500),
            Direction::Left | Direction::Up => ratio.saturating_sub(500),
        };
        let next = next.clamp(1, 9999);
        if self.workspace.layout_mut().set_split_ratio(split, next) {
            self.checkpoint_layout();
            cx.notify();
        }
    }

    fn begin_split_drag(
        &mut self,
        split: u64,
        axis: SplitAxis,
        origin: Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let ratio = self
            .workspace
            .layout()
            .active_tab()
            .and_then(|tab| split_ratio(tab.root(), split))
            .unwrap_or(5000);
        self.split_drag = Some(SplitDrag {
            split,
            axis,
            origin,
            origin_ratio: ratio,
        });
        cx.notify();
    }

    fn split_drag_canvas(
        &self,
        cx: &mut Context<Self>,
    ) -> gpui::Canvas<gpui::Bounds<gpui::Pixels>> {
        let view = cx.entity().downgrade();
        gpui::canvas(
            |bounds, _window, _app| bounds,
            move |_bounds, _prepaint, window, _app| {
                let view_move = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, app| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let _ = view_move.update(app, |this, cx| {
                        this.drag_split(event.position, cx);
                    });
                });
                let view_up = view.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, app| {
                    if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                        return;
                    }
                    let _ = view_up.update(app, |this, cx| {
                        if this.split_drag.take().is_some() {
                            this.checkpoint_layout();
                            cx.notify();
                        }
                    });
                });
            },
        )
    }

    fn drag_split(&mut self, position: Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.split_drag else {
            return;
        };
        let delta = match drag.axis {
            SplitAxis::LeftRight => f32::from(position.x) - f32::from(drag.origin.x),
            SplitAxis::TopBottom => f32::from(position.y) - f32::from(drag.origin.y),
        };
        let next = (i32::from(drag.origin_ratio) + (delta * 20.0) as i32).clamp(1, 9999) as u16;
        if self
            .workspace
            .layout_mut()
            .set_split_ratio(drag.split, next)
        {
            cx.notify();
        }
    }

    fn leaf_view(
        &self,
        leaf: &crate::app::layout::Leaf,
        _active_tree: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let pane = leaf.id();
        let active = self.workspace.layout().active_pane() == Some(pane);
        let session = leaf.session();
        // A tab with one pane needs no pane chrome: the session bar already names
        // it and carries the split buttons, and a focus frame around the only pane
        // marks nothing.
        let solo = self.active_tab_is_solo() && !leaf.is_pending();
        let mut frame = div()
            .id(SharedString::from(format!("pane.leaf.{pane}")))
            .flex()
            .flex_col()
            .flex_1()
            .size_full()
            .overflow_hidden()
            .when(!solo, |frame| {
                frame.border_1().border_color(theme::swatch(if active {
                    palette::ACCENT
                } else {
                    palette::BORDER
                }))
            })
            .when(active, |frame| {
                frame.debug_selector(|| "pane.active".to_owned())
            })
            .when(!active, |frame| {
                frame.debug_selector(|| "pane.idle".to_owned())
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event, window, cx| {
                    this.focus_leaf(pane, window, cx);
                }),
            );
        if !solo {
            frame = frame.child(self.leaf_header(leaf, active, cx));
        }
        if leaf.is_pending() {
            frame = frame.child(self.pending_card(leaf, cx));
        } else if let Some(id) = session
            && let Some(terminal) = self.pane_for(id)
        {
            frame = frame.child(terminal.clone());
        } else if let Some(id) = session {
            let text = self
                .workspace
                .session(id)
                .map(|session| {
                    format!(
                        "{} · {}",
                        strings.session_no_terminal,
                        crate::app::copy::session_state_label(
                            session.state(),
                            self.workspace.locale()
                        )
                    )
                })
                .unwrap_or_else(|| strings.session_no_terminal.to_owned());
            frame = frame.child(
                div()
                    .p(metrics.scale(spacing::SNUG))
                    .child(self.notice_line(text, Tone::Warning)),
            );
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .child(frame)
    }

    fn leaf_header(
        &self,
        leaf: &crate::app::layout::Leaf,
        active: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let pane = leaf.id();
        let title = leaf
            .session()
            .and_then(|id| {
                self.workspace
                    .session(id)
                    .map(|session| session.title().to_owned())
            })
            .unwrap_or_else(|| {
                self.workspace
                    .layout()
                    .active_tab()
                    .map(|tab| tab.title().to_owned())
                    .unwrap_or_else(|| strings.pane_pending.to_owned())
            });
        let zoomed = self
            .workspace
            .layout()
            .active_tab()
            .is_some_and(|tab| tab.zoomed() == Some(pane));
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(metrics.scale(spacing::HAIRLINE))
            .h(metrics.scale(28.0))
            .px(metrics.scale(spacing::TIGHT))
            .bg(theme::swatch(palette::SURFACE))
            .border_b_1()
            .border_color(theme::swatch(palette::BORDER))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_size(metrics.caption_size())
                    .child(SharedString::from(title)),
            )
            .when(active, |row| {
                row.child(crate::app::components::chip(
                    strings.pane_active,
                    Tone::Accent,
                    metrics,
                ))
            })
            .when(active, |row| {
                row.child(self.pane_icon(
                    pane,
                    "split.right",
                    Icon::SplitRight,
                    strings.pane_split_right,
                    cx,
                    |this, window, cx| this.split_active(SplitAxis::LeftRight, window, cx),
                ))
                .child(self.pane_icon(
                    pane,
                    "split.down",
                    Icon::SplitDown,
                    strings.pane_split_down,
                    cx,
                    |this, window, cx| this.split_active(SplitAxis::TopBottom, window, cx),
                ))
                .child(self.pane_icon(
                    pane,
                    "zoom",
                    Icon::Zoom,
                    if zoomed {
                        strings.pane_unzoom
                    } else {
                        strings.pane_zoom
                    },
                    cx,
                    |this, _window, cx| this.toggle_active_zoom(cx),
                ))
            })
            .child(self.pane_icon(
                pane,
                "close",
                Icon::Close,
                strings.pane_close,
                cx,
                move |this, _window, cx| this.close_one_pane(pane, cx),
            ))
    }

    /// Whether the active tab holds exactly one pane and shows it unzoomed.
    pub(super) fn active_tab_is_solo(&self) -> bool {
        self.workspace
            .layout()
            .active_tab()
            .is_some_and(|tab| matches!(tab.root(), Node::Leaf(_)) && tab.zoomed().is_none())
    }

    /// The split buttons, for the session bar of a tab with a single pane.
    pub(super) fn solo_split_buttons(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if !self.active_tab_is_solo() {
            return None;
        }
        let pane = self.workspace.layout().active_pane()?;
        let strings = self.strings();
        let control = self.metrics().hit_target();
        Some(
            div()
                .flex()
                .items_center()
                .gap(self.metrics().scale(spacing::HAIRLINE))
                .child(
                    self.pane_icon(
                        pane,
                        "split.right",
                        Icon::SplitRight,
                        strings.pane_split_right,
                        cx,
                        |this, window, cx| this.split_active(SplitAxis::LeftRight, window, cx),
                    )
                    .size(control),
                )
                .child(
                    self.pane_icon(
                        pane,
                        "split.down",
                        Icon::SplitDown,
                        strings.pane_split_down,
                        cx,
                        |this, window, cx| this.split_active(SplitAxis::TopBottom, window, cx),
                    )
                    .size(control),
                ),
        )
    }

    fn pane_icon(
        &self,
        pane: PaneId,
        name: &'static str,
        icon: Icon,
        label: &'static str,
        cx: &mut Context<Self>,
        click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let metrics = self.metrics();
        let selector = format!("pane.{name}");
        crate::app::components::icon_only_button(icon, metrics)
            // The pane header is 28px tall; the button keeps the 24x24 minimum so a
            // reduced text size cannot shrink the target (R4).
            .w(metrics.hit_target())
            .h(metrics.hit_target())
            .id(SharedString::from(format!("pane.{name}.{pane}")))
            .debug_selector(move || selector.clone())
            .tooltip(crate::app::components::tooltip(label))
            .on_click(cx.listener(move |this, _event, window, cx| {
                cx.stop_propagation();
                click(this, window, cx);
            }))
    }

    fn pending_card(&self, leaf: &crate::app::layout::Leaf, cx: &mut Context<Self>) -> gpui::Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let pane = leaf.id();
        let missing = leaf.mark().is_some();
        let mode = match leaf.mode() {
            TerminalMode::Classic => strings.pane_mode_classic,
            TerminalMode::Integrated => strings.pane_mode_integrated,
        };
        let mut card = div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(metrics.scale(spacing::TIGHT))
            .p(metrics.scale(spacing::SNUG))
            .debug_selector(|| "pane.pending".to_owned())
            .child(
                div()
                    .text_size(metrics.body_size())
                    .child(strings.pane_pending),
            )
            .child(
                div()
                    .max_w(metrics.scale(420.0))
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .text_center()
                    .child(strings.pane_pending_body),
            )
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(mode),
            );
        if missing {
            card = card
                .debug_selector(|| "pane.missing".to_owned())
                .child(self.notice_line(
                    format!(
                        "{} {}",
                        strings.pane_missing_host, strings.pane_missing_next
                    ),
                    Tone::Warning,
                ));
        }
        card.child(
            div()
                .flex()
                .gap(metrics.scale(spacing::TIGHT))
                .when(!missing, |row| {
                    // The click hands over its window, so a local shell that starts
                    // here takes the keyboard at once, as one started anywhere else
                    // does; a remote pane only opens its form.
                    row.child(
                        crate::app::components::compact_button(
                            strings.pane_connect,
                            Variant::Primary,
                            metrics,
                        )
                        .id(SharedString::from(format!("pane.connect.{pane}")))
                        .debug_selector(move || format!("pane.connect.{pane}"))
                        .on_click(cx.listener(
                            move |this, _event, window, cx| {
                                cx.stop_propagation();
                                this.connect_leaf(pane, Some(window), cx);
                            },
                        )),
                    )
                })
                .when(!missing && self.waiting_local_panes().len() > 1, |row| {
                    row.child(
                        crate::app::components::compact_button(
                            strings.pane_connect_all,
                            Variant::Secondary,
                            metrics,
                        )
                        .id(SharedString::from(format!("pane.connect_all.{pane}")))
                        .debug_selector(|| "pane.connect_all".to_owned())
                        .on_click(cx.listener(
                            |this, _event, window, cx| {
                                cx.stop_propagation();
                                this.connect_waiting_local_panes(window, cx);
                            },
                        )),
                    )
                })
                .child(self.text_button(
                    SharedString::from(format!("pane.remove.{pane}")),
                    strings.pane_remove,
                    Variant::Danger,
                    cx,
                    move |this, cx| this.close_one_pane(pane, cx),
                )),
        )
    }

    /// A tab that is only waiting, drawn in the same strip as live sessions.
    pub(super) fn pending_tab(
        &self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let tab = self.workspace.layout().tabs().get(index);
        let title = tab
            .map(|tab| tab.display_title().to_owned())
            .unwrap_or_else(|| strings.pane_pending.to_owned());
        let accent = tab.and_then(|tab| tab.accent());
        let selected = self
            .workspace
            .layout()
            .tabs()
            .get(index)
            .is_some_and(|tab| Some(tab.id()) == self.workspace.layout().active_tab_id())
            && self.workspace.destination() == Destination::LocalShells;
        let selector = format!("session.tab.{index}");
        let mut tab = div()
            .id(("session.tab", index))
            .debug_selector(move || selector.clone())
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .h(metrics.tab_height())
            .px(metrics.scale(spacing::SNUG))
            .rounded(px(radius::MEDIUM))
            .border_1()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _event, window, cx| {
                this.select_tab(index, window, cx);
            }));
        tab = if selected {
            tab.bg(theme::swatch(palette::CANVAS))
                .border_color(theme::swatch(palette::BORDER_STRONG))
        } else {
            tab.border_color(theme::clear())
        };
        tab.children(accent.map(|accent| {
            crate::app::view::tab_accent_mark(
                accent,
                metrics,
                format!("session.tab.accent.{index}"),
            )
        }))
        .child(div().truncate().child(SharedString::from(format!(
            "{title} · {}",
            strings.pane_pending
        ))))
        .child(
            crate::app::components::icon_only_button(Icon::Close, metrics)
                .w(metrics.hit_target())
                .h(metrics.hit_target())
                .id(("session.tab.close", index))
                .debug_selector({
                    let selector = format!("session.tab.close.{index}");
                    move || selector.clone()
                })
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    cx.stop_propagation();
                    this.close_pending_tab(index, cx);
                })),
        )
    }

    fn close_pending_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        self.close_tab_indexes(cx, |candidate| candidate == index);
    }

    fn focus_leaf(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.layout_mut().focus_pane(pane) {
            self.present_active_pane(window, cx);
        }
    }

    fn close_one_pane(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        let session = self
            .workspace
            .layout()
            .leaves()
            .into_iter()
            .find(|leaf| leaf.id() == pane)
            .and_then(|leaf| leaf.session());
        if let Some(id) = session {
            self.request_close_session(id, cx);
            return;
        }
        if self.workspace.layout_mut().close_pane(pane) {
            self.sync_layout_focus(cx);
            self.checkpoint_layout();
        }
    }

    /// Starts the pane the user pointed at. A missing host is not dialed.
    pub(super) fn connect_leaf(
        &mut self,
        pane: PaneId,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let Some(leaf) = self
            .workspace
            .layout()
            .leaves()
            .into_iter()
            .find(|leaf| leaf.id() == pane)
            .cloned()
        else {
            return;
        };
        if leaf.mark().is_some() {
            return;
        }
        match leaf.target().clone() {
            PaneTarget::Local { shell } => {
                let Some(shell) = self.shell_for_split(shell) else {
                    return;
                };
                self.workspace.arm_adopt(pane);
                self.start_shell(shell, window, cx);
            }
            PaneTarget::Remote { host } => {
                let Some(host) = self.book.host(host).cloned() else {
                    cx.notify();
                    return;
                };
                self.park_focused_form();
                self.workspace.arm_adopt(pane);
                self.open_remote_after_park(&host, cx);
            }
        }
    }

    /// The waiting panes of the active tab that a click can start on its own: local
    /// shells whose target is still known. A remote pane asks for its sign-in first.
    fn waiting_local_panes(&self) -> Vec<PaneId> {
        let Some(tab) = self.workspace.layout().active_tab() else {
            return Vec::new();
        };
        let mut leaves = Vec::new();
        collect_leaves(tab.root(), &mut leaves);
        leaves
            .into_iter()
            .filter(|leaf| {
                leaf.is_pending()
                    && leaf.mark().is_none()
                    && matches!(leaf.target(), PaneTarget::Local { .. })
            })
            .map(|leaf| leaf.id())
            .collect()
    }

    /// Starts every waiting local pane of the active tab, one after another (a
    /// local start finishes before it returns), and leaves the keyboard in the
    /// active pane, or in the last one started.
    fn connect_waiting_local_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut panes = self.waiting_local_panes();
        let active = self.workspace.layout().active_pane();
        if let Some(index) = active.and_then(|active| panes.iter().position(|p| *p == active)) {
            let active = panes.remove(index);
            panes.push(active);
        }
        let last = panes.len().saturating_sub(1);
        for (index, pane) in panes.into_iter().enumerate() {
            if index == last {
                self.connect_leaf(pane, Some(&mut *window), cx);
            } else {
                self.connect_leaf(pane, None, cx);
            }
        }
        cx.notify();
    }
}

fn snapshot_is_blank(snapshot: &WorkspaceSnapshot) -> bool {
    snapshot.tabs().is_empty() && snapshot.active_tab().is_none() && !snapshot.panel().visible()
}

#[cfg(test)]
fn leaf_sessions(node: &Node) -> Vec<Option<SessionId>> {
    match node {
        Node::Leaf(leaf) => vec![leaf.session()],
        Node::Split { first, second, .. } => {
            let mut ids = leaf_sessions(first);
            ids.extend(leaf_sessions(second));
            ids
        }
    }
}

fn sessions_in_node(node: &Node) -> Vec<SessionId> {
    match node {
        Node::Leaf(leaf) => leaf.session().into_iter().collect(),
        Node::Split { first, second, .. } => {
            let mut ids = sessions_in_node(first);
            ids.extend(sessions_in_node(second));
            ids
        }
    }
}

fn node_has_pending(node: &Node) -> bool {
    match node {
        Node::Leaf(leaf) => leaf.is_pending(),
        Node::Split { first, second, .. } => node_has_pending(first) || node_has_pending(second),
    }
}

fn collect_leaves<'a>(node: &'a Node, out: &mut Vec<&'a crate::app::layout::Leaf>) {
    match node {
        Node::Leaf(leaf) => out.push(leaf),
        Node::Split { first, second, .. } => {
            collect_leaves(first, out);
            collect_leaves(second, out);
        }
    }
}

fn find_node(node: &Node, pane: PaneId) -> Option<&Node> {
    match node {
        Node::Leaf(leaf) if leaf.id() == pane => Some(node),
        Node::Leaf(_) => None,
        Node::Split { first, second, .. } => {
            find_node(first, pane).or_else(|| find_node(second, pane))
        }
    }
}

fn split_ratio(node: &Node, split: u64) -> Option<u16> {
    let (id, _, ratio, first, second) = node.split()?;
    if id == split {
        return Some(ratio);
    }
    split_ratio(first, split).or_else(|| split_ratio(second, split))
}

fn components_banner(tone: Tone, metrics: crate::app::theme::Metrics) -> gpui::Div {
    crate::app::components::banner(tone, metrics)
}

#[cfg(test)]
impl ShelluneView {
    pub fn split_active_for_test(
        &mut self,
        axis: SplitAxis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_active(axis, window, cx);
    }

    pub fn focus_direction_for_test(
        &mut self,
        direction: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_direction(direction, window, cx);
    }

    pub fn resize_direction_for_test(
        &mut self,
        direction: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_direction(direction, window, cx);
    }

    pub fn layout_ratios_for_test(&self) -> Vec<(SplitAxis, u16)> {
        self.workspace.layout().active_ratios()
    }

    pub fn active_leaf_sessions_for_test(&self) -> Vec<Option<SessionId>> {
        self.workspace
            .layout()
            .active_tab()
            .map(|tab| leaf_sessions(tab.root()))
            .unwrap_or_default()
    }

    pub fn pending_pane_count_for_test(&self) -> usize {
        self.workspace
            .layout()
            .leaves()
            .iter()
            .filter(|leaf| leaf.is_pending())
            .count()
    }

    pub fn missing_host_panes_for_test(&self) -> Vec<HostId> {
        self.workspace
            .layout()
            .leaves()
            .iter()
            .filter_map(|leaf| {
                leaf.mark()
                    .map(|crate::app::layout::LeafMark::MissingHost { host }| *host)
            })
            .collect()
    }

    pub fn layout_notice_count_for_test(&self) -> usize {
        self.workspace.layout().notices().len()
    }

    pub fn layout_error_for_test(&self) -> Option<&str> {
        self.layout_error.as_deref()
    }

    pub fn layout_tab_count_for_test(&self) -> usize {
        self.workspace.layout().tabs().len()
    }

    pub fn leaf_ids_for_test(&self) -> Vec<PaneId> {
        self.workspace
            .layout()
            .leaves()
            .iter()
            .map(|leaf| leaf.id())
            .collect()
    }

    pub fn leaf_snapshot_for_test(
        &self,
        pane: PaneId,
    ) -> Option<(bool, PaneTarget, Option<SessionId>)> {
        self.workspace
            .layout()
            .leaves()
            .iter()
            .find(|leaf| leaf.id() == pane)
            .map(|leaf| (leaf.is_pending(), leaf.target().clone(), leaf.session()))
    }

    pub fn split_pending_for_test(
        &mut self,
        axis: SplitAxis,
        target: PaneTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        let id = self.workspace.layout_mut().split_pending(axis, target)?;
        self.present_active_pane(window, cx);
        self.checkpoint_layout();
        Some(id)
    }

    pub fn connect_leaf_for_test(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        self.connect_leaf(pane, None, cx);
    }

    pub fn local_error_for_test(&self) -> Option<String> {
        self.local_error.clone()
    }

    pub fn close_pending_for_test(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(pane) = self
            .workspace
            .layout()
            .leaves()
            .iter()
            .find(|leaf| leaf.is_pending())
            .map(|leaf| leaf.id())
        else {
            return false;
        };
        self.close_one_pane(pane, cx);
        true
    }
}
