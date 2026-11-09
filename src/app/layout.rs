//! The live tab and pane tree (R4).
//!
//! A leaf names a session or a target that has not been started. Restoring a
//! snapshot fills this tree with pending leaves only: no process, socket or
//! forward is created here. Closing one leaf leaves every other leaf in place.

use shellune_core::{
    HostId, PaneId, PaneNode, PaneTarget, PanelConfig, PendingConnection, RestoredNode,
    RestoredWorkspace, SessionId, SplitAxis, TabAccent, TabId, TerminalMode, WorkspaceBuildError,
    WorkspaceIssue, WorkspaceSnapshot, WorkspaceTab, tab_name_in_limits,
};

/// Which way a pane moves or grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Toward the left child of a side-by-side split.
    Left,
    /// Toward the right child of a side-by-side split.
    Right,
    /// Toward the upper child of a stacked split.
    Up,
    /// Toward the lower child of a stacked split.
    Down,
}

/// How the next session is placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Armed {
    /// Insert the session beside this pane.
    Split { pane: PaneId, axis: SplitAxis },
    /// Bind the session to a pane that is already on screen and waiting.
    Adopt { pane: PaneId },
}

/// One problem that belongs to a single leaf.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeafMark {
    /// The host id is not in the saved book.
    MissingHost { host: HostId },
}

/// A notice that names one stale id. The other tabs stay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutNotice {
    /// The saved active tab is not in the snapshot.
    DanglingTab { tab: TabId },
    /// The saved active pane is not on its tab.
    DanglingPane { tab: TabId, pane: PaneId },
}

/// A leaf in the live tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leaf {
    id: PaneId,
    session: Option<SessionId>,
    target: PaneTarget,
    mode: TerminalMode,
    mark: Option<LeafMark>,
}

impl Node {
    /// The split, when this node is one.
    #[must_use]
    pub fn split(&self) -> Option<(u64, SplitAxis, u16, &Node, &Node)> {
        match self {
            Self::Split {
                id,
                axis,
                ratio,
                first,
                second,
            } => Some((*id, *axis, *ratio, first, second)),
            Self::Leaf(_) => None,
        }
    }
}

impl Leaf {
    fn live(session: SessionId, target: PaneTarget) -> Self {
        Self {
            id: PaneId::new(),
            session: Some(session),
            target,
            mode: TerminalMode::Classic,
            mark: None,
        }
    }

    fn pending(target: PaneTarget, mode: TerminalMode) -> Self {
        Self {
            id: PaneId::new(),
            session: None,
            target,
            mode,
            mark: None,
        }
    }

    /// Stable id.
    #[must_use]
    pub fn id(&self) -> PaneId {
        self.id
    }

    /// The session this pane shows, when one has been started.
    #[must_use]
    pub fn session(&self) -> Option<SessionId> {
        self.session
    }

    /// What a later, explicit connect would open.
    #[must_use]
    pub fn target(&self) -> &PaneTarget {
        &self.target
    }

    /// Terminal presentation. Not live output.
    #[must_use]
    pub fn mode(&self) -> TerminalMode {
        self.mode
    }

    /// A problem that belongs only to this pane.
    #[must_use]
    pub fn mark(&self) -> Option<&LeafMark> {
        self.mark.as_ref()
    }

    /// Whether this pane is waiting and has no process.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.session.is_none()
    }
}

/// A pane tree. Splits carry an id so a resize drag can name one of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// A single pane.
    Leaf(Leaf),
    /// Two children.
    Split {
        /// Identity of this split, for resize. Not written to the snapshot.
        id: u64,
        /// Which way the children are arranged.
        axis: SplitAxis,
        /// Share of the first child, in `1..=9999`.
        ratio: u16,
        /// The first child.
        first: Box<Node>,
        /// The second child.
        second: Box<Node>,
    },
}

/// One tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    id: TabId,
    title: String,
    /// The name the user gave the tab, when they gave one.
    custom_title: Option<String>,
    /// The colour that tells this tab from its neighbours, when one was chosen.
    accent: Option<TabAccent>,
    root: Node,
    active: Option<PaneId>,
    /// The pane that fills the tab, when the user has zoomed. Not persisted.
    zoomed: Option<PaneId>,
}

impl Tab {
    /// Stable id.
    #[must_use]
    pub fn id(&self) -> TabId {
        self.id
    }

    /// Title used when the tab has no session to name it.
    ///
    /// This is the automatic title: what the tab would be called without the user's
    /// own name. Saving writes it, so restoring the automatic title still works.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The name the user gave this tab, if any.
    #[must_use]
    pub fn custom_title(&self) -> Option<&str> {
        self.custom_title
            .as_deref()
            .filter(|title| !title.trim().is_empty())
    }

    /// The name the tab strip shows: the user's own, or the automatic title.
    #[must_use]
    pub fn display_title(&self) -> &str {
        self.custom_title().unwrap_or(&self.title)
    }

    /// The colour chosen for this tab, if any. Never a status.
    #[must_use]
    pub fn accent(&self) -> Option<TabAccent> {
        self.accent
    }

    /// Pane tree.
    #[must_use]
    pub fn root(&self) -> &Node {
        &self.root
    }

    /// The pane that fills the tab, if the user zoomed one.
    #[must_use]
    pub fn zoomed(&self) -> Option<PaneId> {
        self.zoomed
    }
}

/// The leaf an adopt overwrote, so a failed start can put that leaf back.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HeldLeaf {
    pane: PaneId,
    session: Option<SessionId>,
    target: PaneTarget,
    mode: TerminalMode,
    mark: Option<LeafMark>,
}

/// Tabs, the active one, and a one-shot placement for the next session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLayout {
    tabs: Vec<Tab>,
    active: Option<TabId>,
    armed: Option<Armed>,
    /// Set only when the latest attach bound a session onto a leaf that was already there.
    adopted: Option<HeldLeaf>,
    notices: Vec<LayoutNotice>,
    next_split: u64,
}

impl Default for SessionLayout {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
            armed: None,
            adopted: None,
            notices: Vec::new(),
            next_split: 1,
        }
    }
}

impl SessionLayout {
    /// No tabs.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any tab is open.
    #[must_use]
    pub fn has_tabs(&self) -> bool {
        !self.tabs.is_empty()
    }

    /// Whether a leaf is waiting and has no session.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.tabs.iter().any(|tab| has_pending(&tab.root))
    }

    /// Tabs in order.
    #[must_use]
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// The tab on screen.
    #[must_use]
    pub fn active_tab_id(&self) -> Option<TabId> {
        self.active
    }

    /// The tab on screen.
    #[must_use]
    pub fn active_tab(&self) -> Option<&Tab> {
        self.tab(self.active?)
    }

    /// Stale ids from the last restore. Other tabs were kept.
    #[must_use]
    pub fn notices(&self) -> &[LayoutNotice] {
        &self.notices
    }

    /// One tab, by identity.
    #[must_use]
    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    /// The tab that holds `session`.
    ///
    /// A session is a pane of exactly one tab, so this is how a session's own name
    /// and colour are found without trusting a position in the strip.
    #[must_use]
    pub fn tab_id_of_session(&self, session: SessionId) -> Option<TabId> {
        self.tabs
            .iter()
            .find(|tab| find_session(&tab.root, session).is_some())
            .map(|tab| tab.id)
    }

    /// Names one tab, or returns it to its automatic title when given `None`.
    ///
    /// Returns whether the name changed. The panes, the sessions and the order are
    /// untouched: this is a label, not a connection. The name is trimmed and cut to the
    /// length a tab keeps by the same rule the snapshot uses
    /// ([`tab_name_in_limits`]), so a name cannot survive a save and change length on
    /// the way back.
    pub fn set_custom_title(&mut self, id: TabId, title: Option<String>) -> bool {
        let next = title
            .map(|title| tab_name_in_limits(&title))
            .filter(|title| !title.is_empty());
        match self.tab_mut(id) {
            Some(tab) if tab.custom_title != next => {
                tab.custom_title = next;
                true
            }
            _ => false,
        }
    }

    /// Chooses the colour that tells one tab from its neighbours, or clears it.
    ///
    /// Returns whether the colour changed.
    pub fn set_accent(&mut self, id: TabId, accent: Option<TabAccent>) -> bool {
        match self.tab_mut(id) {
            Some(tab) if tab.accent != accent => {
                tab.accent = accent;
                true
            }
            _ => false,
        }
    }

    fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    fn fresh_split(&mut self) -> u64 {
        let id = self.next_split;
        self.next_split = self.next_split.saturating_add(1);
        id
    }

    /// The session each tab is known by, in tab order.
    ///
    /// A tab that is only waiting has no session yet and is skipped. Callers that
    /// draw every tab read [`Self::tabs`] instead.
    #[must_use]
    pub fn tab_anchors(&self) -> Vec<SessionId> {
        self.tabs.iter().filter_map(anchor_session).collect()
    }

    /// The anchor session of the tab at `index`, when that tab has one.
    #[must_use]
    pub fn anchor_of(&self, index: usize) -> Option<SessionId> {
        self.tabs.get(index).and_then(anchor_session)
    }

    /// The session that receives input, when the active pane has been started.
    #[must_use]
    pub fn active_session(&self) -> Option<SessionId> {
        let tab = self.active_tab()?;
        let pane = tab.active.filter(|id| contains(&tab.root, *id))?;
        leaf(&tab.root, pane).and_then(|leaf| leaf.session)
    }

    /// The pane that receives input.
    #[must_use]
    pub fn active_pane(&self) -> Option<PaneId> {
        let tab = self.active_tab()?;
        tab.active.filter(|id| contains(&tab.root, *id))
    }

    /// The presentation stored for the leaf that shows `session`.
    #[must_use]
    pub fn session_mode(&self, session: SessionId) -> TerminalMode {
        self.leaves()
            .into_iter()
            .find(|leaf| leaf.session() == Some(session))
            .map(|leaf| leaf.mode())
            .unwrap_or(TerminalMode::Classic)
    }

    /// Records the presentation of the leaf that shows `session`.
    ///
    /// Returns whether the stored mode changed. The pane's grid is not touched.
    pub fn set_session_mode(&mut self, session: SessionId, mode: TerminalMode) -> bool {
        for tab in &mut self.tabs {
            if set_leaf_mode(&mut tab.root, session, mode) {
                return true;
            }
        }
        false
    }

    /// Every leaf, in tree order.
    #[must_use]
    pub fn leaves(&self) -> Vec<&Leaf> {
        let mut out = Vec::new();
        for tab in &self.tabs {
            collect_leaves(&tab.root, &mut out);
        }
        out
    }

    /// Split ratios of the active tab, outer splits first.
    #[cfg(test)]
    #[must_use]
    pub fn active_ratios(&self) -> Vec<(SplitAxis, u16)> {
        let Some(tab) = self.active_tab() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        collect_ratios(&tab.root, &mut out);
        out
    }

    /// The next session opens beside the active pane instead of in a new tab.
    ///
    /// Returns whether there is an active pane to split.
    pub fn arm_split(&mut self, axis: SplitAxis) -> bool {
        let Some(pane) = self.active_pane() else {
            return false;
        };
        self.armed = Some(Armed::Split { pane, axis });
        true
    }

    /// The next session fills a pane that is already waiting.
    pub fn arm_adopt(&mut self, pane: PaneId) {
        self.armed = Some(Armed::Adopt { pane });
    }

    /// Forgets a placement that was not used.
    pub fn clear_armed(&mut self) {
        self.armed = None;
    }

    /// Puts a session into the tree.
    ///
    /// An armed split or adopt is consumed. Otherwise the session is a new tab,
    /// which is how a new shell stays a tab of its own.
    pub fn attach(&mut self, session: SessionId, target: PaneTarget, title: &str) {
        self.adopted = None;
        match self.armed.take() {
            Some(Armed::Adopt { pane }) if self.bind(pane, session, target.clone()) => {}
            Some(Armed::Split { pane, axis })
                if self.insert_split(pane, axis, Leaf::live(session, target.clone())) => {}
            _ => self.open_tab(Some(session), target, title, TerminalMode::Classic),
        }
    }

    /// Puts an adopted leaf back and reports whether `session` was the one just bound.
    ///
    /// A start that never produced a process must not delete the pane the user
    /// already had. The leaf keeps its id, its place and the target it had.
    pub fn revert_adopted(&mut self, session: SessionId) -> bool {
        let Some(held) = self.adopted.take() else {
            return false;
        };
        restore_held(&mut self.tabs, &held, session)
    }

    /// The launch is real, so the adopted leaf is no longer waiting to be restored.
    pub fn clear_adopted(&mut self) {
        self.adopted = None;
    }

    /// Splits the active pane with a leaf that has not been started.
    pub fn split_pending(&mut self, axis: SplitAxis, target: PaneTarget) -> Option<PaneId> {
        let pane = self.active_pane()?;
        let leaf = Leaf::pending(target, TerminalMode::Classic);
        let id = leaf.id;
        self.insert_split(pane, axis, leaf).then_some(id)
    }

    /// Shows the tab at `index`.
    pub fn select_index(&mut self, index: usize) -> bool {
        let Some(id) = self.tabs.get(index).map(|tab| tab.id) else {
            return false;
        };
        self.active = Some(id);
        true
    }

    /// Swaps the active tab with the one `delta` places away. No wrap.
    pub fn reorder_active(&mut self, delta: isize) -> bool {
        let Some(current) = self
            .active
            .and_then(|id| self.tabs.iter().position(|tab| tab.id == id))
        else {
            return false;
        };
        let next = current as isize + delta;
        if next < 0 || next >= self.tabs.len() as isize {
            return false;
        }
        self.tabs.swap(current, next as usize);
        true
    }

    /// Makes `session` the pane that receives input.
    pub fn focus_session(&mut self, session: SessionId) -> bool {
        for tab in &mut self.tabs {
            if let Some(pane) = find_session(&tab.root, session) {
                tab.active = Some(pane);
                if tab.zoomed.is_some_and(|id| !contains(&tab.root, id)) {
                    tab.zoomed = None;
                }
                self.active = Some(tab.id);
                return true;
            }
        }
        false
    }

    /// Makes one pane the one that receives input.
    pub fn focus_pane(&mut self, pane: PaneId) -> bool {
        for tab in &mut self.tabs {
            if contains(&tab.root, pane) {
                tab.active = Some(pane);
                self.active = Some(tab.id);
                return true;
            }
        }
        false
    }

    /// Moves the input focus to the neighbour in `direction`.
    pub fn focus_neighbor(&mut self, direction: Direction) -> bool {
        let Some(tab_id) = self.active else {
            return false;
        };
        let Some(tab) = self.tab(tab_id) else {
            return false;
        };
        let Some(current) = tab.active else {
            return false;
        };
        let Some(next) = neighbor(&tab.root, current, direction) else {
            return false;
        };
        self.focus_pane(next)
    }

    /// Swaps the active pane with the neighbour in `direction`.
    ///
    /// Other splits keep their ratios. The input focus follows the pane that moved.
    pub fn move_active(&mut self, direction: Direction) -> bool {
        let Some(tab_id) = self.active else {
            return false;
        };
        let Some(current) = self.tab(tab_id).and_then(|tab| tab.active) else {
            return false;
        };
        let Some(tab) = self.tab(tab_id) else {
            return false;
        };
        let Some(other) = neighbor(&tab.root, current, direction) else {
            return false;
        };
        let Some(tab) = self.tab_mut(tab_id) else {
            return false;
        };
        if !swap_payload(&mut tab.root, current, other) {
            return false;
        }
        if tab.active == Some(current) {
            tab.active = Some(other);
        }
        if tab.zoomed == Some(current) {
            tab.zoomed = Some(other);
        } else if tab.zoomed == Some(other) {
            tab.zoomed = Some(current);
        }
        true
    }

    /// Grows or shrinks only the split that directly contains the active pane
    /// along `direction`. Nested splits on the other axis stay as they are.
    pub fn resize_active(&mut self, direction: Direction, step: u16) -> bool {
        if step == 0 {
            return false;
        }
        let Some(tab_id) = self.active else {
            return false;
        };
        let Some(pane) = self.tab(tab_id).and_then(|tab| tab.active) else {
            return false;
        };
        let Some(tab) = self.tab_mut(tab_id) else {
            return false;
        };
        resize_node(&mut tab.root, pane, direction, step)
    }

    /// Sets one split's ratio. `0` and `10000` are rejected.
    pub fn set_split_ratio(&mut self, split: u64, ratio: u16) -> bool {
        if !(1..=9999).contains(&ratio) {
            return false;
        }
        for tab in &mut self.tabs {
            if set_ratio(&mut tab.root, split, ratio) {
                return true;
            }
        }
        false
    }

    /// Toggles whether the active pane fills its tab.
    pub fn toggle_zoom(&mut self) -> bool {
        let Some(tab_id) = self.active else {
            return false;
        };
        let Some(pane) = self.tab(tab_id).and_then(|tab| tab.active) else {
            return false;
        };
        let Some(tab) = self.tab_mut(tab_id) else {
            return false;
        };
        if tab.zoomed == Some(pane) {
            tab.zoomed = None;
        } else {
            tab.zoomed = Some(pane);
        }
        true
    }

    /// Removes the leaf that shows `session`. A split that loses a child collapses.
    ///
    /// Returns whether a leaf was removed. Other sessions stay.
    pub fn detach_session(&mut self, session: SessionId) -> bool {
        let Some(pane) = self
            .tabs
            .iter()
            .find_map(|tab| find_session(&tab.root, session))
        else {
            return false;
        };
        self.close_pane(pane)
    }

    /// Removes one pane. The rest of its tab, and every other tab, stay.
    pub fn close_pane(&mut self, pane: PaneId) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| contains(&tab.root, pane)) else {
            return false;
        };
        let tab_id = self.tabs[index].id;
        let Some(root) = remove_pane(std::mem::replace(&mut self.tabs[index].root, hole()), pane)
        else {
            self.tabs.remove(index);
            if self.active == Some(tab_id) {
                self.active = self
                    .tabs
                    .get(index)
                    .or_else(|| self.tabs.last())
                    .map(|tab| tab.id);
            }
            return true;
        };
        let tab = &mut self.tabs[index];
        tab.root = root;
        if tab.active.is_some_and(|id| !contains(&tab.root, id)) {
            tab.active = first_pane(&tab.root);
        }
        if tab.zoomed.is_some_and(|id| !contains(&tab.root, id)) {
            tab.zoomed = None;
        }
        true
    }

    /// Removes waiting leaves from one tab. A tab that was only waiting is removed.
    ///
    /// Sessions on that tab stay, and every other tab stays.
    pub fn drop_pending_on_tab(&mut self, id: TabId) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return false;
        };
        let Some(root) = drop_pending(std::mem::replace(&mut self.tabs[index].root, hole())) else {
            self.tabs.remove(index);
            if self.active == Some(id) {
                self.active = self
                    .tabs
                    .get(index)
                    .or_else(|| self.tabs.last())
                    .map(|tab| tab.id);
            }
            return true;
        };
        let tab = &mut self.tabs[index];
        tab.root = root;
        if tab.active.is_some_and(|pane| !contains(&tab.root, pane)) {
            tab.active = first_pane(&tab.root);
        }
        if tab.zoomed.is_some_and(|pane| !contains(&tab.root, pane)) {
            tab.zoomed = None;
        }
        true
    }

    /// Removes one tab and reports the sessions it held, so the caller can stop
    /// those processes and no others.
    pub fn take_tab(&mut self, id: TabId) -> Vec<SessionId> {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return Vec::new();
        };
        let tab = self.tabs.remove(index);
        if self.active == Some(id) {
            self.active = self
                .tabs
                .get(index)
                .or_else(|| self.tabs.last())
                .map(|tab| tab.id);
        }
        sessions_of(&tab.root)
    }

    /// The snapshot of this layout.
    ///
    /// Only ids, the tree, the panel, terminal modes and titles are written.
    /// `title_of` names a live session. Passwords are not a parameter.
    ///
    /// # Errors
    ///
    /// Returns the build error when a title is blank or an id is repeated.
    pub fn capture(
        &self,
        mut title_of: impl FnMut(SessionId) -> String,
        panel: PanelConfig,
    ) -> Result<WorkspaceSnapshot, WorkspaceBuildError> {
        let mut tabs = Vec::new();
        for tab in &self.tabs {
            let title = anchor_session(tab)
                .map(&mut title_of)
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| tab.title.clone());
            let mut saved = WorkspaceTab::with_id(tab.id, title, node_to_saved(&tab.root)?)?;
            saved.set_active_pane(tab.active.filter(|id| contains(&tab.root, *id)));
            // The name and the colour travel with the tab, so the next run draws the
            // same strip. They are labels: no pane in the snapshot is started.
            saved.set_custom_title(tab.custom_title.clone());
            saved.set_accent(tab.accent);
            tabs.push(saved);
        }
        let mut snapshot = WorkspaceSnapshot::new(tabs, panel)?;
        snapshot.set_active_tab(self.active);
        Ok(snapshot)
    }

    /// Installs a restored snapshot as pending panes.
    ///
    /// Every leaf has no session. The caller must not start one from this.
    pub fn install_restored(&mut self, restored: RestoredWorkspace) {
        self.armed = None;
        self.adopted = None;
        self.notices.clear();
        let mut tabs = Vec::new();
        let mut seed = self.next_split;
        for tab in restored.tabs() {
            let root = node_from_restored(tab.root(), restored.issues(), &mut seed);
            let active = tab.active_pane().filter(|id| contains(&root, *id));
            tabs.push(Tab {
                id: tab.id(),
                title: tab.title().to_owned(),
                custom_title: tab.custom_title().map(str::to_owned),
                accent: tab.accent(),
                active: active.or_else(|| first_pane(&root)),
                zoomed: None,
                root,
            });
        }
        for issue in restored.issues() {
            match issue {
                WorkspaceIssue::DanglingActiveTab { tab, .. } => {
                    self.notices.push(LayoutNotice::DanglingTab { tab: *tab });
                }
                WorkspaceIssue::DanglingActivePane { tab, pane, .. } => {
                    self.notices.push(LayoutNotice::DanglingPane {
                        tab: *tab,
                        pane: *pane,
                    });
                }
                WorkspaceIssue::MissingHost { .. } => {}
            }
        }
        self.next_split = seed;
        self.tabs = tabs;
        self.active = restored
            .active_tab()
            .filter(|id| self.tabs.iter().any(|tab| tab.id == *id))
            .or_else(|| self.tabs.first().map(|tab| tab.id));
    }

    /// Marks remote leaves whose host is gone, and clears that mark when it returns.
    ///
    /// A leaf that is fine is not touched. Nothing is connected.
    pub fn recheck_hosts(&mut self, mut exists: impl FnMut(HostId) -> bool) {
        for tab in &mut self.tabs {
            recheck_node(&mut tab.root, &mut exists);
        }
    }

    fn bind(&mut self, pane: PaneId, session: SessionId, target: PaneTarget) -> bool {
        for tab in &mut self.tabs {
            if let Some(held) = bind_leaf(&mut tab.root, pane, session, target.clone()) {
                tab.active = Some(pane);
                self.active = Some(tab.id);
                self.adopted = Some(held);
                return true;
            }
        }
        false
    }

    fn insert_split(&mut self, pane: PaneId, axis: SplitAxis, leaf: Leaf) -> bool {
        let new_id = leaf.id;
        let split_id = self.fresh_split();
        for tab in &mut self.tabs {
            let (root, inserted) = insert_at(
                std::mem::replace(&mut tab.root, hole()),
                pane,
                axis,
                split_id,
                leaf.clone(),
            );
            tab.root = root;
            if inserted {
                tab.active = Some(new_id);
                tab.zoomed = None;
                self.active = Some(tab.id);
                return true;
            }
        }
        false
    }

    fn open_tab(
        &mut self,
        session: Option<SessionId>,
        target: PaneTarget,
        title: &str,
        mode: TerminalMode,
    ) {
        let mut leaf = if let Some(session) = session {
            Leaf::live(session, target)
        } else {
            Leaf::pending(target, mode)
        };
        leaf.mode = mode;
        let pane = leaf.id;
        let title = {
            let trimmed = title.trim();
            if trimmed.is_empty() {
                "Session"
            } else {
                trimmed
            }
        };
        let tab = Tab {
            id: TabId::new(),
            title: title.to_owned(),
            custom_title: None,
            accent: None,
            active: Some(pane),
            zoomed: None,
            root: Node::Leaf(leaf),
        };
        self.active = Some(tab.id);
        self.tabs.push(tab);
    }
}

fn hole() -> Node {
    Node::Leaf(Leaf {
        id: PaneId::new(),
        session: None,
        target: PaneTarget::local(None),
        mode: TerminalMode::Classic,
        mark: None,
    })
}

fn anchor_session(tab: &Tab) -> Option<SessionId> {
    let preferred = tab
        .active
        .filter(|id| contains(&tab.root, *id))
        .and_then(|id| leaf(&tab.root, id))
        .and_then(|leaf| leaf.session);
    preferred.or_else(|| first_session(&tab.root))
}

fn has_pending(node: &Node) -> bool {
    match node {
        Node::Leaf(leaf) => leaf.session.is_none(),
        Node::Split { first, second, .. } => has_pending(first) || has_pending(second),
    }
}

fn contains(node: &Node, id: PaneId) -> bool {
    match node {
        Node::Leaf(leaf) => leaf.id == id,
        Node::Split { first, second, .. } => contains(first, id) || contains(second, id),
    }
}

fn set_leaf_mode(node: &mut Node, session: SessionId, mode: TerminalMode) -> bool {
    match node {
        Node::Leaf(leaf) if leaf.session == Some(session) => {
            if leaf.mode == mode {
                return false;
            }
            leaf.mode = mode;
            true
        }
        Node::Leaf(_) => false,
        Node::Split { first, second, .. } => {
            set_leaf_mode(first, session, mode) || set_leaf_mode(second, session, mode)
        }
    }
}

fn leaf(node: &Node, id: PaneId) -> Option<&Leaf> {
    match node {
        Node::Leaf(leaf) if leaf.id == id => Some(leaf),
        Node::Leaf(_) => None,
        Node::Split { first, second, .. } => leaf(first, id).or_else(|| leaf(second, id)),
    }
}

fn first_pane(node: &Node) -> Option<PaneId> {
    match node {
        Node::Leaf(leaf) => Some(leaf.id),
        Node::Split { first, .. } => first_pane(first),
    }
}

fn first_session(node: &Node) -> Option<SessionId> {
    match node {
        Node::Leaf(leaf) => leaf.session,
        Node::Split { first, second, .. } => first_session(first).or_else(|| first_session(second)),
    }
}

fn find_session(node: &Node, session: SessionId) -> Option<PaneId> {
    match node {
        Node::Leaf(leaf) if leaf.session == Some(session) => Some(leaf.id),
        Node::Leaf(_) => None,
        Node::Split { first, second, .. } => {
            find_session(first, session).or_else(|| find_session(second, session))
        }
    }
}

fn sessions_of(node: &Node) -> Vec<SessionId> {
    let mut out = Vec::new();
    fn walk(node: &Node, out: &mut Vec<SessionId>) {
        match node {
            Node::Leaf(leaf) => {
                if let Some(session) = leaf.session {
                    out.push(session);
                }
            }
            Node::Split { first, second, .. } => {
                walk(first, out);
                walk(second, out);
            }
        }
    }
    walk(node, &mut out);
    out
}

fn collect_leaves<'a>(node: &'a Node, out: &mut Vec<&'a Leaf>) {
    match node {
        Node::Leaf(leaf) => out.push(leaf),
        Node::Split { first, second, .. } => {
            collect_leaves(first, out);
            collect_leaves(second, out);
        }
    }
}

#[cfg(test)]
fn collect_ratios(node: &Node, out: &mut Vec<(SplitAxis, u16)>) {
    if let Node::Split {
        axis,
        ratio,
        first,
        second,
        ..
    } = node
    {
        out.push((*axis, *ratio));
        collect_ratios(first, out);
        collect_ratios(second, out);
    }
}

fn axis_matches(axis: SplitAxis, direction: Direction) -> bool {
    match direction {
        Direction::Left | Direction::Right => axis == SplitAxis::LeftRight,
        Direction::Up | Direction::Down => axis == SplitAxis::TopBottom,
    }
}

fn points_second(direction: Direction) -> bool {
    matches!(direction, Direction::Right | Direction::Down)
}

enum Locate {
    Outside,
    Inside,
    Found(PaneId),
}

fn neighbor(node: &Node, id: PaneId, direction: Direction) -> Option<PaneId> {
    match locate(node, id, direction) {
        Locate::Found(pane) => Some(pane),
        Locate::Inside | Locate::Outside => None,
    }
}

fn locate(node: &Node, id: PaneId, direction: Direction) -> Locate {
    match node {
        Node::Leaf(leaf) => {
            if leaf.id == id {
                Locate::Inside
            } else {
                Locate::Outside
            }
        }
        Node::Split {
            axis,
            first,
            second,
            ..
        } => match locate(first, id, direction) {
            Locate::Found(pane) => Locate::Found(pane),
            Locate::Inside => {
                if axis_matches(*axis, direction) && points_second(direction) {
                    Locate::Found(entry_edge(second, direction))
                } else {
                    Locate::Inside
                }
            }
            Locate::Outside => match locate(second, id, direction) {
                Locate::Found(pane) => Locate::Found(pane),
                Locate::Inside => {
                    if axis_matches(*axis, direction) && !points_second(direction) {
                        Locate::Found(entry_edge(first, direction))
                    } else {
                        Locate::Inside
                    }
                }
                Locate::Outside => Locate::Outside,
            },
        },
    }
}

/// The leaf a move into `node` meets first.
fn entry_edge(node: &Node, direction: Direction) -> PaneId {
    match node {
        Node::Leaf(leaf) => leaf.id,
        Node::Split {
            axis,
            first,
            second,
            ..
        } => {
            if axis_matches(*axis, direction) {
                if points_second(direction) {
                    entry_edge(first, direction)
                } else {
                    entry_edge(second, direction)
                }
            } else {
                entry_edge(first, direction)
            }
        }
    }
}

fn clone_leaf(node: &Node, id: PaneId) -> Option<Leaf> {
    leaf(node, id).cloned()
}

fn replace_leaf(node: &mut Node, id: PaneId, next: Leaf) -> Result<Leaf, Leaf> {
    match node {
        Node::Leaf(leaf) if leaf.id == id => Ok(std::mem::replace(leaf, next)),
        Node::Leaf(_) => Err(next),
        Node::Split { first, second, .. } => match replace_leaf(first, id, next) {
            Ok(old) => Ok(old),
            Err(next) => replace_leaf(second, id, next),
        },
    }
}

/// Exchanges the contents of two leaves and leaves each id in its slot.
fn swap_payload(node: &mut Node, a: PaneId, b: PaneId) -> bool {
    let Some(mut content_a) = clone_leaf(node, a) else {
        return false;
    };
    let Some(mut content_b) = clone_leaf(node, b) else {
        return false;
    };
    content_a.id = b;
    content_b.id = a;
    if replace_leaf(node, a, content_b).is_err() {
        return false;
    }
    replace_leaf(node, b, content_a).is_ok()
}

fn resize_node(node: &mut Node, pane: PaneId, direction: Direction, step: u16) -> bool {
    let Node::Split {
        axis,
        ratio,
        first,
        second,
        ..
    } = node
    else {
        return false;
    };
    let in_first = contains(first, pane);
    let in_second = contains(second, pane);
    if !in_first && !in_second {
        return false;
    }
    // Only the split that directly holds the leaf moves. An ancestor on the
    // other axis stays put when this direction does not match the parent.
    let direct = match (in_first, first.as_ref(), second.as_ref()) {
        (true, Node::Leaf(leaf), _) => leaf.id == pane,
        (false, _, Node::Leaf(leaf)) => leaf.id == pane,
        _ => false,
    };
    if !direct {
        let child = if in_first {
            first.as_mut()
        } else {
            second.as_mut()
        };
        return resize_node(child, pane, direction, step);
    }
    if !axis_matches(*axis, direction) {
        return false;
    }
    let increase = match direction {
        Direction::Right | Direction::Down => in_first,
        Direction::Left | Direction::Up => in_second,
    };
    let next = if increase {
        ratio.saturating_add(step)
    } else {
        ratio.saturating_sub(step)
    };
    let next = next.clamp(1, 9999);
    if next == *ratio {
        return false;
    }
    *ratio = next;
    true
}

fn set_ratio(node: &mut Node, split: u64, ratio: u16) -> bool {
    match node {
        Node::Leaf(_) => false,
        Node::Split {
            id,
            ratio: slot,
            first,
            second,
            ..
        } => {
            if *id == split {
                *slot = ratio;
                return true;
            }
            set_ratio(first, split, ratio) || set_ratio(second, split, ratio)
        }
    }
}

fn remove_pane(node: Node, pane: PaneId) -> Option<Node> {
    match node {
        Node::Leaf(leaf) if leaf.id == pane => None,
        Node::Leaf(leaf) => Some(Node::Leaf(leaf)),
        Node::Split {
            id,
            axis,
            ratio,
            first,
            second,
        } => {
            let first = remove_pane(*first, pane);
            let second = remove_pane(*second, pane);
            match (first, second) {
                (None, None) => None,
                (Some(only), None) | (None, Some(only)) => Some(only),
                (Some(first), Some(second)) => Some(Node::Split {
                    id,
                    axis,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
            }
        }
    }
}

fn insert_at(node: Node, pane: PaneId, axis: SplitAxis, split: u64, leaf: Leaf) -> (Node, bool) {
    match node {
        Node::Leaf(existing) if existing.id == pane => (
            Node::Split {
                id: split,
                axis,
                ratio: 5000,
                first: Box::new(Node::Leaf(existing)),
                second: Box::new(Node::Leaf(leaf)),
            },
            true,
        ),
        Node::Leaf(existing) => (Node::Leaf(existing), false),
        Node::Split {
            id,
            axis: split_axis,
            ratio,
            first,
            second,
        } => {
            if contains(&first, pane) {
                let (first, inserted) = insert_at(*first, pane, axis, split, leaf);
                (
                    Node::Split {
                        id,
                        axis: split_axis,
                        ratio,
                        first: Box::new(first),
                        second,
                    },
                    inserted,
                )
            } else if contains(&second, pane) {
                let (second, inserted) = insert_at(*second, pane, axis, split, leaf);
                (
                    Node::Split {
                        id,
                        axis: split_axis,
                        ratio,
                        first,
                        second: Box::new(second),
                    },
                    inserted,
                )
            } else {
                (
                    Node::Split {
                        id,
                        axis: split_axis,
                        ratio,
                        first,
                        second,
                    },
                    false,
                )
            }
        }
    }
}

fn bind_leaf(
    node: &mut Node,
    pane: PaneId,
    session: SessionId,
    target: PaneTarget,
) -> Option<HeldLeaf> {
    match node {
        Node::Leaf(leaf) if leaf.id == pane => {
            let held = HeldLeaf {
                pane,
                session: leaf.session,
                target: leaf.target.clone(),
                mode: leaf.mode,
                mark: leaf.mark.clone(),
            };
            leaf.session = Some(session);
            leaf.target = target;
            leaf.mark = None;
            Some(held)
        }
        Node::Leaf(_) => None,
        Node::Split { first, second, .. } => bind_leaf(first, pane, session, target.clone())
            .or_else(|| bind_leaf(second, pane, session, target)),
    }
}

fn restore_held(tabs: &mut [Tab], held: &HeldLeaf, session: SessionId) -> bool {
    for tab in tabs {
        if restore_held_node(&mut tab.root, held, session) {
            if tab.active.is_some_and(|pane| !contains(&tab.root, pane)) {
                tab.active = Some(held.pane);
            }
            return true;
        }
    }
    false
}

fn restore_held_node(node: &mut Node, held: &HeldLeaf, session: SessionId) -> bool {
    match node {
        Node::Leaf(leaf) if leaf.id == held.pane && leaf.session == Some(session) => {
            leaf.session = held.session;
            leaf.target = held.target.clone();
            leaf.mode = held.mode;
            leaf.mark = held.mark.clone();
            true
        }
        Node::Leaf(_) => false,
        Node::Split { first, second, .. } => {
            restore_held_node(first, held, session) || restore_held_node(second, held, session)
        }
    }
}

fn drop_pending(node: Node) -> Option<Node> {
    match node {
        Node::Leaf(leaf) if leaf.session.is_none() => None,
        Node::Leaf(leaf) => Some(Node::Leaf(leaf)),
        Node::Split {
            id,
            axis,
            ratio,
            first,
            second,
        } => {
            let first = drop_pending(*first);
            let second = drop_pending(*second);
            match (first, second) {
                (None, None) => None,
                (Some(only), None) | (None, Some(only)) => Some(only),
                (Some(first), Some(second)) => Some(Node::Split {
                    id,
                    axis,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
            }
        }
    }
}

fn node_to_saved(node: &Node) -> Result<PaneNode, WorkspaceBuildError> {
    match node {
        Node::Leaf(leaf) => Ok(PaneNode::leaf_with_id(
            leaf.id,
            leaf.target.clone(),
            leaf.mode,
        )),
        Node::Split {
            axis,
            ratio,
            first,
            second,
            ..
        } => PaneNode::split(*axis, *ratio, node_to_saved(first)?, node_to_saved(second)?),
    }
}

fn node_from_restored(
    node: &RestoredNode,
    issues: &[WorkspaceIssue],
    next_split: &mut u64,
) -> Node {
    match node {
        RestoredNode::Leaf(pane) => {
            let (target, mark) = match pane.connection() {
                PendingConnection::Local { shell } => (PaneTarget::local(shell.clone()), None),
                PendingConnection::Remote { host } => {
                    let missing = issues.iter().any(|issue| {
                        matches!(
                            issue,
                            WorkspaceIssue::MissingHost { pane: id, .. } if *id == pane.id()
                        )
                    });
                    (
                        PaneTarget::remote(*host),
                        missing.then_some(LeafMark::MissingHost { host: *host }),
                    )
                }
            };
            // A restored leaf has no session. `is_started` on the connection is
            // always false; this tree does not grow a process to disagree with it.
            debug_assert!(!pane.connection().is_started());
            Node::Leaf(Leaf {
                id: pane.id(),
                session: None,
                target,
                mode: pane.mode(),
                mark,
            })
        }
        RestoredNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let id = *next_split;
            *next_split = next_split.saturating_add(1);
            let first = node_from_restored(first, issues, next_split);
            let second = node_from_restored(second, issues, next_split);
            Node::Split {
                id,
                axis: *axis,
                ratio: ratio.get(),
                first: Box::new(first),
                second: Box::new(second),
            }
        }
    }
}

fn recheck_node(node: &mut Node, exists: &mut impl FnMut(HostId) -> bool) {
    match node {
        Node::Leaf(leaf) => {
            if let Some(host) = leaf.target.host() {
                if exists(host) {
                    if matches!(leaf.mark, Some(LeafMark::MissingHost { .. })) {
                        leaf.mark = None;
                    }
                } else {
                    leaf.mark = Some(LeafMark::MissingHost { host });
                }
            }
        }
        Node::Split { first, second, .. } => {
            recheck_node(first, exists);
            recheck_node(second, exists);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_leaf(layout: &mut SessionLayout, title: &str) -> (SessionId, PaneId) {
        let session = SessionId::new();
        layout.attach(session, PaneTarget::local(Some("pwsh".to_owned())), title);
        let pane = layout.active_pane().expect("the new tab has a pane");
        (session, pane)
    }

    #[test]
    fn nested_resize_changes_only_the_split_around_the_active_pane() {
        let mut layout = SessionLayout::new();
        let (outer, _) = session_leaf(&mut layout, "A");
        assert!(layout.arm_split(SplitAxis::LeftRight));
        let (_right, _) = session_leaf(&mut layout, "B");
        assert_eq!(layout.tabs().len(), 1, "a split stays on the same tab");
        assert!(layout.arm_split(SplitAxis::TopBottom));
        let (lower, lower_pane) = session_leaf(&mut layout, "C");
        assert_eq!(layout.tabs().len(), 1);
        assert_eq!(layout.active_session(), Some(lower));

        let before = layout.active_ratios();
        assert_eq!(before.len(), 2);
        assert!(layout.resize_active(Direction::Down, 800));
        let after_vertical = layout.active_ratios();
        assert_eq!(after_vertical[0], before[0], "the outer split did not move");
        assert_ne!(after_vertical[1], before[1], "the inner split grew");

        assert!(layout.focus_session(outer));
        let focused = layout.active_ratios();
        assert!(layout.resize_active(Direction::Right, 600));
        let after_horizontal = layout.active_ratios();
        assert_ne!(
            after_horizontal[0], focused[0],
            "the outer split follows the active pane"
        );
        assert_eq!(
            after_horizontal[1], focused[1],
            "the nested split is not resized with it"
        );

        assert!(layout.focus_pane(lower_pane));
        assert_eq!(layout.active_session(), Some(lower));
        assert!(!layout.resize_active(Direction::Right, 400));
        assert_eq!(
            layout.active_ratios(),
            after_horizontal,
            "a direction the active pane does not own changes nothing"
        );
    }

    #[test]
    fn closing_one_pane_keeps_the_other_and_moving_swaps_only_those_two() {
        let mut layout = SessionLayout::new();
        let (first, first_pane) = session_leaf(&mut layout, "A");
        assert!(layout.arm_split(SplitAxis::LeftRight));
        let (second, _) = session_leaf(&mut layout, "B");
        assert!(layout.arm_split(SplitAxis::TopBottom));
        let (third, _) = session_leaf(&mut layout, "C");

        assert!(layout.move_active(Direction::Up));
        assert_eq!(layout.tabs().len(), 1);
        assert_eq!(layout.active_ratios().len(), 2, "the tree shape stays");
        let sessions = sessions_of(&layout.active_tab().expect("tab").root);
        assert!(
            sessions.contains(&first) && sessions.contains(&second) && sessions.contains(&third)
        );

        assert!(
            layout.close_pane(
                layout
                    .active_pane()
                    .expect("a pane is still active after the move")
            )
        );
        let left = sessions_of(&layout.active_tab().expect("tab").root);
        assert_eq!(left.len(), 2, "one pane closed");
        assert!(left.contains(&first), "the pane that was not closed stays");
        assert_ne!(layout.active_session(), None);

        assert!(layout.close_pane(first_pane));
        let last = sessions_of(&layout.active_tab().expect("one tab remains").root);
        assert_eq!(last.len(), 1);
        assert!(!last.contains(&first));
        layout.close_pane(layout.active_pane().expect("last pane"));
        assert!(!layout.has_tabs());
        assert!(layout.active_session().is_none());
    }

    #[test]
    fn a_named_and_tinted_tab_saves_and_restores_as_a_pending_tab() {
        let mut layout = SessionLayout::new();
        let (first, _) = session_leaf(&mut layout, "pwsh");
        let (second, _) = session_leaf(&mut layout, "pwsh");
        let first_tab = layout.tab_id_of_session(first).expect("the first tab");
        let second_tab = layout.tab_id_of_session(second).expect("the second tab");
        assert_ne!(first_tab, second_tab);

        assert!(layout.set_custom_title(first_tab, Some("  deploy box  ".to_owned())));
        assert!(layout.set_accent(first_tab, Some(TabAccent::Teal)));
        assert!(!layout.set_custom_title(first_tab, Some("deploy box".to_owned())));
        assert_eq!(
            layout.tab_id_of_session(second),
            Some(second_tab),
            "the other tab is untouched"
        );
        assert_eq!(layout.tab(second_tab).expect("tab").custom_title(), None);
        assert_eq!(layout.tab(second_tab).expect("tab").accent(), None);
        assert_eq!(
            layout.tab(first_tab).expect("tab").display_title(),
            "deploy box"
        );
        assert_eq!(
            layout.tab(first_tab).expect("tab").title(),
            "pwsh",
            "the automatic title is kept beside the name"
        );

        let panel = PanelConfig::new(false, shellune_core::PanelSection::None);
        let saved = layout
            .capture(|_| "pwsh".to_owned(), panel)
            .expect("snapshot");
        assert_eq!(
            saved.tabs()[0].custom_title(),
            Some("deploy box"),
            "the name is saved"
        );
        assert_eq!(saved.tabs()[0].accent(), Some(TabAccent::Teal));
        assert_eq!(saved.tabs()[0].title(), "pwsh");

        let mut restored_layout = SessionLayout::new();
        restored_layout.install_restored(saved.restore(|_| true));
        let restored_tabs = restored_layout.tabs();
        assert_eq!(restored_tabs.len(), 2, "the order is the saved order");
        assert_eq!(restored_tabs[0].id(), first_tab);
        assert_eq!(restored_tabs[0].display_title(), "deploy box");
        assert_eq!(restored_tabs[0].accent(), Some(TabAccent::Teal));
        assert_eq!(
            restored_tabs[1].display_title(),
            "pwsh",
            "a tab without a name falls back to the automatic one"
        );
        assert!(
            restored_layout
                .leaves()
                .iter()
                .all(|leaf| leaf.is_pending()),
            "a restored tab waits for the user"
        );

        // Restoring the automatic title clears only the name.
        assert!(restored_layout.set_custom_title(first_tab, None));
        let tab = restored_layout.tab(first_tab).expect("tab");
        assert_eq!(tab.custom_title(), None);
        assert_eq!(tab.display_title(), "pwsh");
        assert_eq!(tab.accent(), Some(TabAccent::Teal), "the colour stays");
        assert!(restored_layout.set_accent(first_tab, None));
        assert_eq!(restored_layout.tab(first_tab).expect("tab").accent(), None);
    }

    #[test]
    fn closing_a_named_tab_removes_its_name_with_it() {
        let mut layout = SessionLayout::new();
        let (first, _) = session_leaf(&mut layout, "one");
        let (second, _) = session_leaf(&mut layout, "two");
        let first_tab = layout.tab_id_of_session(first).expect("tab");
        let second_tab = layout.tab_id_of_session(second).expect("tab");
        layout.set_custom_title(first_tab, Some("named".to_owned()));

        assert!(layout.take_tab(first_tab).contains(&first));
        assert!(
            layout.tab(first_tab).is_none(),
            "the tab and its name are gone"
        );
        assert_eq!(
            layout
                .tab(second_tab)
                .expect("the other tab")
                .display_title(),
            "two"
        );
        assert!(!layout.set_custom_title(first_tab, Some("late".to_owned())));
        assert!(!layout.set_accent(first_tab, Some(TabAccent::Clay)));
    }

    #[test]
    fn restore_is_pending_and_a_missing_host_marks_only_that_leaf() {
        let host = HostId::new();
        let gone = HostId::new();
        let root = PaneNode::split(
            SplitAxis::LeftRight,
            5000,
            PaneNode::leaf(
                PaneTarget::local(Some("pwsh".to_owned())),
                TerminalMode::Classic,
            ),
            PaneNode::split(
                SplitAxis::TopBottom,
                2500,
                PaneNode::leaf(PaneTarget::remote(host), TerminalMode::Integrated),
                PaneNode::leaf(PaneTarget::remote(gone), TerminalMode::Classic),
            )
            .expect("inner"),
        )
        .expect("outer");
        let mut tab = WorkspaceTab::new("Work", root).expect("title");
        let stale = PaneId::new();
        tab.set_active_pane(Some(stale));
        let mut snapshot = WorkspaceSnapshot::new(
            vec![tab],
            PanelConfig::new(true, shellune_core::PanelSection::Files),
        )
        .expect("snapshot");
        let stale_tab = TabId::new();
        snapshot.set_active_tab(Some(stale_tab));

        let restored = snapshot.restore(|id| id == host);
        let mut layout = SessionLayout::new();
        layout.install_restored(restored);
        assert_eq!(layout.tabs().len(), 1, "the workspace was not dropped");
        let leaves = layout.leaves();
        assert_eq!(leaves.len(), 3);
        assert!(leaves.iter().all(|leaf| leaf.session().is_none()));
        assert!(leaves.iter().all(|leaf| leaf.is_pending()));
        let marked: Vec<_> = leaves.iter().filter(|leaf| leaf.mark().is_some()).collect();
        assert_eq!(marked.len(), 1, "only the missing host is marked");
        assert!(matches!(
            marked[0].mark(),
            Some(LeafMark::MissingHost { host: id }) if *id == gone
        ));
        assert!(layout.notices().iter().any(|notice| {
            matches!(notice, LayoutNotice::DanglingTab { tab } if *tab == stale_tab)
        }));
        assert!(layout.notices().iter().any(|notice| {
            matches!(notice, LayoutNotice::DanglingPane { pane, .. } if *pane == stale)
        }));

        let secret = "hunter2-layout";
        let panel = PanelConfig::new(false, shellune_core::PanelSection::None);
        let saved = layout
            .capture(|_| secret.to_owned(), panel)
            .expect("snapshot");
        let rendered = serde_json::to_string(&saved).expect("json");
        assert!(!rendered.contains(secret));
        assert!(!rendered.to_lowercase().contains("password"));
        assert!(!rendered.contains("socket"));
        assert_eq!(saved.tabs()[0].id(), layout.tabs()[0].id());
        let saved_panes = saved.tabs()[0].root();
        let live_ids: Vec<_> = layout.leaves().iter().map(|leaf| leaf.id()).collect();
        fn ids(node: &PaneNode, out: &mut Vec<PaneId>) {
            match node {
                PaneNode::Leaf { pane } => out.push(pane.id()),
                PaneNode::Split { first, second, .. } => {
                    ids(first, out);
                    ids(second, out);
                }
            }
        }
        let mut round = Vec::new();
        ids(saved_panes, &mut round);
        assert_eq!(round, live_ids, "a save keeps the pane ids");
    }
}
