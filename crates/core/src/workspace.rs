//! Saved workspace layout (R4).
//!
//! A snapshot records tabs, the pane tree and the context panel. Restoring it
//! builds pending panes only: a remote pane remembers which host it belongs to,
//! and a local pane remembers which shell it would start. No process is spawned,
//! no socket is opened and no forwarding rule is started. A pane whose host was
//! deleted stays in the tree as a local problem so the rest of the workspace can
//! still be repaired.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::ids::{HostId, PaneId, TabId};

/// The format this build writes for a workspace snapshot.
pub const WORKSPACE_FORMAT_VERSION: u32 = 1;

/// Classic grid, or the optional shell-integration presentation.
///
/// The mode is a preference. Restoring it does not replay terminal contents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalMode {
    /// The plain terminal.
    #[default]
    Classic,
    /// Command blocks, when the shell can support them. Still not a live session.
    Integrated,
}

/// How a split divides its two children.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitAxis {
    /// The children sit side by side.
    LeftRight,
    /// The children sit one above the other.
    TopBottom,
}

/// Share of a split given to the first child, in ten-thousandths.
///
/// An integer keeps the snapshot comparable and exact. `1` and `9999` are the
/// extremes; `0` and `10000` would hide a child, so they are rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct SplitRatio(u16);

impl SplitRatio {
    /// Validates a share.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceBuildError::RatioOutOfRange`] when `value` is `0` or
    /// greater than `9999`.
    pub const fn new(value: u16) -> Result<Self, WorkspaceBuildError> {
        if value >= 1 && value <= 9999 {
            Ok(Self(value))
        } else {
            Err(WorkspaceBuildError::RatioOutOfRange { value })
        }
    }

    /// The stored share.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// The first child's fraction of the split, in `0.0..=1.0` exclusive of the ends.
    #[must_use]
    pub fn first_fraction(self) -> f64 {
        f64::from(self.0) / 10_000.0
    }
}

impl TryFrom<u16> for SplitRatio {
    type Error = WorkspaceBuildError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SplitRatio> for u16 {
    fn from(ratio: SplitRatio) -> Self {
        ratio.0
    }
}

/// What a pane will connect to once the user asks. Not a running session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PaneTarget {
    /// A local shell that has not been started.
    Local {
        /// Shell executable, or `None` for the platform default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shell: Option<String>,
    },
    /// A saved host that has not been dialed.
    Remote {
        /// The host the pane belongs to.
        host: HostId,
    },
}

impl PaneTarget {
    /// A local shell. A blank path means the platform default.
    #[must_use]
    pub fn local(shell: Option<String>) -> Self {
        let shell = shell
            .map(|shell| shell.trim().to_owned())
            .filter(|shell| !shell.is_empty());
        Self::Local { shell }
    }

    /// A remote host. The id is stored; no connection is opened.
    #[must_use]
    pub fn remote(host: HostId) -> Self {
        Self::Remote { host }
    }

    /// The shell path when this pane is local.
    #[must_use]
    pub fn shell(&self) -> Option<&str> {
        match self {
            Self::Local { shell } => shell.as_deref(),
            Self::Remote { .. } => None,
        }
    }

    /// The host id when this pane is remote.
    #[must_use]
    pub fn host(&self) -> Option<HostId> {
        match self {
            Self::Remote { host } => Some(*host),
            Self::Local { .. } => None,
        }
    }
}

/// One leaf pane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneLeaf {
    id: PaneId,
    mode: TerminalMode,
    target: PaneTarget,
}

impl PaneLeaf {
    /// Stable id inside the snapshot.
    #[must_use]
    pub fn id(&self) -> PaneId {
        self.id
    }

    /// Terminal presentation to restore. Not live output.
    #[must_use]
    pub fn mode(&self) -> TerminalMode {
        self.mode
    }

    /// What a later, explicit connect would open.
    #[must_use]
    pub fn target(&self) -> &PaneTarget {
        &self.target
    }
}

/// A pane tree: a leaf, or a split of two trees.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "node", rename_all = "snake_case", deny_unknown_fields)]
pub enum PaneNode {
    /// A single pane.
    Leaf {
        /// The pane.
        pane: PaneLeaf,
    },
    /// Two children.
    Split {
        /// Which way the children are arranged.
        axis: SplitAxis,
        /// Share of the first child.
        ratio: SplitRatio,
        /// The first child.
        first: Box<PaneNode>,
        /// The second child.
        second: Box<PaneNode>,
    },
}

impl PaneNode {
    /// A leaf pane with a fresh id.
    #[must_use]
    pub fn leaf(target: PaneTarget, mode: TerminalMode) -> Self {
        Self::Leaf {
            pane: PaneLeaf {
                id: PaneId::new(),
                mode,
                target,
            },
        }
    }

    /// A split.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceBuildError::RatioOutOfRange`] when `ratio` is not in `1..=9999`.
    pub fn split(
        axis: SplitAxis,
        ratio: u16,
        first: Self,
        second: Self,
    ) -> Result<Self, WorkspaceBuildError> {
        Ok(Self::Split {
            axis,
            ratio: SplitRatio::new(ratio)?,
            first: Box::new(first),
            second: Box::new(second),
        })
    }

    fn contains(&self, id: PaneId) -> bool {
        match self {
            Self::Leaf { pane } => pane.id == id,
            Self::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }
}

/// Which context panel is selected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelSection {
    /// No section selected.
    #[default]
    None,
    /// Saved hosts.
    Hosts,
    /// File browser.
    Files,
    /// Transfers.
    Transfers,
    /// Forwarding rules.
    Forwards,
}

/// The context panel as it was arranged. Visibility is not a running task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelConfig {
    visible: bool,
    section: PanelSection,
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            visible: false,
            section: PanelSection::None,
        }
    }
}

impl PanelConfig {
    /// A panel arrangement.
    #[must_use]
    pub fn new(visible: bool, section: PanelSection) -> Self {
        Self { visible, section }
    }

    /// Whether the panel was shown.
    #[must_use]
    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Which section was selected.
    #[must_use]
    pub fn section(&self) -> PanelSection {
        self.section
    }
}

/// One tab and its pane tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTab {
    id: TabId,
    title: String,
    root: PaneNode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_pane: Option<PaneId>,
}

impl WorkspaceTab {
    /// A tab. The title is trimmed.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceBuildError::EmptyTitle`] when the title is blank.
    pub fn new(title: impl Into<String>, root: PaneNode) -> Result<Self, WorkspaceBuildError> {
        let title = title.into().trim().to_owned();
        if title.is_empty() {
            return Err(WorkspaceBuildError::EmptyTitle);
        }
        Ok(Self {
            id: TabId::new(),
            title,
            root,
            active_pane: None,
        })
    }

    /// Stable id.
    #[must_use]
    pub fn id(&self) -> TabId {
        self.id
    }

    /// Tab title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Pane tree.
    #[must_use]
    pub fn root(&self) -> &PaneNode {
        &self.root
    }

    /// The pane that was active, when one was.
    ///
    /// The id is not checked here. A stale id is reported by
    /// [`WorkspaceSnapshot::restore`] and does not discard the tab.
    #[must_use]
    pub fn active_pane(&self) -> Option<PaneId> {
        self.active_pane
    }

    /// Records the active pane. Passing an unknown id keeps the edit so it can be repaired.
    pub fn set_active_pane(&mut self, pane: Option<PaneId>) {
        self.active_pane = pane;
    }
}

/// Why a snapshot could not be built.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceBuildError {
    /// The title is empty or only whitespace.
    #[error(
        "a tab title cannot be empty; give the tab a name or remove it. Nothing was connected."
    )]
    EmptyTitle,
    /// The split would hide one of its children.
    #[error(
        "split ratio {value} is outside 1-9999; choose a share for the first pane. Nothing was connected."
    )]
    RatioOutOfRange {
        /// The rejected share.
        value: u16,
    },
    /// Two tabs use one id.
    #[error(
        "two tabs share the id {id}. Remove the duplicate. The workspace was not restored. Nothing was connected."
    )]
    DuplicateTab {
        /// The id that collided.
        id: TabId,
    },
    /// Two panes use one id.
    #[error(
        "two panes share the id {id}. Remove the duplicate. The workspace was not restored. Nothing was connected."
    )]
    DuplicatePane {
        /// The id that collided.
        id: PaneId,
    },
}

/// A versioned workspace snapshot.
///
/// The snapshot is the layout and the references. It is not a set of processes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSnapshot {
    version: u32,
    tabs: Vec<WorkspaceTab>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_tab: Option<TabId>,
    panel: PanelConfig,
}

impl Default for WorkspaceSnapshot {
    fn default() -> Self {
        Self {
            version: WORKSPACE_FORMAT_VERSION,
            tabs: Vec::new(),
            active_tab: None,
            panel: PanelConfig::default(),
        }
    }
}

impl WorkspaceSnapshot {
    /// A snapshot at the current format version.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceBuildError`] when two tabs or two panes share an id, or a
    /// title is blank.
    pub fn new(tabs: Vec<WorkspaceTab>, panel: PanelConfig) -> Result<Self, WorkspaceBuildError> {
        let snapshot = Self {
            version: WORKSPACE_FORMAT_VERSION,
            tabs,
            active_tab: None,
            panel,
        };
        snapshot.validate_shape()?;
        Ok(snapshot)
    }

    /// Format version.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Tabs in saved order.
    #[must_use]
    pub fn tabs(&self) -> &[WorkspaceTab] {
        &self.tabs
    }

    /// The tab that was active, when one was.
    #[must_use]
    pub fn active_tab(&self) -> Option<TabId> {
        self.active_tab
    }

    /// Records the active tab. An unknown id is kept and reported on restore.
    pub fn set_active_tab(&mut self, tab: Option<TabId>) {
        self.active_tab = tab;
    }

    /// Context panel arrangement.
    #[must_use]
    pub fn panel(&self) -> &PanelConfig {
        &self.panel
    }

    /// Checks the document structure.
    ///
    /// A missing host, or an active tab or pane that is not in the tree, is not
    /// structural damage. Those are reported by [`Self::restore`] so the rest of
    /// the snapshot stays usable.
    ///
    /// # Errors
    ///
    /// Returns the reason the document cannot be read: a version this build does
    /// not write, an empty title, or a repeated tab or pane id.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != WORKSPACE_FORMAT_VERSION {
            return Err(format!(
                "format version {} is not the version this build writes ({WORKSPACE_FORMAT_VERSION}). The snapshot was not restored and nothing was connected.",
                self.version
            ));
        }
        self.validate_shape().map_err(|error| error.to_string())
    }

    fn validate_shape(&self) -> Result<(), WorkspaceBuildError> {
        let mut tabs = HashSet::new();
        let mut panes = HashSet::new();
        for tab in &self.tabs {
            if tab.title.trim().is_empty() {
                return Err(WorkspaceBuildError::EmptyTitle);
            }
            if !tabs.insert(tab.id) {
                return Err(WorkspaceBuildError::DuplicateTab { id: tab.id });
            }
            collect_panes(&tab.root, &mut panes)?;
        }
        Ok(())
    }

    /// Builds the pending workspace described by this snapshot.
    ///
    /// `host_exists` is only asked whether a host id is still saved. The function
    /// does not receive credentials, commands or a network, and every pane it
    /// returns is waiting rather than connected.
    #[must_use]
    pub fn restore(&self, mut host_exists: impl FnMut(HostId) -> bool) -> RestoredWorkspace {
        let mut issues = Vec::new();
        let active_tab = match self.active_tab {
            Some(id) if self.tabs.iter().any(|tab| tab.id == id) => Some(id),
            Some(id) => {
                issues.push(WorkspaceIssue::DanglingActiveTab {
                    tab: id,
                    message: format!(
                        "The active tab {id} is not in this workspace. Choose one of the saved tabs. The other tabs were kept. Nothing was connected."
                    ),
                });
                None
            }
            None => None,
        };
        let tabs = self
            .tabs
            .iter()
            .map(|tab| restore_tab(tab, &mut host_exists, &mut issues))
            .collect();
        RestoredWorkspace {
            tabs,
            active_tab,
            panel: self.panel.clone(),
            issues,
        }
    }
}

fn collect_panes(node: &PaneNode, panes: &mut HashSet<PaneId>) -> Result<(), WorkspaceBuildError> {
    match node {
        PaneNode::Leaf { pane } => {
            if !panes.insert(pane.id) {
                return Err(WorkspaceBuildError::DuplicatePane { id: pane.id });
            }
            Ok(())
        }
        PaneNode::Split { first, second, .. } => {
            collect_panes(first, panes)?;
            collect_panes(second, panes)
        }
    }
}

fn restore_tab(
    tab: &WorkspaceTab,
    host_exists: &mut impl FnMut(HostId) -> bool,
    issues: &mut Vec<WorkspaceIssue>,
) -> RestoredTab {
    let active_pane = match tab.active_pane {
        Some(id) if tab.root.contains(id) => Some(id),
        Some(id) => {
            issues.push(WorkspaceIssue::DanglingActivePane {
                tab: tab.id,
                pane: id,
                message: format!(
                    "The active pane {id} is not on tab \"{}\". Choose a pane in that tab. The layout was kept. Nothing was connected.",
                    tab.title
                ),
            });
            None
        }
        None => None,
    };
    RestoredTab {
        id: tab.id,
        title: tab.title.clone(),
        root: restore_node(&tab.root, tab, host_exists, issues),
        active_pane,
    }
}

fn restore_node(
    node: &PaneNode,
    tab: &WorkspaceTab,
    host_exists: &mut impl FnMut(HostId) -> bool,
    issues: &mut Vec<WorkspaceIssue>,
) -> RestoredNode {
    match node {
        PaneNode::Leaf { pane } => {
            let connection = match &pane.target {
                PaneTarget::Local { shell } => PendingConnection::Local {
                    shell: shell
                        .as_ref()
                        .map(|shell| shell.trim().to_owned())
                        .filter(|shell| !shell.is_empty()),
                },
                PaneTarget::Remote { host } => {
                    if !host_exists(*host) {
                        issues.push(WorkspaceIssue::MissingHost {
                            tab: tab.id,
                            pane: pane.id,
                            host: *host,
                            message: format!(
                                "Pane {} on tab \"{}\" references host {host}, which is not saved. Fix the host or remove the pane. Nothing was connected.",
                                pane.id, tab.title
                            ),
                        });
                    }
                    PendingConnection::Remote { host: *host }
                }
            };
            RestoredNode::Leaf(PendingPane {
                id: pane.id,
                mode: pane.mode,
                connection,
            })
        }
        PaneNode::Split {
            axis,
            ratio,
            first,
            second,
        } => RestoredNode::Split {
            axis: *axis,
            ratio: *ratio,
            first: Box::new(restore_node(first, tab, host_exists, issues)),
            second: Box::new(restore_node(second, tab, host_exists, issues)),
        },
    }
}

/// A problem that belongs to one tab or pane. The snapshot is otherwise intact.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceIssue {
    /// A remote pane names a host that is not saved.
    #[error("{message}")]
    MissingHost {
        /// The tab the pane is on.
        tab: TabId,
        /// The pane that needs a host.
        pane: PaneId,
        /// The host id that is not saved.
        host: HostId,
        /// What to do next.
        message: String,
    },
    /// The active tab id is not in the snapshot.
    #[error("{message}")]
    DanglingActiveTab {
        /// The id that was stored.
        tab: TabId,
        /// What to do next.
        message: String,
    },
    /// The active pane id is not in its tab.
    #[error("{message}")]
    DanglingActivePane {
        /// The tab whose selection is stale.
        tab: TabId,
        /// The id that was stored.
        pane: PaneId,
        /// What to do next.
        message: String,
    },
}

impl WorkspaceIssue {
    /// The explanation a person can act on.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::MissingHost { message, .. }
            | Self::DanglingActiveTab { message, .. }
            | Self::DanglingActivePane { message, .. } => message,
        }
    }
}

/// The workspace restore produced. Every connection is still waiting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoredWorkspace {
    tabs: Vec<RestoredTab>,
    active_tab: Option<TabId>,
    panel: PanelConfig,
    issues: Vec<WorkspaceIssue>,
}

impl RestoredWorkspace {
    /// Restored tabs.
    #[must_use]
    pub fn tabs(&self) -> &[RestoredTab] {
        &self.tabs
    }

    /// Active tab, or `None` when none was saved or the saved id is gone.
    #[must_use]
    pub fn active_tab(&self) -> Option<TabId> {
        self.active_tab
    }

    /// Panel arrangement. Showing it does not start a task.
    #[must_use]
    pub fn panel(&self) -> &PanelConfig {
        &self.panel
    }

    /// Local problems. Empty when every reference resolved.
    #[must_use]
    pub fn issues(&self) -> &[WorkspaceIssue] {
        &self.issues
    }

    /// Every pane, so a caller can see that none of them has been started.
    #[must_use]
    pub fn panes(&self) -> Vec<&PendingPane> {
        let mut out = Vec::new();
        for tab in &self.tabs {
            collect_pending(&tab.root, &mut out);
        }
        out
    }
}

fn collect_pending<'a>(node: &'a RestoredNode, out: &mut Vec<&'a PendingPane>) {
    match node {
        RestoredNode::Leaf(pane) => out.push(pane),
        RestoredNode::Split { first, second, .. } => {
            collect_pending(first, out);
            collect_pending(second, out);
        }
    }
}

/// A restored tab. Its panes are pending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoredTab {
    id: TabId,
    title: String,
    root: RestoredNode,
    active_pane: Option<PaneId>,
}

impl RestoredTab {
    /// Stable id.
    #[must_use]
    pub fn id(&self) -> TabId {
        self.id
    }

    /// Tab title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Restored pane tree.
    #[must_use]
    pub fn root(&self) -> &RestoredNode {
        &self.root
    }

    /// Active pane, or `None` when the saved id is not in the tree.
    #[must_use]
    pub fn active_pane(&self) -> Option<PaneId> {
        self.active_pane
    }
}

/// A restored pane tree. Splits keep their ratio; leaves are not running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoredNode {
    /// A pending pane.
    Leaf(PendingPane),
    /// Two pending subtrees.
    Split {
        /// Which way the children are arranged.
        axis: SplitAxis,
        /// Share of the first child.
        ratio: SplitRatio,
        /// The first child.
        first: Box<RestoredNode>,
        /// The second child.
        second: Box<RestoredNode>,
    },
}

/// A pane that has been restored and not started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingPane {
    id: PaneId,
    mode: TerminalMode,
    connection: PendingConnection,
}

impl PendingPane {
    /// Stable id.
    #[must_use]
    pub fn id(&self) -> PaneId {
        self.id
    }

    /// Terminal presentation.
    #[must_use]
    pub fn mode(&self) -> TerminalMode {
        self.mode
    }

    /// What the user would still have to start.
    #[must_use]
    pub fn connection(&self) -> &PendingConnection {
        &self.connection
    }
}

/// A connection restore is willing to describe.
///
/// There is no connected variant. Adding one would be a different operation from
/// restore, and it would have to be started by the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingConnection {
    /// A local shell that has not been spawned.
    Local {
        /// Shell executable, or `None` for the platform default.
        shell: Option<String>,
    },
    /// A remote host that has not been dialed.
    Remote {
        /// The host the pane belongs to, even when that host is no longer saved.
        host: HostId,
    },
}

impl PendingConnection {
    /// Whether a process or a network connection exists for this pane.
    ///
    /// Always `false`. Restore does not start one.
    #[must_use]
    pub const fn is_started(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_keeps_a_missing_host_local_and_starts_nothing() {
        let saved = HostId::new();
        let gone = HostId::new();
        let local = PaneNode::leaf(
            PaneTarget::local(Some("  pwsh  ".to_owned())),
            TerminalMode::Classic,
        );
        let remote = PaneNode::leaf(PaneTarget::remote(saved), TerminalMode::Integrated);
        let broken = PaneNode::leaf(PaneTarget::remote(gone), TerminalMode::Classic);
        let right = PaneNode::split(SplitAxis::TopBottom, 2500, remote, broken).expect("ratio");
        let root = PaneNode::split(SplitAxis::LeftRight, 5000, local, right).expect("ratio");
        let mut tab = WorkspaceTab::new("Servers", root).expect("title");
        let stale = PaneId::new();
        tab.set_active_pane(Some(stale));
        let mut snapshot =
            WorkspaceSnapshot::new(vec![tab], PanelConfig::new(true, PanelSection::Forwards))
                .expect("snapshot");
        let dangling_tab = TabId::new();
        snapshot.set_active_tab(Some(dangling_tab));

        let restored = snapshot.restore(|id| id == saved);
        assert_eq!(restored.panel().section(), PanelSection::Forwards);
        assert!(restored.panel().visible());
        assert!(restored.active_tab().is_none());
        assert_eq!(restored.tabs().len(), 1);
        assert!(restored.tabs()[0].active_pane().is_none());
        let panes = restored.panes();
        assert_eq!(panes.len(), 3);
        assert!(panes.iter().all(|pane| !pane.connection().is_started()));
        assert!(panes.iter().any(|pane| matches!(
            pane.connection(),
            PendingConnection::Local { shell: Some(shell) } if shell == "pwsh"
        )));
        assert!(restored.issues().iter().any(|issue| matches!(
            issue,
            WorkspaceIssue::MissingHost { host, .. } if *host == gone
        )));
        assert!(restored.issues().iter().any(|issue| matches!(
            issue,
            WorkspaceIssue::DanglingActiveTab { tab, .. } if *tab == dangling_tab
        )));
        assert!(restored.issues().iter().any(|issue| matches!(
            issue,
            WorkspaceIssue::DanglingActivePane { pane, .. } if *pane == stale
        )));
        let secret = "hunter2-workspace";
        let rendered = format!("{restored:?}");
        assert!(!rendered.contains(secret));
        for issue in restored.issues() {
            assert!(issue.message().contains("Nothing was connected"));
            assert!(!issue.message().contains(secret));
        }
    }

    #[test]
    fn an_extreme_ratio_and_a_blank_title_are_refused() {
        assert!(matches!(
            SplitRatio::new(0),
            Err(WorkspaceBuildError::RatioOutOfRange { value: 0 })
        ));
        assert!(matches!(
            PaneNode::split(
                SplitAxis::LeftRight,
                10_000,
                PaneNode::leaf(PaneTarget::local(None), TerminalMode::Classic),
                PaneNode::leaf(PaneTarget::local(None), TerminalMode::Classic),
            ),
            Err(WorkspaceBuildError::RatioOutOfRange { value: 10_000 })
        ));
        assert!(matches!(
            WorkspaceTab::new(
                "   ",
                PaneNode::leaf(PaneTarget::local(None), TerminalMode::Classic)
            ),
            Err(WorkspaceBuildError::EmptyTitle)
        ));
    }
}
