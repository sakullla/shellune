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
    LocalShell, PaneId, PaneTarget, PanelSection, SessionId, SessionKind, SplitAxis, TabId,
    TerminalMode, WorkspaceSnapshot,
};
use shellune_storage::WorkspaceStore;

use crate::app::components::Variant;
use crate::app::icons::Icon;
use crate::app::layout::{Direction, LayoutNotice, Node};
use crate::app::nav::Destination;
use crate::app::theme::{self, Tone, palette, radius, spacing};
use crate::app::view::{ShelluneView, SplitDrag};

/// The width of the band between two split panes that the pointer drags, before zoom.
const DIVIDER_BAND: f32 = 12.0;

/// The crisp 1px centre hairline drawn inside the split divider band.
const DIVIDER_LINE: f32 = 1.0;

/// The gap between the grip dots on a split divider, before zoom.
const DIVIDER_GRIP_GAP: f32 = 1.0;

/// The height of a split pane's own head, before zoom.
const PANE_HEAD_HEIGHT: f32 = 26.0;

/// The edge of the emblem on a pane still waiting to start, before zoom.
const PENDING_EMBLEM_SIZE: f32 = 40.0;

/// The widest the explanation on a waiting pane grows before it wraps.
const PENDING_WORDS_MAX_WIDTH: f32 = 420.0;

impl ShelluneView {
    /// Reads `workspace.json` into pending panes.
    ///
    /// A missing file is a first run and leaves the page the caller selected.
    /// A damaged file is reported and left unchanged. Nothing in the snapshot is
    /// started.
    #[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
    pub(super) fn restore_workspace_layout(&mut self) {
        let store = WorkspaceStore::at(self.store.root());
        let read = store.load().map_err(|error| error.to_string());
        self.adopt_workspace_layout(read);
    }

    /// Takes `workspace.json` as it was read: now, or behind the splash (R7). The
    /// hosts are adopted first, so a pane that names a deleted record is marked.
    pub(super) fn adopt_workspace_layout(
        &mut self,
        read: Result<shellune_core::WorkspaceSnapshot, String>,
    ) {
        match read {
            Err(error) => {
                self.layout_persist = false;
                self.layout_error = Some(error);
            }
            Ok(snapshot) => {
                self.layout_persist = true;
                if snapshot_is_blank(&snapshot) {
                    return;
                }
                // A pane may name a host or a connection of another protocol; both are
                // saved records, so a restore asks the book about either one.
                let restored = snapshot.restore(|id| self.book.contains_connection(id));
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
        } else {
            self.focus_return = None;
            self.restore_focus_pending = true;
        }
        cx.notify();
    }

    /// Shows the tab the layout just selected.
    pub(super) fn show_layout_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.workspace.layout().active_session() {
            if self.pane_for(id).is_some() || self.desktop_for(id).is_some() {
                self.activate_in(id, window, cx);
                return;
            }
            if self.has_session_form(id) {
                if self.workspace.destination() != Destination::LocalShells {
                    self.workspace.select(Destination::LocalShells);
                    self.workspace
                        .set_context(shellune_core::PanelSection::None);
                }
                self.focus_attempt(id, cx);
                self.focus_remote_form_in(window, cx);
                return;
            }
            if self.form_session.is_some() && self.form_session != Some(id) {
                self.park_focused_form();
                self.form_session = None;
            }
            self.active = Some(id);
            self.session_terminal = true;
            self.showing_connection_form = false;
            self.focus_return = None;
            let handle = self.destination_focus(self.workspace.destination()).clone();
            window.focus(&handle);
            cx.notify();
            return;
        }
        if self.form_session.is_some() {
            self.park_focused_form();
            self.form_session = None;
        }
        self.active = None;
        self.session_terminal = true;
        self.showing_connection_form = false;
        self.focus_return = None;
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
            .any(|id| self.pane_for(*id).is_some() || self.desktop_for(*id).is_some())
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
                    self.start_shell(shell, cx);
                }
                SessionKind::Remote { host } => {
                    self.workspace
                        .layout_mut()
                        .split_pending(axis, PaneTarget::remote(host));
                    self.present_active_pane(window, cx);
                    self.checkpoint_layout();
                }
                // A desktop or serial tab splits into a waiting pane of its own
                // protocol: a split records a target, it does not connect or open one.
                SessionKind::Rdp { host } => {
                    self.workspace
                        .layout_mut()
                        .split_pending(axis, PaneTarget::rdp(host));
                    self.present_active_pane(window, cx);
                    self.checkpoint_layout();
                }
                SessionKind::Vnc { host } => {
                    self.workspace
                        .layout_mut()
                        .split_pending(axis, PaneTarget::vnc(host));
                    self.present_active_pane(window, cx);
                    self.checkpoint_layout();
                }
                SessionKind::Serial { host } => {
                    self.workspace
                        .layout_mut()
                        .split_pending(axis, PaneTarget::serial(host));
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

    pub(super) fn split_session_pane(
        &mut self,
        id: SessionId,
        axis: SplitAxis,
        cx: &mut Context<Self>,
    ) {
        let _ = self.workspace.layout_mut().focus_session(id);
        self.active = self.workspace.layout().active_session();
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
                self.start_shell(shell, cx);
            }
            SessionKind::Remote { host } => {
                if self.form_session.is_some() {
                    self.park_focused_form();
                    self.form_session = None;
                }
                self.showing_connection_form = false;
                self.workspace
                    .layout_mut()
                    .split_pending(axis, PaneTarget::remote(host));
                self.sync_layout_focus(cx);
                self.focus_return = None;
                self.restore_focus_pending = true;
                self.checkpoint_layout();
                cx.notify();
            }
            SessionKind::Rdp { host } => {
                if self.form_session.is_some() {
                    self.park_focused_form();
                    self.form_session = None;
                }
                self.showing_connection_form = false;
                self.workspace
                    .layout_mut()
                    .split_pending(axis, PaneTarget::rdp(host));
                self.sync_layout_focus(cx);
                self.focus_return = None;
                self.restore_focus_pending = true;
                self.checkpoint_layout();
                cx.notify();
            }
            SessionKind::Vnc { host } => {
                if self.form_session.is_some() {
                    self.park_focused_form();
                    self.form_session = None;
                }
                self.showing_connection_form = false;
                self.workspace
                    .layout_mut()
                    .split_pending(axis, PaneTarget::vnc(host));
                self.sync_layout_focus(cx);
                self.focus_return = None;
                self.restore_focus_pending = true;
                self.checkpoint_layout();
                cx.notify();
            }
            SessionKind::Serial { host } => {
                if self.form_session.is_some() {
                    self.park_focused_form();
                    self.form_session = None;
                }
                self.showing_connection_form = false;
                self.workspace
                    .layout_mut()
                    .split_pending(axis, PaneTarget::serial(host));
                self.sync_layout_focus(cx);
                self.focus_return = None;
                self.restore_focus_pending = true;
                self.checkpoint_layout();
                cx.notify();
            }
        }
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

    pub(super) fn toggle_active_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.layout_mut().toggle_zoom() {
            self.present_active_pane(window, cx);
            self.checkpoint_layout();
            cx.notify();
        }
    }

    fn zoom_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.layout().active_pane() != Some(pane) {
            let _ = self.workspace.layout_mut().focus_pane(pane);
        }
        if self.workspace.layout_mut().toggle_zoom() {
            self.present_active_pane(window, cx);
            self.checkpoint_layout();
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

    pub(super) fn reorder_tab_by_id(&mut self, tab: TabId, delta: isize, cx: &mut Context<Self>) {
        let prev_active = self.workspace.layout().active_tab().map(|t| t.id());
        let Some(index) = self
            .workspace
            .layout()
            .tabs()
            .iter()
            .position(|candidate| candidate.id() == tab)
        else {
            return;
        };
        let _ = self.workspace.layout_mut().select_index(index);
        if self.workspace.layout_mut().reorder_active(delta) {
            if let Some(prev) = prev_active
                && prev != tab
                && let Some(prev_idx) = self
                    .workspace
                    .layout()
                    .tabs()
                    .iter()
                    .position(|candidate| candidate.id() == prev)
            {
                let _ = self.workspace.layout_mut().select_index(prev_idx);
            }
            self.sync_layout_focus(cx);
            self.checkpoint_layout();
            cx.notify();
        }
    }

    fn present_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.workspace.layout().active_session();
        self.active = session;
        if self.workspace.layout().has_tabs() {
            self.session_terminal = true;
        }
        if let Some(id) = session
            && (self.pane_for(id).is_some() || self.desktop_for(id).is_some())
        {
            if self.form_session.is_some() && self.form_session != Some(id) {
                self.park_focused_form();
                self.form_session = None;
            }
            self.showing_connection_form = false;
            // Moving the keyboard to a pane is looking at that pane's tab, so whatever
            // the tab was marked for has been seen — also when the tab was already on
            // screen and only the focused pane changed (R7).
            self.clear_viewed_attention(cx);
            let handle = self
                .pane_for(id)
                .map(|pane| pane.read(cx).focus_handle().clone())
                .or_else(|| {
                    self.desktop_for(id)
                        .map(|pane| pane.read(cx).focus_handle().clone())
                });
            if let Some(handle) = handle {
                self.focus_return = Some(handle.downgrade());
                window.focus(&handle);
            } else {
                self.focus_return = None;
            }
            cx.notify();
            return;
        }
        if let Some(id) = session
            && self.has_session_form(id)
        {
            self.focus_attempt(id, cx);
            self.focus_remote_form_in(window, cx);
            return;
        }
        if self.form_session.is_some() {
            self.park_focused_form();
            self.form_session = None;
        }
        self.showing_connection_form = false;
        self.clear_viewed_attention(cx);
        self.focus_return = None;
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

    pub(super) fn close_tab_by_id(&mut self, tab: TabId, cx: &mut Context<Self>) {
        let Some(index) = self
            .workspace
            .layout()
            .tabs()
            .iter()
            .position(|candidate| candidate.id() == tab)
        else {
            return;
        };
        self.close_tab_indexes(cx, |candidate| candidate == index);
    }

    pub(super) fn close_other_tabs_by_id(&mut self, tab: TabId, cx: &mut Context<Self>) {
        let keep = self
            .workspace
            .layout()
            .tabs()
            .iter()
            .position(|candidate| candidate.id() == tab);
        self.close_tab_indexes(cx, |index| keep != Some(index));
    }

    pub(super) fn close_tabs_after_by_id(&mut self, tab: TabId, cx: &mut Context<Self>) {
        let keep = self
            .workspace
            .layout()
            .tabs()
            .iter()
            .position(|candidate| candidate.id() == tab);
        self.close_tab_indexes(cx, |index| keep.is_some_and(|keep| index > keep));
    }

    pub(super) fn pending_tab_can_connect(&self, tab: TabId) -> bool {
        let Some(tab) = self.workspace.layout().tab(tab) else {
            return false;
        };
        let mut leaves = Vec::new();
        collect_leaves(tab.root(), &mut leaves);
        leaves
            .into_iter()
            .any(|leaf| leaf.is_pending() && leaf.mark().is_none())
    }

    pub(super) fn connect_pending_tab_from_menu(
        &mut self,
        tab: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self
            .workspace
            .layout()
            .tabs()
            .iter()
            .position(|candidate| candidate.id() == tab)
        else {
            return;
        };
        self.select_tab(index, window, cx);
        if !self.waiting_local_panes().is_empty() {
            self.connect_waiting_local_panes(window, cx);
            return;
        }
        let first_pane = self.workspace.layout().tab(tab).and_then(|t| {
            let mut leaves = Vec::new();
            collect_leaves(t.root(), &mut leaves);
            leaves
                .into_iter()
                .find(|leaf| leaf.is_pending() && leaf.mark().is_none())
                .map(|leaf| leaf.id())
        });
        if let Some(pane) = first_pane {
            self.connect_leaf(pane, Some(window), cx);
        }
    }

    pub(super) fn pending_tab_can_duplicate(&self, tab: TabId) -> bool {
        let Some(tab) = self.workspace.layout().tab(tab) else {
            return false;
        };
        let mut leaves = Vec::new();
        collect_leaves(tab.root(), &mut leaves);
        let Some(leaf) = leaves.first() else {
            return false;
        };
        if leaf.mark().is_some() {
            return false;
        }
        match leaf.target() {
            PaneTarget::Local { shell } => self.shell_for_split(shell.clone()).is_some(),
            PaneTarget::Remote { host } => self.book.host(*host).is_some(),
            PaneTarget::Rdp { host } | PaneTarget::Vnc { host } | PaneTarget::Serial { host } => {
                self.book.connection(*host).is_some()
            }
        }
    }

    pub(super) fn duplicate_pending_tab(
        &mut self,
        tab: TabId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.workspace.layout().tab(tab).and_then(|t| {
            let mut leaves = Vec::new();
            collect_leaves(t.root(), &mut leaves);
            leaves
                .first()
                .filter(|leaf| leaf.mark().is_none())
                .map(|leaf| leaf.target().clone())
        }) else {
            return;
        };
        self.workspace.clear_armed();
        if self.workspace.destination() != Destination::LocalShells {
            self.workspace.select(Destination::LocalShells);
        }
        self.session_terminal = true;
        match target {
            PaneTarget::Local { shell } => {
                if let Some(shell) = self.shell_for_split(shell) {
                    self.start_shell(shell, cx);
                }
            }
            PaneTarget::Remote { host } => {
                if let Some(saved) = self.book.host(host).cloned() {
                    self.open_remote_duplicate(&saved, false, cx);
                }
            }
            PaneTarget::Rdp { host } | PaneTarget::Vnc { host } | PaneTarget::Serial { host } => {
                if self.book.connection(host).is_some() {
                    self.connect_saved_connection(host, cx);
                }
            }
        }
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
            .min_h_0()
            // A page with no terminal on it - a connection being set up - shows this
            // floor.
            .bg(theme::swatch(super::PAGE_FLOOR))
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
        // A connection being set up in its tab is the whole page: its card states the
        // progress, the question or the failure once, so no banner repeats it above.
        if self.connection_inline()
            && !self.connection_behind_screen()
            && let Some(page) = self.connection_page(cx)
        {
            return column
                .child(page)
                .children(self.standalone_session_bar(cx))
                .children(self.focus_mode_exit_pill(cx));
        }
        column = column.children(self.layout_status_banners());
        let session_id = if self.workspace.layout().active_tab().is_some() {
            self.workspace.layout().active_session()
        } else {
            self.active
        };
        // The session's own identity, state and actions are the window's head (R12);
        // only what the head cannot hold - a failed bridge, the result of a save -
        // is drawn here, above the terminal it belongs to.
        for notice in self.session_notices(session_id, window, cx) {
            // Edge to edge, like the banner it is: the box the reason lives in already
            // carries its own padding, and another margin around it took the room the
            // terminal needs at 720x480 with 200% text (R4).
            column = column.child(div().flex_none().child(notice));
        }
        let files = session_id.filter(|id| self.files.is_open(*id));
        // Why the session ended is laid over its own terminal while that terminal is on
        // screen (the pane draws it, see `leaf_view`), and over the session's place when
        // it has no terminal at all. Only a page that shows something else in the
        // terminal's place - a remote desktop's own surface, or the files in a window
        // too narrow to dock them - still states it above, as a card of its own (R3).
        let tree_shown = files.is_none() || self.files_docked();
        let desktop = session_id.is_some_and(|id| self.desktop_for(id).is_some());
        // A remote desktop that has not shown a first frame renders its own centred
        // status card; stacking the top status banner above it would duplicate the
        // error and next step (ADR-5, R5). Once a frame has been painted, the last
        // frame stays visible and the single top banner states why it ended.
        let desktop_without_picture = session_id
            .and_then(|id| self.desktop_for(id))
            .is_some_and(|pane| pane.read(cx).surface().is_empty());
        let overlaid = tree_shown && !desktop;
        let banner = (!overlaid && !desktop_without_picture)
            .then_some(session_id)
            .flatten()
            .filter(|id| self.pane_for(*id).is_some() || self.desktop_for(*id).is_some())
            .and_then(|id| self.status_banner(id, cx));
        // A remote desktop draws no banner of its own, so it is handed the first control
        // of the one above it: Tab reaches the banner from a session that ended, as it
        // does from a terminal (ADR-6).
        if let Some(pane) = session_id.and_then(|id| self.desktop_for(id)) {
            let focus = banner.as_ref().map(|(_, focus)| focus.clone());
            pane.read(cx).set_banner_focus(focus);
        }
        if let Some((banner, _)) = banner {
            // The files have the terminal's place, or a remote desktop draws its own
            // card where the picture was: the banner stands above them (ADR-6, ADR-7).
            column = column.children(banner);
        } else if !overlaid
            && !desktop
            && let Some(notice) = self.active_failure_notice(session_id, cx)
        {
            column = column.child(
                div()
                    .flex()
                    .flex_none()
                    .justify_center()
                    .px(metrics.scale(spacing::SNUG))
                    .pt(metrics.scale(spacing::SNUG))
                    // Room for the card's floating shadow: the page under it is painted
                    // after it and would cut the shadow off in a hard line (R6).
                    .pb(px(spacing::LOOSE))
                    .child(notice),
            );
        }
        if let Some(id) = files {
            // A wide window keeps the terminal on screen beside the files, and a narrow
            // one shows the files in its place: the session behind either arrangement is
            // the same one, so switching cannot stop it (R14).
            let docked = self
                .files_docked()
                .then(|| self.active_tab_tree(cx))
                .flatten();
            let area: gpui::AnyElement = match docked {
                Some(tree) => self
                    .files_with_terminal(id, tree, window, cx)
                    .into_any_element(),
                None => self.files_view(id, window, cx).into_any_element(),
            };
            column = column.child(area).children(self.standalone_session_bar(cx));
        } else if let Some(tree) = self.active_tab_tree(cx) {
            column = column.child(tree);
        }
        // A session with no terminal on screen - still connecting, or ended without one
        // - keeps its bar at the bottom all the same.
        if files.is_none()
            && let Some(id) = session_id
            && self.pane_for(id).is_none()
            && self.desktop_for(id).is_none()
            && self.workspace.session(id).is_some()
        {
            column = column.children(self.standalone_session_bar(cx));
        }
        if self.split_drag.is_some() {
            column = column.child(self.split_drag_canvas(cx));
        }
        column = column
            .children(self.focus_mode_top_exit(cx))
            .children(self.focus_mode_exit_pill(cx));
        column
    }

    /// Low-interference top-center hover zone to exit focus mode on every platform and
    /// session kind, including live RDP/VNC surfaces where the bottom-right pill is
    /// omitted to avoid covering remote window chrome.
    fn focus_mode_top_exit(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if !self.focus_mode || self.desktop_fullscreen.is_some() {
            return None;
        }
        let metrics = self.metrics();
        let ux = crate::app::copy::ux();
        let tooltip = match self.keymap.label("view.focus") {
            Some(shortcut) => format!("{} ({shortcut})", ux.focus_mode_exit),
            None => ux.focus_mode_exit.to_owned(),
        };
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("focus.mode.top.zone")
                        .debug_selector(|| "focus.mode.top.zone".to_owned())
                        .group("focus-top-exit")
                        .flex()
                        .items_start()
                        .justify_center()
                        .pt(metrics.scale(spacing::HAIRLINE))
                        .px(metrics.scale(spacing::BASE))
                        .h(metrics.scale(34.0))
                        .child(
                            div()
                                .occlude()
                                .opacity(0.0)
                                .group_hover("focus-top-exit", |style| style.opacity(1.0))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                                .on_mouse_down(MouseButton::Middle, |_, _, cx| {
                                    cx.stop_propagation()
                                })
                                .flex()
                                .items_center()
                                .rounded(px(radius::MEDIUM))
                                .bg(theme::faded(theme::swatch(palette::CHROME), 0.92))
                                .border_1()
                                .border_color(theme::swatch(palette::BORDER_STRONG))
                                .child(
                                    crate::app::components::icon_label_button(
                                        Icon::Restore,
                                        ux.focus_mode_exit,
                                        Variant::Ghost,
                                        metrics,
                                    )
                                    .h(metrics.compact_control_height())
                                    .id("focus.mode.top.exit")
                                    .debug_selector(|| "focus.mode.top.exit".to_owned())
                                    .tooltip(crate::app::components::tooltip(SharedString::from(
                                        tooltip,
                                    )))
                                    .on_click(cx.listener(
                                        |this, _event, window, cx| {
                                            cx.stop_propagation();
                                            this.toggle_focus_mode(window, cx);
                                        },
                                    )),
                                ),
                        ),
                ),
        )
    }

    /// Lightweight, discoverable corner affordance to leave focus mode without blocking
    /// authentication, certificate, failure banners or remote window controls (R1, R2).
    ///
    /// Live RDP/VNC surfaces omit a permanently resident floating pill so remote window
    /// chrome and hit targets are never occluded; focus mode is toggled via `view.focus`
    /// (`Alt+Shift+F` / `Opt+Shift+F`). Non-desktop and non-picture views anchor the
    /// exit control at the bottom-right with `.occlude()` and mouse-down propagation
    /// stopped so clicks never bleed through to the underlying pane.
    fn focus_mode_exit_pill(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if !self.focus_mode || self.desktop_fullscreen.is_some() {
            return None;
        }
        let session_id = if self.workspace.layout().active_tab().is_some() {
            self.workspace.layout().active_session()
        } else {
            self.active
        };
        let live_desktop = session_id
            .and_then(|id| self.desktop_for(id))
            .is_some_and(|pane| !pane.read(cx).surface().is_empty());
        if live_desktop {
            return None;
        }
        let metrics = self.metrics();
        let ux = crate::app::copy::ux();
        let tooltip = match self.keymap.label("view.focus") {
            Some(shortcut) => format!("{} ({shortcut})", ux.focus_mode_exit),
            None => ux.focus_mode_exit.to_owned(),
        };
        Some(
            div()
                .absolute()
                .bottom(metrics.scale(spacing::TIGHT))
                .right(metrics.scale(spacing::TIGHT))
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
                .flex()
                .items_center()
                .rounded(px(radius::MEDIUM))
                .bg(theme::faded(theme::swatch(palette::CHROME), 0.92))
                .border_1()
                .border_color(theme::swatch(palette::BORDER_STRONG))
                .child(
                    crate::app::components::icon_only_button(Icon::Restore, metrics)
                        .size(metrics.hit_target())
                        .id("focus.mode.exit")
                        .debug_selector(|| "focus.mode.exit".to_owned())
                        .tooltip(crate::app::components::tooltip(SharedString::from(tooltip)))
                        .on_click(cx.listener(|this, _event, window, cx| {
                            cx.stop_propagation();
                            this.toggle_focus_mode(window, cx);
                        })),
                ),
        )
    }

    /// The one status of the session on screen, when it has ended (R17).
    ///
    /// The session record answers first: it is where a refused reconnect is written, so
    /// a notice read from the pane alone would keep stating the older problem. The pane
    /// is only asked while the record has not caught up with it, and either way the
    /// drawing comes from the same projection, so one session never shows two states.
    fn active_failure_notice(
        &self,
        id: Option<SessionId>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let id = id?;
        if let Some(status) = self.workspace.status(id)
            && status.phase() == crate::app::workspace::SessionPhase::Ended
        {
            return Some(self.failure_notice(id, &status, cx));
        }
        let failure = self
            .pane_for(id)
            .and_then(|pane| pane.read(cx).state().failure())
            .or_else(|| {
                self.desktop_for(id)
                    .and_then(|pane| pane.read(cx).phase().failure())
            })?;
        let status = crate::app::workspace::SessionStatus::ended(failure);
        Some(self.failure_notice(id, &status, cx))
    }

    /// A status card with its entrance: it rises into place once each time it appears
    /// for a session, and is simply there when motion is reduced (R3).
    ///
    /// Every place a card is shown holds it in a box that scrolls once the card is
    /// taller than the space it has, so the card keeps a press to itself but lets the
    /// wheel through to that box: over the card is where the pointer is when the user
    /// wants to read on (R3, R9).
    pub(super) fn entering_status_card(&self, id: SessionId, card: gpui::Div) -> gpui::AnyElement {
        // Only the card keeps the pointer; the slot it is centred in lets a press beside
        // it through to the grid.
        let card = crate::app::components::status_card_slot(
            card.block_mouse_except_scroll(),
            self.metrics(),
        );
        crate::app::theme::enter(
            card,
            SharedString::from(format!("session.status.enter.{id:?}")),
            crate::app::theme::Enter::Rise,
        )
    }

    /// A status card in the place of a terminal: centred on the terminal's floor while
    /// it fits, and in a pane shorter than the card held in a box no taller than the
    /// pane that scrolls from the card's headline down, so nothing of it is pushed out
    /// above the pane where no scroll reaches it (R3, R9).
    ///
    /// The same structure as the card over an ended terminal
    /// ([`crate::app::terminal::TerminalPane`]'s status overlay).
    pub(super) fn status_card_floor(
        &self,
        scroll_id: SharedString,
        card: Option<gpui::AnyElement>,
    ) -> gpui::Div {
        let metrics = self.metrics();
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .bg(theme::swatch(palette::CANVAS))
            .debug_selector(|| "pane.state".to_owned())
            .child(
                div()
                    .id(scroll_id)
                    .debug_selector(|| "pane.state.scroll".to_owned())
                    .flex()
                    .flex_col()
                    .items_center()
                    .w_full()
                    .min_h_0()
                    .max_h_full()
                    .overflow_y_scroll()
                    .p(metrics.scale(spacing::BASE))
                    .children(card),
            )
    }

    /// The card of a session that has no terminal on screen and has not ended: it is
    /// on its way, or it has nothing to show yet (R3).
    ///
    /// It states the session's one status, the same words the tab and the bar use
    /// (R17); while the connection is being made the emblem is the loading mark and the
    /// line under the headline says where the terminal will appear.
    pub(super) fn session_state_card(
        &self,
        id: SessionId,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let locale = self.workspace.locale();
        let status = self.workspace.status(id);
        let starting = status
            .as_ref()
            .is_some_and(|status| status.phase() == crate::app::workspace::SessionPhase::Starting);
        // A start that comes up inside the floor draws no card at all.
        if starting
            && self
                .workspace
                .connection_wait(id)
                .is_some_and(|wait| !wait.due(cx))
        {
            return None;
        }
        let tone = status
            .as_ref()
            .map_or(Tone::Neutral, |status| status.tone());
        let (emblem, headline, detail) = if starting {
            (
                crate::app::components::loading_indicator(metrics),
                status
                    .as_ref()
                    .map(|status| status.headline(locale))
                    .unwrap_or_default(),
                crate::app::copy::ux()
                    .session_connecting_card_hint
                    .to_owned(),
            )
        } else {
            (
                crate::app::icons::icon(Icon::Terminal, metrics.icon_size(), tone.text())
                    .into_any_element(),
                strings.session_no_terminal.to_owned(),
                self.workspace
                    .session(id)
                    .map(|session| crate::app::copy::session_state_label(session.state(), locale))
                    .unwrap_or_default(),
            )
        };
        Some(
            crate::app::components::status_card(
                tone,
                emblem,
                div()
                    .debug_selector(|| "session.state.headline".to_owned())
                    .child(SharedString::from(headline)),
                (!detail.is_empty()).then(|| div().child(SharedString::from(detail))),
                metrics,
            )
            .debug_selector(|| "session.state.card".to_owned()),
        )
    }

    fn layout_status_banners(&self) -> Vec<gpui::Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let mut banners = Vec::new();
        if let Some(reason) = &self.layout_error {
            banners.push(
                crate::app::components::banner(Tone::Danger, metrics)
                    .child(SharedString::from(reason.clone())),
            );
        }
        for notice in self.workspace.layout().notices() {
            let text = match notice {
                LayoutNotice::DanglingTab { .. } => strings.workspace_dangling_tab,
                LayoutNotice::DanglingPane { .. } => strings.workspace_dangling_pane,
            };
            banners.push(
                crate::app::components::banner(Tone::Warning, metrics)
                    .debug_selector(|| "workspace.notice".to_owned())
                    .child(SharedString::from(text.to_owned())),
            );
        }
        banners
    }

    /// The panes of the active tab, or nothing when the window has no tab (R14).
    ///
    /// The docked file view needs the same tree the tab would draw on its own, so it is
    /// built here once instead of being copied into the branch that docks it.
    fn active_tab_tree(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let tab = self.workspace.layout().active_tab()?;
        let zoomed = tab.zoomed();
        Some(if let Some(zoomed) = zoomed {
            self.zoomed_leaf(tab.root(), zoomed, cx).into_any_element()
        } else {
            self.layout_node(tab.root(), true, cx).into_any_element()
        })
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
        let view = cx.entity().downgrade();
        let measure = gpui::canvas(
            move |bounds, _window, app| {
                let span = if across {
                    f32::from(bounds.size.width)
                } else {
                    f32::from(bounds.size.height)
                };
                if span > 1.0 {
                    let _ = view.update(app, |this, _cx| {
                        this.split_spans.borrow_mut().insert(id, span);
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();
        let mut row = div().relative().flex().flex_1().min_w_0().min_h_0();
        row = if across { row } else { row.flex_col() };
        row.child(measure)
            .child(self.sized_child(self.layout_node(first, true, cx), fraction, across))
            .child(self.split_divider(id, axis, cx, metrics))
            .child(self.sized_child(self.layout_node(second, true, cx), 1.0 - fraction, across))
    }

    fn sized_child(&self, child: gpui::Div, fraction: f32, across: bool) -> gpui::Div {
        let fraction = fraction.clamp(0.01, 0.99);
        let child = child
            .flex_basis(gpui::relative(fraction))
            .flex_shrink()
            .min_w_0()
            .min_h_0();
        if across {
            child.min_w_0()
        } else {
            child.min_h_0()
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
        let show_nudges = self.split_hover_band == Some(id)
            || self
                .split_hover_button
                .is_some_and(|(split, _)| split == id)
            || self.split_drag.is_some_and(|drag| drag.split == id)
            || self.split_focused_divider == Some(id);
        let divider_focus = self.divider_focus_handle(id, cx);
        // The band keeps its 12 thickness: it is a continuous drag strip, not a
        // button, and a wider strip would take the space of the panes on both sides.
        // At rest only a quiet 1px centre line is drawn so adjacent scrollbars stay
        // unobstructed; hovering or focusing reveals the two 24×24 nudge buttons (R4).
        div()
            .id(SharedString::from(format!("pane.divider.{id}")))
            // The id is per split; the debug name stays one string so a test can
            // measure the band without knowing which split it belongs to.
            .debug_selector(|| "pane.divider".to_owned())
            .track_focus(&divider_focus)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(metrics.scale(DIVIDER_GRIP_GAP))
            .when(across, |bar| {
                bar.w(metrics.scale(DIVIDER_BAND))
                    .h_full()
                    .flex_col()
                    .cursor_col_resize()
            })
            .when(!across, |bar| {
                bar.h(metrics.scale(DIVIDER_BAND))
                    .w_full()
                    .cursor_row_resize()
            })
            .when(show_nudges, |bar| {
                bar.bg(theme::swatch(palette::SURFACE_HOVER))
            })
            .when(!show_nudges, |bar| bar.bg(theme::clear()))
            .child(
                div()
                    .absolute()
                    .bg(if show_nudges {
                        theme::swatch(palette::BORDER_STRONG)
                    } else {
                        theme::swatch(palette::BORDER)
                    })
                    .when(across, |line| {
                        line.top_0()
                            .bottom_0()
                            .left((metrics.scale(DIVIDER_BAND) - px(DIVIDER_LINE)) / 2.0)
                            .w(px(DIVIDER_LINE))
                    })
                    .when(!across, |line| {
                        line.left_0()
                            .right_0()
                            .top((metrics.scale(DIVIDER_BAND) - px(DIVIDER_LINE)) / 2.0)
                            .h(px(DIVIDER_LINE))
                    })
                    .debug_selector(|| "pane.divider.line".to_owned()),
            )
            .when(show_nudges, |bar| {
                bar.child(self.resize_button(id, shrink, strings.pane_resize_less, "less", cx))
                    .child(self.resize_button(id, grow, strings.pane_resize_more, "more", cx))
            })
            .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                if *hovered && this.split_hover_band != Some(id) {
                    this.split_hover_band = Some(id);
                    cx.notify();
                } else if !*hovered && this.split_hover_band == Some(id) {
                    this.split_hover_band = None;
                    cx.notify();
                }
            }))
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "left" if across => {
                            cx.stop_propagation();
                            this.nudge_split(id, Direction::Left, cx);
                        }
                        "right" if across => {
                            cx.stop_propagation();
                            this.nudge_split(id, Direction::Right, cx);
                        }
                        "up" if !across => {
                            cx.stop_propagation();
                            this.nudge_split(id, Direction::Up, cx);
                        }
                        "down" if !across => {
                            cx.stop_propagation();
                            this.nudge_split(id, Direction::Down, cx);
                        }
                        "tab" => {
                            let splits = this
                                .workspace
                                .layout()
                                .active_tab()
                                .map(|tab| all_split_ids(tab.root()))
                                .unwrap_or_default();
                            if splits.len() > 1
                                && let Some(pos) = splits.iter().position(|split| *split == id)
                            {
                                cx.stop_propagation();
                                let next = if event.keystroke.modifiers.shift {
                                    (pos + splits.len() - 1) % splits.len()
                                } else {
                                    (pos + 1) % splits.len()
                                };
                                let next_id = splits[next];
                                this.split_focused_divider = Some(next_id);
                                let handle = this.divider_focus_handle(next_id, cx);
                                window.focus(&handle);
                                cx.notify();
                            }
                        }
                        "escape" | "enter" => {
                            cx.stop_propagation();
                            this.split_focused_divider = None;
                            this.present_active_pane(window, cx);
                        }
                        _ => {}
                    }
                }),
            )
            .tooltip(crate::app::components::tooltip(
                crate::app::copy::ux().pane_divider_reset_hint,
            ))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    if event.click_count >= 2 {
                        this.split_drag = None;
                        if this.workspace.layout_mut().set_split_ratio(id, 5000) {
                            this.checkpoint_layout();
                            cx.notify();
                        }
                    } else {
                        this.begin_split_drag(id, axis, event.position, cx);
                    }
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
            .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                let key = Some((split, name));
                if *hovered && this.split_hover_button != key {
                    this.split_hover_button = key;
                    cx.notify();
                } else if !*hovered && this.split_hover_button == key {
                    this.split_hover_button = None;
                    cx.notify();
                }
            }))
            .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                cx.stop_propagation();
            })
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
        let next = next.clamp(1000, 9000);
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
        let span = self
            .split_spans
            .borrow()
            .get(&split)
            .copied()
            .unwrap_or(500.0)
            .max(120.0);
        self.split_drag = Some(SplitDrag {
            split,
            axis,
            origin,
            origin_ratio: ratio,
            span,
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
        let ratio_delta = ((delta / drag.span.max(120.0)) * 10_000.0).round() as i32;
        let next = (i32::from(drag.origin_ratio) + ratio_delta).clamp(1000, 9000) as u16;
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
        let pane = leaf.id();
        let active = self.workspace.layout().active_pane() == Some(pane);
        let session = leaf.session();
        // A tab with one pane needs no pane chrome: the session bar already names
        // it and carries the split buttons, and a focus frame around the only pane
        // marks nothing.
        // A waiting pane is no exception: its card names the wait and carries its own
        // remove, so a lone one is not framed as "Pane 1 · Active" over nothing.
        let solo = self.active_tab_is_solo();
        let mut frame = div()
            .id(SharedString::from(format!("pane.leaf.{pane}")))
            .flex()
            .flex_col()
            .flex_1()
            .size_full()
            .overflow_hidden()
            .when(!solo, |frame| {
                let framed = frame.rounded(px(radius::MEDIUM));
                if active {
                    framed
                        .border_2()
                        .border_color(theme::swatch(palette::ACCENT))
                } else {
                    framed
                        .border_1()
                        .border_color(theme::swatch(palette::BORDER))
                }
            })
            .when(active, |frame| {
                frame.debug_selector(|| "pane.active".to_owned())
            })
            .when(!active, |frame| {
                frame
                    .debug_selector(|| "pane.idle".to_owned())
                    .capture_any_mouse_down(cx.listener(
                        move |this, event: &MouseDownEvent, window, cx| {
                            if matches!(event.button, MouseButton::Middle | MouseButton::Right)
                                && this.workspace.layout_mut().focus_pane(pane)
                            {
                                this.present_active_pane(window, cx);
                            }
                        },
                    ))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event, window, cx| {
                    this.focus_leaf(pane, window, cx);
                }),
            );
        if !solo && !self.focus_mode {
            frame = frame.child(self.leaf_header(leaf, active, cx));
        }
        if leaf.is_pending() {
            frame = frame.child(self.pending_card(leaf, cx));
        } else if let Some(id) = session
            && let Some(surface) = self.desktop_for(id)
        {
            // A desktop pane draws its own canvas and its own status card; the
            // window adds no chrome of its own around a remote screen (R18).
            frame = frame.child(surface.clone());
        } else if let Some(id) = session
            && let Some(terminal) = self.pane_for(id)
        {
            terminal.read(cx).set_focus_mode(self.focus_mode);
            // The pane the keys are in carries the session's half of the bar under its
            // grid; the others keep their own bar alone. In focus mode the session chrome
            // is hidden so the terminal keeps maximum room (R1).
            if active
                && !self.focus_mode
                && let Some(parts) = self.session_bar_parts(cx)
            {
                terminal
                    .read(cx)
                    .set_session_slot(crate::app::terminal::SessionSlot {
                        left: parts.left.into_any_element(),
                        controls: parts.controls,
                        pinned: parts.pinned,
                    });
            }
            // Every visible split pane states why its session ended in a banner above
            // the grid it ended on, including background panes before activation (ADR-6).
            self.hand_banner(id, terminal, cx);
            frame = frame.child(terminal.clone());
        } else if let Some(id) = session
            && let Some(screen) = self.reconnect_screen(id).cloned()
        {
            // A remote session that is reconnecting keeps its last screen under the
            // banner that says so, until the new terminal takes its place (ADR-6).
            screen.read(cx).set_focus_mode(self.focus_mode);
            if active
                && !self.focus_mode
                && let Some(parts) = self.session_bar_parts(cx)
            {
                screen
                    .read(cx)
                    .set_session_slot(crate::app::terminal::SessionSlot {
                        left: parts.left.into_any_element(),
                        controls: parts.controls,
                        pinned: parts.pinned,
                    });
            }
            self.hand_banner(id, &screen, cx);
            frame = frame.child(screen);
        } else if let Some(id) = session {
            // No terminal to lay a card over: the card takes the pane's place, centred,
            // on the terminal's own floor.
            let card = self
                .active_failure_notice(Some(id), cx)
                .or_else(|| self.session_state_card(id, cx));
            frame = frame.child(self.status_card_floor(
                SharedString::from(format!("pane.state.{pane}")),
                card.map(|card| self.entering_status_card(id, card)),
            ));
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(frame)
    }

    /// What marks one pane of a split tab (R12).
    ///
    /// It is not a second session bar. The tab already carries the session's name, its
    /// colour and its state; repeating them in every pane is what the merge removed, and
    /// two panes of one session would then write the same sentence twice. A pane is
    /// marked by its slot - "Pane 1", "Pane 2" - and the active one takes the accent, so
    /// where the keys are is still visible. The pane's own close and zoom stay on it:
    /// they belong to that pane, not to the session.
    /// Returns (and lazily creates) the keyboard focus handle for a split divider at any
    /// nesting depth.
    fn divider_focus_handle(&self, id: u64, cx: &mut Context<Self>) -> gpui::FocusHandle {
        self.divider_focuses
            .borrow_mut()
            .entry(id)
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
    }

    /// Resolves a pane leaf's display name, optional secondary target/address, full tooltip,
    /// and connection status tone from either its live session or its pending target.
    fn leaf_identity(
        &self,
        leaf: &crate::app::layout::Leaf,
    ) -> (String, Option<String>, String, Option<Tone>) {
        let strings = self.strings();
        let locale = self.workspace.locale();
        if let Some(session_id) = leaf.session()
            && let Some(session) = self.workspace.session(session_id)
        {
            let name = session.title().to_owned();
            let target = match session.kind() {
                shellune_core::SessionKind::Local { .. } => {
                    super::program_label(&super::session_target(&self.book, session))
                }
                _ => super::session_target(&self.book, session),
            };
            let state = crate::app::copy::session_state_label(session.state(), locale);
            let detail = (!target.is_empty() && target != name).then_some(target);
            let tooltip = match &detail {
                Some(detail) => format!("{name} · {detail} · {state}"),
                None => format!("{name} · {state}"),
            };
            return (
                name,
                detail,
                tooltip,
                Some(super::session_tone(session.state())),
            );
        }
        let (name, detail) = match leaf.target() {
            PaneTarget::Local { shell } => {
                let name = self
                    .shell_for_split(shell.clone())
                    .map(|found| found.name().to_owned())
                    .or_else(|| shell.as_deref().map(super::program_label))
                    .unwrap_or_else(|| strings.session_kind_local.to_owned());
                (name, None)
            }
            PaneTarget::Remote { host } => match self.book.host(*host) {
                Some(saved) => (saved.name().to_owned(), Some(saved.target())),
                None => (strings.pane_missing_host.to_owned(), None),
            },
            PaneTarget::Rdp { host } | PaneTarget::Vnc { host } | PaneTarget::Serial { host } => {
                match self.book.connection(*host) {
                    Some(record) => (record.name().to_owned(), Some(record.target())),
                    None => (strings.pane_missing_host.to_owned(), None),
                }
            }
        };
        let state = if leaf.mark().is_some() {
            strings.pane_missing_host
        } else {
            strings.pane_pending
        };
        let tooltip = match &detail {
            Some(detail) if detail != &name => format!("{name} · {detail} · {state}"),
            _ => format!("{name} · {state}"),
        };
        let tone = if leaf.mark().is_some() {
            Tone::Warning
        } else {
            Tone::Neutral
        };
        (name, detail, tooltip, Some(tone))
    }

    fn leaf_header(
        &self,
        leaf: &crate::app::layout::Leaf,
        active: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let pane = leaf.id();
        let slot = self.pane_slot(pane);
        let (identity, _detail, identity_tooltip, status_tone) = self.leaf_identity(leaf);
        let heading_label = if identity.is_empty() {
            match slot {
                Some(slot) => format!("{} {slot}", strings.pane_label),
                None => strings.pane_label.to_owned(),
            }
        } else {
            identity
        };
        let zoomed = self
            .workspace
            .layout()
            .active_tab()
            .is_some_and(|tab| tab.zoomed() == Some(pane));
        // A pane narrower than its head - a side-by-side split at 200% text - lets the
        // name and the "Active" chip give way with an ellipsis, so zoom and close stay
        // whole inside the pane instead of being cut at its edge.
        div()
            .id(SharedString::from(format!("pane.head.{pane}")))
            .debug_selector(move || format!("pane.head.{pane}"))
            .flex()
            .flex_none()
            .items_center()
            .min_w_0()
            .gap(metrics.scale(spacing::HAIRLINE))
            .h(metrics.scale(PANE_HEAD_HEIGHT))
            .px(metrics.scale(spacing::TIGHT))
            .border_b_1()
            .border_color(theme::swatch(palette::BORDER))
            // The active pane is marked by its own fill and an accent bar, not only by
            // the weight of a word: the frame around it is the other half of the mark.
            .when(active, |row| row.bg(theme::swatch(palette::SURFACE)))
            .when(!active, |row| row.bg(theme::swatch(palette::CANVAS)))
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    if event.click_count() >= 2 {
                        cx.stop_propagation();
                        this.zoom_pane(pane, window, cx);
                    }
                }),
            )
            .child(
                crate::app::components::accent_mark(active, metrics).when(active, |mark| {
                    mark.debug_selector(|| "pane.active.mark".to_owned())
                }),
            )
            .children(status_tone.map(|tone| {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .debug_selector(move || format!("pane.status.{pane}"))
                    .child(crate::app::components::status_dot(tone, metrics))
            }))
            .child(
                div()
                    .id(SharedString::from(format!("pane.slot.{pane}")))
                    .flex_shrink()
                    .min_w_0()
                    .truncate()
                    .text_size(metrics.caption_size())
                    .when(active, |label| {
                        label
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    })
                    .when(!active, |label| {
                        label.text_color(theme::swatch(palette::TEXT_MUTED))
                    })
                    .debug_selector(move || format!("pane.slot.{pane}"))
                    .tooltip(crate::app::components::tooltip(SharedString::from(
                        identity_tooltip,
                    )))
                    .child(SharedString::from(heading_label)),
            )
            // The word, not only the accent: which pane holds the keys is never said by
            // colour alone (R8). When zoomed, the chip also states that other panes are
            // hidden behind this one.
            .when(active, |row| {
                let chip_label: SharedString = if zoomed {
                    SharedString::from(format!(
                        "{} · {}",
                        strings.pane_active,
                        crate::app::copy::ux().pane_zoomed
                    ))
                } else {
                    SharedString::from(strings.pane_active)
                };
                row.child(crate::app::components::chip_within(
                    chip_label,
                    Tone::Accent,
                    metrics,
                ))
            })
            .child(div().flex_1())
            .when(active, |row| {
                let zoom_base = if zoomed {
                    strings.pane_unzoom
                } else {
                    strings.pane_zoom
                };
                let zoom_tip = match self.keymap.label("pane.zoom") {
                    Some(shortcut) => format!("{zoom_base} ({shortcut})"),
                    None => zoom_base.to_owned(),
                };
                let zoom_btn = crate::app::components::toggle_icon_button(
                    if zoomed { Icon::Restore } else { Icon::Zoom },
                    zoomed,
                    metrics,
                )
                .flex_none()
                .w(metrics.hit_target())
                .h(metrics.hit_target())
                .id(SharedString::from(format!("pane.zoom.{pane}")))
                .debug_selector(|| "pane.zoom".to_owned())
                .tooltip(crate::app::components::tooltip(SharedString::from(
                    zoom_tip,
                )))
                .on_click(cx.listener(|this, _event, window, cx| {
                    cx.stop_propagation();
                    this.toggle_active_zoom(window, cx);
                }));
                let close_tip = match self.keymap.label("tab.close") {
                    Some(shortcut) => format!("{} ({shortcut})", strings.pane_close),
                    None => strings.pane_close.to_owned(),
                };
                row.child(zoom_btn).child(self.pane_icon(
                    pane,
                    "close",
                    Icon::Close,
                    close_tip,
                    cx,
                    move |this, _window, cx| this.close_one_pane(pane, cx),
                ))
            })
            .when(!active, |row| {
                let zoom_tip = match self.keymap.label("pane.zoom") {
                    Some(shortcut) => format!("{} ({shortcut})", strings.pane_zoom),
                    None => strings.pane_zoom.to_owned(),
                };
                row.child(
                    crate::app::components::icon_only_button(Icon::Zoom, metrics)
                        .flex_none()
                        .w(metrics.hit_target())
                        .h(metrics.hit_target())
                        .id(SharedString::from(format!("pane.zoom.{pane}")))
                        .debug_selector(move || format!("pane.zoom.{pane}"))
                        .tooltip(crate::app::components::tooltip(SharedString::from(
                            zoom_tip,
                        )))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|_this, _event, _window, cx| cx.stop_propagation()),
                        )
                        .on_click(cx.listener(move |this, _event, window, cx| {
                            cx.stop_propagation();
                            this.zoom_pane(pane, window, cx);
                        })),
                )
                .child(
                    crate::app::components::icon_only_button(Icon::Close, metrics)
                        .flex_none()
                        .w(metrics.hit_target())
                        .h(metrics.hit_target())
                        .id(SharedString::from(format!("pane.close.{pane}")))
                        .debug_selector(move || format!("pane.close.{pane}"))
                        .tooltip(crate::app::components::tooltip(strings.pane_close))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|_this, _event, _window, cx| cx.stop_propagation()),
                        )
                        .on_click(cx.listener(move |this, _event, _window, cx| {
                            cx.stop_propagation();
                            this.close_one_pane(pane, cx);
                        })),
                )
            })
    }

    /// Where one pane sits in its tab, counting from one, as the pane mark writes it.
    fn pane_slot(&self, pane: PaneId) -> Option<usize> {
        let tab = self.workspace.layout().active_tab()?;
        let mut leaves = Vec::new();
        collect_leaves(tab.root(), &mut leaves);
        leaves
            .iter()
            .position(|leaf| leaf.id() == pane)
            .map(|index| index + 1)
    }

    /// Whether the active tab holds exactly one pane and shows it unzoomed.
    pub(super) fn active_tab_is_solo(&self) -> bool {
        self.workspace
            .layout()
            .active_tab()
            .is_some_and(|tab| matches!(tab.root(), Node::Leaf(_)) && tab.zoomed().is_none())
    }

    /// The split buttons, for the head of whichever tab is on screen (R5).
    ///
    /// They sit in the head rather than in one pane's own strip: splitting is a
    /// session-level action that acts on the active pane, and a button drawn inside the
    /// pane it would split is one more copy of the pane chrome (R12).
    ///
    /// Each button is its own control, so a bar too narrow for both leaves the second
    /// out whole instead of cutting it (R3).
    pub(super) fn split_buttons(&self, cx: &mut Context<Self>) -> Vec<gpui::Stateful<gpui::Div>> {
        let Some(pane) = self.workspace.layout().active_pane() else {
            return Vec::new();
        };
        let strings = self.strings();
        let control = self.metrics().hit_target();
        let right_tip = match self.keymap.label("pane.split-right") {
            Some(shortcut) => format!("{} ({shortcut})", strings.pane_split_right),
            None => strings.pane_split_right.to_owned(),
        };
        let down_tip = match self.keymap.label("pane.split-down") {
            Some(shortcut) => format!("{} ({shortcut})", strings.pane_split_down),
            None => strings.pane_split_down.to_owned(),
        };
        vec![
            self.pane_icon(
                pane,
                "split.right",
                Icon::SplitRight,
                right_tip,
                cx,
                |this, window, cx| this.split_active(SplitAxis::LeftRight, window, cx),
            )
            .size(control)
            .flex_none(),
            self.pane_icon(
                pane,
                "split.down",
                Icon::SplitDown,
                down_tip,
                cx,
                |this, window, cx| this.split_active(SplitAxis::TopBottom, window, cx),
            )
            .size(control)
            .flex_none(),
        ]
    }

    fn pane_icon(
        &self,
        pane: PaneId,
        name: &'static str,
        icon: Icon,
        label: impl Into<SharedString> + 'static,
        cx: &mut Context<Self>,
        click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let metrics = self.metrics();
        let selector = format!("pane.{name}");
        crate::app::components::icon_only_button(icon, metrics)
            // The pane header is 28px tall; the button keeps the 24x24 minimum so a
            // reduced text size cannot shrink the target (R4).
            .flex_none()
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
        let (target_name, target_detail, _tooltip, _tone) = self.leaf_identity(leaf);
        let target_summary = match target_detail {
            Some(detail) if detail != target_name => format!("{target_name} · {detail}"),
            _ => target_name,
        };
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
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(metrics.scale(PENDING_EMBLEM_SIZE))
                    .mb(metrics.scale(spacing::HAIRLINE))
                    .rounded(px(radius::MEDIUM))
                    .bg(theme::swatch(palette::ACCENT_TINT))
                    .child(crate::app::icons::icon(
                        Icon::Terminal,
                        metrics.icon_size(),
                        theme::swatch(palette::ACCENT_ON_TINT),
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .text_size(metrics.title_size())
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(strings.pane_pending),
                    )
                    .child(crate::app::components::tag(mode, metrics)),
            )
            .when(!missing && !target_summary.is_empty(), |card| {
                card.child(
                    div()
                        .max_w(metrics.scale(PENDING_WORDS_MAX_WIDTH))
                        .truncate()
                        .text_size(metrics.label_size())
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .debug_selector(move || format!("pane.pending.target.{pane}"))
                        .child(SharedString::from(target_summary)),
                )
            })
            .child(
                div()
                    .max_w(metrics.scale(PENDING_WORDS_MAX_WIDTH))
                    .mb(metrics.scale(spacing::HAIRLINE))
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .text_center()
                    .child(strings.pane_pending_body),
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
                    Variant::Ghost,
                    cx,
                    move |this, cx| this.close_one_pane(pane, cx),
                )),
        )
    }

    /// A tab that is only waiting, drawn in the same strip and the same shape as the
    /// tabs of live sessions (R2).
    ///
    /// Its dot is the neutral "not started" mark and its word is in the tooltip, so a
    /// restored strip of waiting tabs is as narrow as one of running ones.
    pub(super) fn pending_tab(
        &self,
        index: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let tab = self.workspace.layout().tabs().get(index);
        let tab_id = tab.map(|tab| tab.id());
        let title = tab
            .map(|tab| tab.display_title().to_owned())
            .unwrap_or_else(|| strings.pane_pending.to_owned());
        let accent = tab.and_then(|tab| tab.accent());
        let selected = self.tab_is_current(index);
        let tooltip = format!("{title} · {}", strings.pane_pending);
        let title_width = super::tab_title_width(window, &title, metrics);
        let frame = super::browser_tab(
            ("session.tab", index),
            format!("session.tab.{index}"),
            selected,
            metrics,
        )
        // As wide as its title, within the strip's bounds, as a session tab (R2).
        .max_w(metrics.tab_max_width())
        .flex_shrink()
        .min_w(self.tab_min_width())
        .tooltip(crate::app::components::tooltip(tooltip))
        .on_click(
            cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                this.select_tab(index, window, cx);
                if event.click_count() == 2
                    && let Some(tab) = this.workspace.layout().tabs().get(index).map(|t| t.id())
                {
                    this.open_tab_rename(tab, window, cx);
                }
            }),
        )
        .on_mouse_down(
            MouseButton::Middle,
            cx.listener(move |this, _event, _window, cx| {
                cx.stop_propagation();
                this.close_pending_tab(index, cx);
            }),
        )
        .when_some(tab_id, |frame, tab_id| {
            frame.on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_pending_tab_menu(tab_id, event, window, cx);
                }),
            )
        });
        let body = super::tab_body(metrics)
            .child(crate::app::components::pulsing_status_dot(
                Tone::Neutral,
                metrics,
                false,
                ("session.tab.pulse", index),
                format!("session.tab.dot.{index}"),
            ))
            .children(accent.map(|accent| {
                super::tab_accent_mark(accent, metrics, format!("session.tab.accent.{index}"))
            }))
            // As a session tab: a compact row's one tab gives its icon to the title.
            .when(!self.header_compact(), |body| {
                body.child(super::tab_icon(Icon::Terminal, selected, metrics))
            })
            .child(super::tab_title_label(
                title,
                title_width,
                selected,
                format!("session.tab.title.{index}"),
                metrics,
            ));
        frame.child(body).child(
            super::tab_close_button(
                ("session.tab.close", index),
                format!("session.tab.close.{index}"),
                format!("session.tab.{index}"),
                selected,
                metrics,
            )
            .on_click(cx.listener(move |this, _event, _window, cx| {
                cx.stop_propagation();
                this.close_pending_tab(index, cx);
            })),
        )
    }

    pub(super) fn close_pending_tab(&mut self, index: usize, cx: &mut Context<Self>) {
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
        _window: Option<&mut Window>,
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
                self.start_shell(shell, cx);
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
            // A desktop pane starts its own protocol worker, one session per pane
            // (R18, R19), and a serial pane opens its own device (R21). Neither is
            // dialled before the user points at it, which is why a restored layout
            // places these panes as waiting ones.
            PaneTarget::Rdp { host } => {
                self.open_desktop_leaf(pane, host, shellune_core::ProtocolKind::Rdp, cx);
            }
            PaneTarget::Vnc { host } => {
                self.open_desktop_leaf(pane, host, shellune_core::ProtocolKind::Vnc, cx);
            }
            PaneTarget::Serial { host } => {
                self.open_serial_leaf(pane, host, cx);
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

    /// Starts every waiting local pane of the active tab and leaves the keyboard in the
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

#[cfg(test)]
fn first_split_id(node: &Node) -> Option<u64> {
    let (id, _, _, _, _) = node.split()?;
    Some(id)
}

fn all_split_ids(node: &Node) -> Vec<u64> {
    let mut ids = Vec::new();
    fn collect(node: &Node, out: &mut Vec<u64>) {
        if let Some((id, _, _, first, second)) = node.split() {
            out.push(id);
            collect(first, out);
            collect(second, out);
        }
    }
    collect(node, &mut ids);
    ids
}

fn split_ratio(node: &Node, split: u64) -> Option<u16> {
    let (id, _, ratio, first, second) = node.split()?;
    if id == split {
        return Some(ratio);
    }
    split_ratio(first, split).or_else(|| split_ratio(second, split))
}

#[cfg(test)]
impl ShelluneView {
    pub fn focus_first_split_divider_for_test(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let split = self
            .workspace
            .layout()
            .active_tab()
            .and_then(|tab| first_split_id(tab.root()));
        self.split_focused_divider = split;
        if let Some(split) = split {
            let handle = self.divider_focus_handle(split, cx);
            window.focus(&handle);
        }
        cx.notify();
    }

    pub fn active_split_ids_for_test(&self) -> Vec<u64> {
        self.workspace
            .layout()
            .active_tab()
            .map(|tab| all_split_ids(tab.root()))
            .unwrap_or_default()
    }

    pub fn focus_split_divider_for_test(
        &mut self,
        split: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_focused_divider = Some(split);
        let handle = self.divider_focus_handle(split, cx);
        window.focus(&handle);
        cx.notify();
    }

    pub fn pane_header_identity_for_test(&self, pane: PaneId) -> Option<String> {
        let leaf = self
            .workspace
            .layout()
            .leaves()
            .into_iter()
            .find(|leaf| leaf.id() == pane)?;
        Some(self.leaf_identity(leaf).0)
    }
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

    pub fn missing_host_panes_for_test(&self) -> Vec<shellune_core::HostId> {
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
