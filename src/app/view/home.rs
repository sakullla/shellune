//! The servers region of the home page: the filters, the group column and the list.
//!
//! The page is drawn from one row model (crate::app::hosts::HostList): the group headers
//! and the hosts the filters keep, flattened into rows that name a HostId or a GroupPath.
//! The rows are drawn through gpui's uniform_list, which asks for the slice it is about
//! to show instead of the whole list, so five thousand saved hosts cost the elements of
//! the visible screen and nothing more (R18, ADR-8).
//!
//! Three properties hold at both window widths:
//!
//! * a row is identified by its host or its group, never by its position, so a search, a
//!   collapse or a reload cannot move a selection onto another host;
//! * the wide window adds a group column and the narrow one folds the same groups into a
//!   filter control, but both draw the same rows;
//! * nothing here asks a server anything. The connection state the rows show and the
//!   status filter narrows by are read from the sessions this window already has and the
//!   recent-connection log the host file carries.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Div, FontWeight, IntoElement, SharedString, Stateful, WeakEntity, div, px,
    uniform_list,
};
use shellune_core::{GroupPath, HostId, SessionKind, SessionState};

use super::shell::ModalContent;
use crate::app::components::Variant;
use crate::app::copy::{self, Strings, UxStrings};
use crate::app::hosts::{
    GroupAction, HostGroupRow, HostList, HostListRow, HostStatusFilter, SelectionScope, format_unix,
};
use crate::app::icons::{self, Icon};
use crate::app::theme::{self, Metrics, Tone, palette, spacing};
use crate::app::view::ShelluneView;
use crate::app::{components, theme::radius};

/// The window width at which the group tree gets a column of its own.
///
/// Narrower than this, the column would take more room than the hosts it navigates; the
/// groups then become the filter control above the list, which reaches the same hosts
/// with one press (R8).
const GROUP_COLUMN_MIN_WIDTH: f32 = 900.0;

/// How wide the group column is when it is drawn.
const GROUP_COLUMN_WIDTH: f32 = 208.0;

/// The narrowest a dashboard card is drawn on a wide window, before zoom.
const HOME_CARD_MIN_WIDTH: f32 = 200.0;

/// The most cards one dashboard section shows on a wide window.
const HOME_CARDS_WIDE: usize = 4;

/// The cards one section shows on a narrow window: half as many, stacked (R1, R8).
const HOME_CARDS_NARROW: usize = 2;

/// What a section's "view all" does.
type SectionAction = Box<dyn Fn(&mut ShelluneView, &mut Context<ShelluneView>) + 'static>;

/// One saved record as a dashboard card shows it.
struct CardRecord {
    /// The selector prefix of the section the card is in.
    selector: &'static str,
    /// The card's number in its section.
    number: usize,
    id: HostId,
    name: String,
    /// What a connection would reach.
    target: String,
    /// When it was last reached, or which protocol it speaks.
    meta: Option<String>,
    /// Whether it is marked as a favourite.
    favorite: bool,
}

/// One row of the list, as the window draws it.
enum SnapshotRow {
    /// A group header.
    Group {
        /// The header's number among the headers of this list, for its selector.
        number: usize,
        /// The group itself.
        row: HostGroupRow,
    },
    /// A saved host.
    Host(HostRowView),
    /// A saved connection of another protocol.
    Connection(ConnectionRowView),
}

/// One connection of another protocol, with what its row shows (R2, R18).
///
/// It has no checkbox: a selection is a batch or group-move target, and both are SSH
/// operations, so a desktop or serial record is never silently pulled into one. Its
/// main action is the same as a host row's - the labelled connect button at the end,
/// with the row's own double press meaning the same - and the record's own protocol
/// decides what pressing it starts (R2).
struct ConnectionRowView {
    id: HostId,
    /// The row's number among the connection rows, for its selector.
    number: usize,
    name: SharedString,
    /// The protocol, as the chip in front of the target says it.
    protocol: SharedString,
    target: SharedString,
    group: Option<SharedString>,
    /// Whether this window already holds a running session for the record.
    open: bool,
    /// Whether the keyboard cursor is on this row.
    cursor: bool,
    /// Whether the record is marked as a favourite (R1).
    favorite: bool,
}

/// One host of the list, with what its row shows.
///
/// The values are read once per frame from the book: the row builder inside the
/// uniform list cannot borrow the view, so what a row shows is prepared with the list
/// it belongs to.
struct HostRowView {
    /// The host, by identifier.
    id: HostId,
    /// The host's number among the host rows of this list, for its selector.
    number: usize,
    /// The display name.
    name: SharedString,
    /// What a connection would dial.
    target: SharedString,
    /// The group the host is in, when it is in one.
    group: Option<SharedString>,
    /// Whether this window already has a session to it.
    open: bool,
    /// When the book last recorded a connection to it, said relatively ("3 hours
    /// ago · 12 connections"), when it ever did.
    last: Option<SharedString>,
    /// The exact stamp of that connection, for the hover.
    last_exact: Option<SharedString>,
    /// Whether the user picked it.
    selected: bool,
    /// Whether the keyboard cursor is on this row.
    cursor: bool,
    /// Whether the viewport is wide enough to show the inline duplicate icon alongside
    /// the forward, edit and delete icons without crowding narrow zoomed windows.
    show_duplicate: bool,
    /// Whether any host is picked, which keeps every checkbox in view: once a
    /// selection has started, picking the next host must not need a hover first.
    selecting: bool,
    /// Whether the host is marked as a favourite (R1).
    favorite: bool,
}

impl ShelluneView {
    /// Whether this window already holds a session to one host.
    ///
    /// Read from the sessions the workspace has, which is state this window already
    /// produced by connecting. Asking the book would say what was saved, and asking a
    /// server would be exactly the background traffic R18 rules out.
    pub(super) fn host_is_open(&self, id: HostId) -> bool {
        self.workspace.sessions().items().iter().any(|session| {
            matches!(session.kind(), SessionKind::Remote { host } if *host == id)
                && matches!(session.state(), SessionState::Connected)
        })
    }

    /// Whether a saved desktop or serial record has a session running here (R2).
    ///
    /// The same rule as a host row's: the state is read from the sessions this window
    /// already holds, never from the saved record, so a connection that was saved and
    /// never started is not drawn as online. A record with no running session says
    /// nothing rather than saying "not connected", because idle is not a state it has.
    pub(super) fn connection_is_open(&self, id: HostId) -> bool {
        self.workspace.sessions().items().iter().any(|session| {
            matches!(
                session.kind(),
                SessionKind::Rdp { host } | SessionKind::Vnc { host } | SessionKind::Serial { host }
                    if *host == id
            ) && matches!(session.state(), SessionState::Connected)
        })
    }

    // -----------------------------------------------------------------------
    // The list
    // -----------------------------------------------------------------------

    /// The servers section: the filters, the selection strip and the list itself.
    pub(super) fn hosts_management(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let list = self
            .hosts
            .list(&self.book, &self.groups, &|id| self.host_is_open(id));
        let short = self.viewport_height < metrics.scale(320.0);
        let mut column = div()
            .flex()
            .flex_col()
            .flex_1()
            .when(!short, |col| col.min_h(px(0.0)))
            .min_w(px(0.0))
            .gap(metrics.scale(spacing::TIGHT))
            .child(self.hosts_filters(&list, cx));

        if !self.hosts.selected().is_empty() {
            column = column.child(self.hosts_selection_strip(&list, cx));
        }
        if let Some(problem) = &self.group_problem {
            column = column.child(
                components::banner(Tone::Danger, metrics)
                    .debug_selector(|| "hosts.group.problem".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(problem.clone())),
            );
        }
        column.child(self.hosts_body(&list, cx))
    }

    /// The search, the view switch, the status filter and the active narrowing.
    fn hosts_filters(&self, list: &HostList, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();

        let mut views = components::segmented(metrics);
        for view in crate::app::hosts::HostView::ALL {
            let selected = self.hosts.view() == view;
            views = views.child(
                components::segment(view.label(strings), selected, metrics)
                    .id(view.debug_selector())
                    .debug_selector(move || view.debug_selector().to_owned())
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.select_host_view(view, cx);
                    })),
            );
        }

        // One toolbar line: find, choose how the list is arranged, and the two tools that
        // work on the list as a whole. The status filter lives in the side column on a
        // wide window - beside the groups, where the other narrowing is - and only takes a
        // line of its own here when there is no column.
        let wide = self.group_column_fits();
        let mut row = div()
            .flex()
            .flex_col()
            .w_full()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "hosts.toolbar".to_owned())
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .flex_1()
                            .min_w(metrics.scale(180.0))
                            .debug_selector(|| "hosts.search".to_owned())
                            .child(self.hosts_search.clone()),
                    )
                    .child(views)
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(metrics.scale(2.0))
                            .child(self.favorites_filter_button(cx))
                            .child(self.batch_entry_button(cx))
                            .child(self.toolbar_import_button(cx)),
                    ),
            )
            .when(!wide, |row| {
                row.child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap(metrics.scale(spacing::TIGHT))
                        .child(
                            div()
                                .flex_none()
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED))
                                .child(ux.hosts_status_label),
                        )
                        .child(self.hosts_status_segments(cx)),
                )
            });
        let _ = list;

        // A narrowed list says what narrows it, and offers one press to drop it: a
        // filter that hides hosts without naming itself is how a list looks empty for no
        // reason (R4).
        if self.hosts.filter().is_narrowing() {
            let mut chips = div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap(metrics.scale(spacing::TIGHT))
                .debug_selector(|| "hosts.filters".to_owned());
            if let Some(path) = self.hosts.filter().group() {
                chips = chips.child(
                    components::chip(path.to_canonical(), Tone::Accent, metrics)
                        .debug_selector(|| "hosts.filter.group".to_owned()),
                );
            }
            if self.hosts.filter().favorites() {
                chips = chips.child(
                    components::chip(ux.hosts_filter_favorites, Tone::Accent, metrics)
                        .debug_selector(|| "hosts.filter.favorites.chip".to_owned()),
                );
            }
            if self.hosts.filter().status() != HostStatusFilter::Any {
                chips = chips.child(
                    components::chip(
                        self.hosts.filter().status().label(ux),
                        Tone::Neutral,
                        metrics,
                    )
                    .debug_selector(|| "hosts.filter.status".to_owned()),
                );
            }
            row = row.child(
                chips.child(
                    components::compact_button(ux.hosts_filter_clear, Variant::Ghost, metrics)
                        .id("hosts.filter.clear")
                        .debug_selector(|| "hosts.filter.clear".to_owned())
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.hosts.clear_filters();
                            cx.notify();
                        })),
                ),
            );
        }
        row
    }

    /// The import entry of the toolbar: labelled on a wide window, an icon on a narrow one.
    fn toolbar_import_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let button = if self.group_column_fits() {
            components::icon_label_button(
                Icon::Import,
                strings.import_open,
                Variant::Ghost,
                metrics,
            )
        } else {
            components::icon_only_button(Icon::Import, metrics).size(metrics.control_height())
        };
        button
            .id("hosts.import")
            .debug_selector(|| "hosts.import".to_owned())
            .tooltip(components::tooltip(strings.import_open))
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.open_import(cx);
            }))
    }

    /// The status filter as one segmented control, for a window without the side column.
    fn hosts_status_segments(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let mut statuses = components::segmented(metrics);
        for status in HostStatusFilter::ALL {
            let selected = self.hosts.filter().status() == status;
            statuses = statuses.child(
                components::segment(status.label(ux), selected, metrics)
                    .id(status.debug_selector())
                    .debug_selector(move || status.debug_selector().to_owned())
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.hosts.set_status_filter(status);
                        cx.notify();
                    })),
            );
        }
        statuses
    }

    /// How many hosts the list shows of how many are saved.
    ///
    /// The count is the same number the selection uses: "shown of saved" answers both
    /// "how many are left" and "is this everything".
    fn hosts_count_line(&self, list: &HostList) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let count = format!(
            "{} · {}",
            view_label(self.hosts.view(), strings),
            copy::fill(
                ux.hosts_result_count,
                &[
                    ("shown", &list.matched_len().to_string()),
                    ("total", &list.total().to_string()),
                ],
            )
        );
        div()
            .flex_1()
            .min_w(metrics.scale(80.0))
            .truncate()
            .debug_selector(|| "hosts.count".to_owned())
            .text_size(metrics.caption_size())
            .text_color(theme::swatch(palette::TEXT_MUTED))
            .child(SharedString::from(count))
    }

    /// What the selection holds, what it covers, and what can be done with it.
    fn hosts_selection_strip(&self, list: &HostList, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let mut scopes = components::segmented(metrics);
        for scope in SelectionScope::ALL {
            let selected = self.hosts.scope() == scope;
            scopes = scopes.child(
                components::segment(scope.label(ux), selected, metrics)
                    .id(scope.debug_selector())
                    .debug_selector(move || scope.debug_selector().to_owned())
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.hosts.set_scope(scope);
                        cx.notify();
                    })),
            );
        }
        let count = copy::fill(
            ux.hosts_selection_count,
            &[("count", &self.hosts.selected_count().to_string())],
        );
        let scope = self.hosts.scope();
        let in_scope = list.ids_in_scope(scope).to_vec();
        let scope_text = format!("{} {}", ux.hosts_scope_label, scope.label(ux));

        div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(metrics.scale(spacing::TIGHT))
            .p(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::SURFACE_SELECTED))
            .debug_selector(|| "hosts.selection".to_owned())
            .child(
                div()
                    .flex_none()
                    .text_size(metrics.label_size())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "hosts.selection.count".to_owned())
                    .child(SharedString::from(count)),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(SharedString::from(scope_text)),
            )
            .child(scopes)
            .child(
                components::compact_button(ux.hosts_select_scope, Variant::Secondary, metrics)
                    .id("hosts.select.scope")
                    .debug_selector(|| "hosts.select.scope".to_owned())
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.select_hosts_in_scope(in_scope.clone(), cx);
                    })),
            )
            .child(
                components::compact_button(ux.hosts_select_none, Variant::Ghost, metrics)
                    .id("hosts.select.none")
                    .debug_selector(|| "hosts.select.none".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.hosts.clear_selection();
                        cx.notify();
                    })),
            )
            .child(
                components::compact_button(ux.hosts_group_move, Variant::Primary, metrics)
                    .id("hosts.select.move")
                    .debug_selector(|| "hosts.select.move".to_owned())
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.open_group_dialog(GroupAction::Move, window, cx);
                    })),
            )
            .child(
                components::compact_button(ux.hosts_batch_open, Variant::Secondary, metrics)
                    .id("hosts.batch.open")
                    .debug_selector(|| "hosts.batch.open".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.open_batch_preview(cx);
                    })),
            )
    }

    /// The way into a batch task from the list itself.
    ///
    /// A task is built from the current results as well as from a selection (R18), so the
    /// entry sits beside the filters: narrowing the list then pressing once is a whole
    /// batch target, and the preview is where the count is confirmed.
    fn batch_entry_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let ux = copy::ux();
        // A narrow window keeps the toolbar on one line: the tool is its icon, and the
        // tooltip says its name.
        let button = if self.group_column_fits() {
            components::icon_label_button(
                Icon::Layers,
                ux.hosts_batch_open,
                Variant::Ghost,
                metrics,
            )
        } else {
            components::icon_only_button(Icon::Layers, metrics).size(metrics.control_height())
        };
        button
            .id("hosts.batch.open.list")
            .debug_selector(|| "hosts.batch.open.list".to_owned())
            .tooltip(components::tooltip(ux.hosts_batch_open_hint))
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.open_batch_preview(cx);
            }))
    }

    /// The group column and the list on a wide window; the list alone on a narrow one.
    fn hosts_body(&self, list: &HostList, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let wide = self.group_column_fits();
        let rows = self.host_rows_snapshot(list);
        let empty = list.is_empty();
        let short = self.viewport_height < metrics.scale(320.0);
        let mut list_column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .when(!short, |col| col.min_h(px(0.0)))
            .gap(metrics.scale(spacing::TIGHT))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .min_h(metrics.compact_control_height())
                    .gap(metrics.scale(spacing::TIGHT))
                    .when(!wide, |row| row.child(self.hosts_group_chooser(cx)))
                    .child(self.hosts_count_line(list))
                    .when(wide, |row| row.child(self.group_column_actions(cx))),
            );

        // The rows sit on one surface, so the list reads as one object beside the
        // column that narrows it.
        let surface = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .when(!short, |col| col.min_h(px(0.0)))
            .p(metrics.scale(spacing::HAIRLINE))
            .rounded(px(radius::LARGE))
            .bg(theme::swatch(palette::SURFACE))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(|| "hosts.surface".to_owned());
        if empty {
            list_column = list_column.child(
                surface
                    .px(metrics.scale(spacing::BASE))
                    .py(metrics.scale(spacing::SNUG))
                    .child(self.hosts_empty_list(cx)),
            );
        } else {
            list_column = list_column.child(surface.child(self.hosts_virtual_list(rows, cx)));
        }

        let mut body = div()
            .flex()
            .flex_row()
            .flex_1()
            .when(!short, |b| b.min_h(px(0.0)))
            .min_w(px(0.0))
            .gap(metrics.scale(spacing::SNUG))
            // The number of rows is part of the selector, so a test can tell "the
            // search narrowed the list" from "the list did not change" without reading
            // text, and a person reading the manifest sees the same count.
            .debug_selector({
                let shown = list.matched_len();
                move || format!("hosts.list.{shown}")
            });
        if wide {
            body = body.child(self.hosts_group_column(&self.group_tree_rows(), cx));
        }
        body.child(list_column)
    }

    /// Whether the window is wide enough for the group column (R8).
    ///
    /// One predicate decides it, so the layout and a test cannot disagree about which
    /// of the two shapes was drawn.
    pub(crate) fn group_column_fits(&self) -> bool {
        self.viewport_width >= self.metrics().scale(GROUP_COLUMN_MIN_WIDTH)
    }

    /// The group tree the column and the narrow chooser both read.
    fn group_tree_rows(&self) -> Vec<HostGroupRow> {
        self.hosts
            .group_tree(&self.book, &self.groups, &|id| self.host_is_open(id))
    }

    /// The group column: the tree the list's headers come from (R4, R18).
    ///
    /// It opens with "all hosts", the row that clears the narrowing, and the creation of
    /// a group sits on the column's own heading, where a new group will appear.
    fn hosts_group_column(&self, tree: &[HostGroupRow], cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let column = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(metrics.scale(GROUP_COLUMN_WIDTH))
            .min_h(px(0.0))
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(|| "hosts.groups".to_owned())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pl(metrics.scale(spacing::TIGHT))
                    .child(components::section_label(ux.hosts_groups_title, metrics))
                    .child(
                        components::icon_only_button(Icon::Plus, metrics)
                            .id("hosts.group.new")
                            .debug_selector(|| "hosts.group.new".to_owned())
                            .tooltip(components::tooltip(ux.hosts_group_new))
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.open_group_dialog(GroupAction::Create, window, cx);
                            })),
                    ),
            );

        let mut rows = div()
            .id("hosts.groups.scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .gap(metrics.scale(1.0))
            .overflow_y_scroll()
            .child(self.group_tree_all_row(cx));

        // A row of the tree is a group to narrow to. The hosts in no group are not one:
        // they have their own header in the list, and "all hosts" above reaches them.
        let groups: Vec<(usize, &HostGroupRow)> = tree
            .iter()
            .enumerate()
            .filter(|(_, group)| group.path.is_some())
            .collect();
        for (position, (number, group)) in groups.iter().enumerate() {
            let branch = groups
                .get(position + 1)
                .is_some_and(|(_, next)| next.depth > group.depth);
            rows = rows.child(self.group_tree_row(group, *number, branch, cx));
        }
        // The tree follows the search, so an empty tree under a search only means the
        // search hid the groups; the list beside it already says so. The hint to make
        // a group is for a book that really has none.
        let searching =
            !self.hosts.search().trim().is_empty() || self.hosts.filter().is_narrowing();
        if groups.is_empty() && !searching {
            rows = rows.child(
                div()
                    .px(metrics.scale(spacing::TIGHT))
                    .py(metrics.scale(spacing::TIGHT))
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "hosts.groups.empty".to_owned())
                    .child(ux.hosts_group_empty),
            );
        }
        // The statuses follow the groups in the same scrolling column, so they sit
        // right under the last group instead of at the far end of the window.
        column.child(rows.child(self.hosts_status_column(cx)))
    }

    /// The status filter as rows of the side column, under the groups: both narrow the
    /// same list, so they sit together, and the toolbar keeps a single line.
    fn hosts_status_column(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let mut section = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(metrics.scale(1.0))
            .pt(metrics.scale(spacing::SNUG))
            .mt(metrics.scale(spacing::HAIRLINE))
            .border_t_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(|| "hosts.statuses".to_owned())
            .child(
                div()
                    .pl(metrics.scale(spacing::TIGHT))
                    .pb(metrics.scale(spacing::HAIRLINE))
                    .child(components::section_label(ux.hosts_status_label, metrics)),
            );
        for status in HostStatusFilter::ALL {
            let chosen = self.hosts.filter().status() == status;
            let tone = match status {
                HostStatusFilter::Any => None,
                HostStatusFilter::Connected => Some(Tone::Positive),
                HostStatusFilter::Recent => Some(Tone::Accent),
                HostStatusFilter::NeverConnected => Some(Tone::Neutral),
            };
            section = section.child(
                tree_row_frame(chosen, metrics)
                    .id(status.debug_selector())
                    .debug_selector(move || status.debug_selector().to_owned())
                    .pl(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(metrics.small_icon_size())
                            .children(tone.map(|tone| components::status_dot(tone, metrics))),
                    )
                    .child(tree_row_label(status.label(ux).to_owned(), chosen, metrics))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.hosts.set_status_filter(status);
                        cx.notify();
                    })),
            );
        }
        section
    }

    /// The favourites filter of the toolbar: a star that stays lit while only the
    /// favourites are listed (R1).
    ///
    /// It sits with the list's other whole-list tools rather than in the side column,
    /// so it is in view on both window shapes; a host can be a favourite and never
    /// connected, so it combines with the status instead of being one.
    fn favorites_filter_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let ux = copy::ux();
        let favorites = self.hosts.filter().favorites();
        let hover = theme::swatch(palette::SURFACE_HOVER);
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(metrics.control_height())
            .rounded(px(radius::MEDIUM))
            .cursor_pointer()
            .when(favorites, |button| {
                button.bg(theme::swatch(palette::SURFACE_SELECTED))
            })
            .when(!favorites, |button| {
                button.hover(move |style| style.bg(hover))
            })
            .child(icons::icon(
                if favorites {
                    Icon::StarFilled
                } else {
                    Icon::Star
                },
                metrics.small_icon_size(),
                theme::swatch(if favorites {
                    palette::ACCENT
                } else {
                    palette::TEXT_SECONDARY
                }),
            ))
            .id("hosts.filter.favorites")
            .debug_selector(|| "hosts.filter.favorites".to_owned())
            .tooltip(components::tooltip(ux.hosts_filter_favorites))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.hosts.set_favorites_filter(!favorites);
                this.hosts_cursor = None;
                cx.notify();
            }))
    }

    /// The first row of the group column: every saved host, which is no narrowing.
    fn group_tree_all_row(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let ux = copy::ux();
        let chosen = self.hosts.filter().group().is_none();
        tree_row_frame(chosen, metrics)
            .id("hosts.tree.all")
            .debug_selector(|| "hosts.tree.all".to_owned())
            .pl(metrics.scale(spacing::TIGHT))
            .child(icons::icon(
                Icon::Server,
                metrics.small_icon_size(),
                theme::swatch(if chosen {
                    palette::ACCENT
                } else {
                    palette::TEXT_MUTED
                }),
            ))
            .child(tree_row_label(
                ux.hosts_groups_all.to_owned(),
                chosen,
                metrics,
            ))
            // Every saved record the list can show, desktop and serial ones included,
            // so the number matches the list's own "n of m" line.
            .child(components::badge(
                self.book.hosts().len() + self.book.connections().len(),
                if chosen { Tone::Accent } else { Tone::Neutral },
                metrics,
            ))
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.pick_group_row(None, cx);
            }))
    }

    /// One group of the column: its chevron, its name and how many hosts it holds.
    ///
    /// Only a branch has a chevron; a leaf keeps the chevron's room so names still line
    /// up, but offers nothing to open.
    fn group_tree_row(
        &self,
        group: &HostGroupRow,
        number: usize,
        branch: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let path = group.path.clone();
        let narrowed = match (&path, self.hosts.filter().group()) {
            (Some(path), Some(filter)) => path == filter,
            _ => false,
        };
        let label = group.path.as_ref().map_or_else(String::new, |path| {
            path.segments().last().cloned().unwrap_or_default()
        });
        let chevron = if group.expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        let toggle_path = path.clone();
        let pick_path = path.clone();
        let toggle = if branch {
            components::icon_only_button(chevron, metrics)
                .size(metrics.scale(18.0))
                .id(SharedString::from(format!("hosts.tree.{number}.toggle")))
                .debug_selector(move || format!("hosts.tree.{number}.toggle"))
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    cx.stop_propagation();
                    this.toggle_group_row(toggle_path.clone(), cx);
                }))
                .into_any_element()
        } else {
            div()
                .flex_none()
                .size(metrics.scale(18.0))
                .into_any_element()
        };
        tree_row_frame(narrowed, metrics)
            .id(SharedString::from(format!("hosts.tree.{number}")))
            .debug_selector(move || format!("hosts.tree.{number}"))
            .pl(metrics.scale(spacing::HAIRLINE + group.depth as f32 * 14.0))
            .child(toggle)
            .child(icons::icon(
                Icon::Folder,
                metrics.small_icon_size(),
                theme::swatch(if narrowed {
                    palette::ACCENT
                } else {
                    palette::TEXT_MUTED
                }),
            ))
            .child(tree_row_label(label, narrowed, metrics))
            .child(components::badge(
                group.hosts,
                if narrowed {
                    Tone::Accent
                } else {
                    Tone::Neutral
                },
                metrics,
            ))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.pick_group_row(pick_path.clone(), cx);
            }))
    }

    /// The group filter, folded into one control on a narrow window (R8).
    fn hosts_group_chooser(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let current = self.hosts.filter().group().cloned();
        let mut chooser = div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(|| "hosts.group.chooser".to_owned())
            .child(
                components::segment(ux.hosts_groups_all, current.is_none(), metrics)
                    .id("hosts.group.all")
                    .debug_selector(|| "hosts.group.all".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.hosts.set_group_filter(None);
                        cx.notify();
                    })),
            );
        // The top level of the tree is what a narrow window offers: a branch is reached
        // by choosing its parent, which is one press and still narrows to the subtree.
        let mut paths: Vec<GroupPath> = self
            .group_tree_rows()
            .into_iter()
            .filter(|group| group.depth == 0)
            .filter_map(|group| group.path)
            .collect();
        paths.sort();
        for path in paths {
            let label = path.to_canonical();
            let chosen = current.as_ref() == Some(&path);
            let pick = path.clone();
            chooser = chooser.child(
                components::segment(label, chosen, metrics)
                    .id(SharedString::from(format!(
                        "hosts.group.option.{}",
                        path.to_canonical()
                    )))
                    .debug_selector({
                        let name = path.to_canonical();
                        move || format!("hosts.group.option.{name}")
                    })
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.hosts.set_group_filter(Some(pick.clone()));
                        cx.notify();
                    })),
            );
        }
        chooser
    }

    /// The group actions that the wide window keeps beside the tree.
    fn group_column_actions(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let mut row = div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(|| "hosts.group.actions".to_owned());
        if let Some(path) = self.hosts.filter().group().cloned() {
            let rename = path.clone();
            let delete = path;
            row = row
                .child(
                    components::compact_button(ux.hosts_group_rename, Variant::Ghost, metrics)
                        .id("hosts.group.rename.open")
                        .debug_selector(|| "hosts.group.rename.open".to_owned())
                        .on_click(cx.listener(move |this, _event, window, cx| {
                            this.open_group_dialog(GroupAction::Rename(rename.clone()), window, cx);
                        })),
                )
                .child(
                    components::compact_button(ux.hosts_group_delete, Variant::Ghost, metrics)
                        .id("hosts.group.delete.open")
                        .debug_selector(|| "hosts.group.delete.open".to_owned())
                        .on_click(cx.listener(move |this, _event, window, cx| {
                            this.open_group_dialog(GroupAction::Delete(delete.clone()), window, cx);
                        })),
                );
        }
        row
    }

    /// What a list with no row says: the reason, and what would change it (R1, R3).
    ///
    /// A list emptied by a search or a filter names what narrowed it and offers one press
    /// that drops all of it; an empty recent view offers the whole list. Neither is allowed
    /// to be a sentence with no next step.
    fn hosts_empty_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        // An empty list is a calm, centred note in the middle of the list's surface: an
        // icon, what happened, and the one press that changes it. The button is as wide
        // as its words, not a bar across the list.
        let note = |selector: &'static str,
                    icon: Icon,
                    title: &'static str,
                    hint: Option<&'static str>| {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(metrics.scale(spacing::HAIRLINE + 2.0))
                .py(metrics.scale(spacing::LOOSE))
                .debug_selector(move || selector.to_owned())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(metrics.scale(40.0))
                        .mb(metrics.scale(spacing::HAIRLINE))
                        .rounded_full()
                        .bg(theme::swatch(palette::SURFACE_SELECTED))
                        .child(icons::icon(
                            icon,
                            metrics.icon_size(),
                            theme::swatch(palette::TEXT_MUTED),
                        )),
                )
                .child(
                    div()
                        .text_size(metrics.body_size())
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .child(title),
                )
                .children(hint.map(|hint| {
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .child(hint)
                }))
        };
        if self.hosts.view() == crate::app::hosts::HostView::Recent {
            return note(
                "hosts.recent.empty",
                Icon::Layers,
                strings.hosts_recent_empty,
                None,
            )
            .child(
                components::compact_button(strings.hosts_view_all, Variant::Secondary, metrics)
                    .mt(metrics.scale(spacing::HAIRLINE + 2.0))
                    .id("hosts.recent.all")
                    .debug_selector(|| "hosts.recent.all".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.select_host_view(crate::app::hosts::HostView::All, cx);
                    })),
            )
            .into_any_element();
        }
        let narrow = !self.hosts.search().trim().is_empty() || self.hosts.filter().is_narrowing();
        if !narrow {
            return div().into_any_element();
        }
        note(
            "hosts.nomatch",
            Icon::Search,
            strings.hosts_no_match,
            Some(ux.hosts_no_match_hint),
        )
        .child(
            components::compact_button(ux.hosts_filter_clear, Variant::Secondary, metrics)
                .mt(metrics.scale(spacing::HAIRLINE + 2.0))
                .id("hosts.nomatch.clear")
                .debug_selector(|| "hosts.nomatch.clear".to_owned())
                .on_click(cx.listener(|this, _event, _window, cx| {
                    this.clear_host_narrowing(cx);
                })),
        )
        .into_any_element()
    }

    /// The list itself: a uniform list that is asked for the rows it shows.
    ///
    /// The closure cannot borrow the view, so it is handed the prepared rows and a weak
    /// handle for the actions a row offers. What it never gets is the book: five
    /// thousand hosts are five thousand rows of data, and building an element for one of
    /// them only happens when that row is about to be drawn.
    #[allow(clippy::needless_pass_by_value)]
    fn hosts_virtual_list(&self, rows: Rc<Vec<SnapshotRow>>, cx: &mut Context<Self>) -> AnyElement {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let weak = cx.entity().downgrade();
        let row_height = metrics.list_row_height();
        // How many rows one frame is asked to build is recorded, not asserted: the list
        // is the thing that decides which rows are visible, and this is what it decided.
        let drawn = self.hosts_rows_drawn.clone();
        // The recent view names its rows after the connection that put them there, and
        // every other view names them after the host; both are the host's number in the
        // list, so a test can read either without knowing the layout.
        let prefix = if self.hosts.view() == crate::app::hosts::HostView::Recent {
            "hosts.recent"
        } else {
            "hosts.row"
        };
        let min_list_height = if self.viewport_height < metrics.scale(320.0) {
            row_height * 2.0
        } else {
            px(0.0)
        };
        uniform_list("hosts.rows", rows.len(), move |range, _window, _cx| {
            drawn.set(drawn.get().max(range.len()));
            let mut items: Vec<AnyElement> = Vec::with_capacity(range.len());
            for row in &rows[range] {
                let element = match row {
                    SnapshotRow::Group { number, row } => {
                        group_header_row(row, *number, row_height, metrics, strings, &weak)
                    }
                    SnapshotRow::Host(host) => {
                        host_row_element(host, &weak, row_height, prefix, metrics, strings, ux)
                    }
                    SnapshotRow::Connection(connection) => {
                        connection_row_element(connection, &weak, row_height, metrics, strings)
                    }
                };
                items.push(element.into_any_element());
            }
            items
        })
        .track_scroll(self.hosts_scroll.clone())
        .flex_1()
        .min_h(min_list_height)
        .min_w(px(0.0))
        .debug_selector(|| "hosts.rows".to_owned())
        .into_any_element()
    }

    /// Reads every row of the list once, for the frame about to be drawn.
    fn host_rows_snapshot(&self, list: &HostList) -> Rc<Vec<SnapshotRow>> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let cursor = self.active_hosts_cursor();
        let show_duplicate = self.viewport_width >= self.metrics().scale(760.0);
        let show_group_tag = self.viewport_width >= self.metrics().scale(540.0);
        let mut rows = Vec::with_capacity(list.len());
        let mut hosts = 0usize;
        let mut groups = 0usize;
        let mut connections = 0usize;
        for row in list.rows() {
            match row {
                HostListRow::Group(group) => {
                    rows.push(SnapshotRow::Group {
                        number: groups,
                        row: group.clone(),
                    });
                    groups += 1;
                }
                HostListRow::Host(host) => {
                    let Some(saved) = self.book.host(host.id) else {
                        continue;
                    };
                    rows.push(SnapshotRow::Host(HostRowView {
                        id: host.id,
                        number: hosts,
                        name: SharedString::from(saved.name().to_owned()),
                        target: SharedString::from(saved.target()),
                        group: show_group_tag
                            .then(|| saved.group_text().map(SharedString::from))
                            .flatten(),
                        open: self.host_is_open(host.id),
                        last: self.book.recent_of(host.id).map(|recent| {
                            SharedString::from(format!(
                                "{} {} · {} {}",
                                self.strings().host_last_connected,
                                copy::relative_time(
                                    recent.last_connected_unix(),
                                    now,
                                    copy::Locale::current()
                                ),
                                recent.connections(),
                                self.strings().host_connection_count
                            ))
                        }),
                        last_exact: self.book.recent_of(host.id).map(|recent| {
                            SharedString::from(format!(
                                "{} {}",
                                self.strings().host_last_connected,
                                format_unix(recent.last_connected_unix())
                            ))
                        }),
                        selected: self.hosts.selected().contains(host.id),
                        cursor: cursor == Some(host.id),
                        show_duplicate,
                        selecting: !self.hosts.selected().is_empty(),
                        favorite: self.book.is_favorite(host.id),
                    }));
                    hosts += 1;
                }
                HostListRow::Connection(row) => {
                    let Some(saved) = self.book.connection(row.id) else {
                        continue;
                    };
                    rows.push(SnapshotRow::Connection(ConnectionRowView {
                        id: row.id,
                        number: connections,
                        name: SharedString::from(saved.name().to_owned()),
                        protocol: SharedString::from(saved.protocol().label()),
                        target: SharedString::from(saved.target()),
                        group: show_group_tag
                            .then(|| saved.group_text().map(SharedString::from))
                            .flatten(),
                        open: self.connection_is_open(row.id),
                        cursor: cursor == Some(row.id),
                        favorite: self.book.is_favorite(row.id),
                    }));
                    connections += 1;
                }
            }
        }
        Rc::new(rows)
    }

    // -----------------------------------------------------------------------
    // The dashboard (R1)
    // -----------------------------------------------------------------------

    /// The band under the quick-connect bar: what the bar found while a phrase is
    /// typed, otherwise the recent, favourite and group cards.
    ///
    /// A section with nothing to show is left out rather than drawn empty, and a band
    /// with no section at all is no band: a first run shows its welcome card instead.
    pub(super) fn home_dashboard(
        &self,
        cards_allowed: bool,
        wide: bool,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if !self.quick_query.trim().is_empty() {
            return Some(self.quick_results_panel(cx));
        }
        if !cards_allowed {
            return None;
        }
        let metrics = self.metrics();
        let ux = copy::ux();
        let per_section = self.home_cards_per_section(wide);
        let mut band = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .debug_selector(|| "home.cards".to_owned());
        let mut any = false;

        let recent = self.book.recent_hosts();
        if !recent.is_empty() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs());
            let cards: Vec<AnyElement> = recent
                .iter()
                .take(per_section)
                .enumerate()
                .map(|(number, recent)| {
                    let host = recent.host;
                    let id = host.id();
                    let when = copy::relative_time(
                        recent.last_connected_unix,
                        now,
                        copy::Locale::current(),
                    );
                    self.home_record_card(
                        CardRecord {
                            selector: "home.recent",
                            number,
                            id,
                            name: host.name().to_owned(),
                            target: host.target(),
                            meta: Some(when),
                            favorite: self.book.is_favorite(id),
                        },
                        cx,
                    )
                    .into_any_element()
                })
                .collect();
            band = band.child(self.home_section(
                "home.recent",
                ux.home_recent_section,
                recent.len(),
                cards,
                wide,
                per_section,
                Some(Box::new(|this: &mut Self, cx: &mut Context<Self>| {
                    this.show_all_recent(cx);
                })),
                cx,
            ));
            any = true;
        }

        let favorites = self.book.favorite_hosts();
        if !favorites.is_empty() {
            let cards: Vec<AnyElement> = favorites
                .iter()
                .take(per_section)
                .enumerate()
                .map(|(number, record)| {
                    let id = record.id();
                    // A record of another protocol says which, as its list row does.
                    let meta = (record.protocol() != shellune_core::ProtocolKind::Ssh)
                        .then(|| record.protocol().label().to_owned());
                    self.home_record_card(
                        CardRecord {
                            selector: "home.favorite",
                            number,
                            id,
                            name: record.name().to_owned(),
                            target: record.target(),
                            meta,
                            favorite: true,
                        },
                        cx,
                    )
                    .into_any_element()
                })
                .collect();
            band = band.child(self.home_section(
                "home.favorites",
                ux.home_favorites_section,
                favorites.len(),
                cards,
                wide,
                per_section,
                Some(Box::new(|this: &mut Self, cx: &mut Context<Self>| {
                    this.show_all_favorites(cx);
                })),
                cx,
            ));
            any = true;
        }

        let groups = crate::app::hosts::group_overview(&self.book, &self.groups, &|id| {
            self.host_is_open(id) || self.connection_is_open(id)
        });
        if !groups.is_empty() {
            // Every group fits the band when the window has room for it; the tree beside
            // the list is the place for the rest, so the section has no "view all".
            let cards: Vec<AnyElement> = groups
                .iter()
                .take(per_section)
                .enumerate()
                .map(|(number, group)| self.home_group_card(group, number, cx).into_any_element())
                .collect();
            band = band.child(self.home_section(
                "home.groups",
                ux.home_groups_section,
                cards.len(),
                cards,
                wide,
                per_section,
                None,
                cx,
            ));
            any = true;
        }
        any.then_some(band)
    }

    /// How many cards one section shows.
    ///
    /// A wide window lays the cards out on one line and shows as many as fit, at most
    /// four; a narrow one stacks them in one column and shows half as many, so the
    /// band never pushes the list off the window (R1, R8).
    pub(crate) fn home_cards_per_section(&self, wide: bool) -> usize {
        if !wide {
            return HOME_CARDS_NARROW;
        }
        let metrics = self.metrics();
        let gap = metrics.scale(spacing::TIGHT + 2.0);
        let width = self
            .viewport_width
            .min(metrics.scale(super::HOME_MAX_WIDTH))
            - metrics.scale(super::HOME_SIDE_PADDING) * 2.0;
        let fits = ((width + gap) / (metrics.scale(HOME_CARD_MIN_WIDTH) + gap)).floor();
        (fits.max(1.0) as usize).min(HOME_CARDS_WIDE)
    }

    /// One section of the band: its title, the "view all" that opens the rest in the
    /// list, and the cards - on one line when wide, stacked when narrow.
    #[allow(clippy::too_many_arguments)]
    fn home_section(
        &self,
        selector: &'static str,
        title: &'static str,
        total: usize,
        cards: Vec<AnyElement>,
        wide: bool,
        per_line: usize,
        view_all: Option<SectionAction>,
        cx: &mut Context<Self>,
    ) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let shown = cards.len();
        let mut heading = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(metrics.scale(spacing::TIGHT))
            .min_h(metrics.scale(20.0))
            .child(components::section_label(title, metrics));
        if let Some(action) = view_all.filter(|_| total > shown) {
            heading = heading.child(
                components::compact_button(ux.home_view_all, Variant::Ghost, metrics)
                    .h(metrics.scale(20.0))
                    .id(SharedString::from(format!("{selector}.all")))
                    .debug_selector(move || format!("{selector}.all"))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        action(this, cx);
                    })),
            );
        }
        let mut line = if wide {
            div().flex()
        } else {
            div().flex().flex_col()
        }
        .gap(metrics.scale(spacing::TIGHT + 2.0))
        .debug_selector(move || format!("{selector}.cards"));
        let count = cards.len();
        for card in cards {
            line = line.child(div().flex().flex_1().min_w(px(0.0)).child(card));
        }
        // A short line keeps the cards the width a full one gives them.
        if wide {
            for _ in count..per_line {
                line = line.child(div().flex_1().min_w(px(0.0)));
            }
        }
        div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE + 2.0))
            .debug_selector(move || selector.to_owned())
            .child(heading)
            .child(line)
    }

    /// One saved record as a card: press to connect, right press or the "⋯" for every
    /// action the list row offers.
    fn home_record_card(&self, card: CardRecord, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let ux = copy::ux();
        let CardRecord {
            selector,
            number,
            id,
            name,
            target,
            meta,
            favorite,
        } = card;
        let group = format!("{selector}-card");
        let more_group = group.clone();
        home_card_frame(metrics)
            .id(SharedString::from(format!("{selector}.{number}")))
            .debug_selector(move || format!("{selector}.{number}"))
            .group(SharedString::from(group))
            .tooltip(components::tooltip(ux.home_card_connect_hint))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.connect_saved_connection(id, cx);
            }))
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_host_menu(id, event, window, cx);
                }),
            )
            .child(super::host_avatar(&name, false, metrics).size(metrics.scale(30.0)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .gap(metrics.scale(1.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(metrics.scale(spacing::HAIRLINE))
                            .min_w(px(0.0))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(metrics.label_size())
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                    .child(SharedString::from(name)),
                            )
                            .when(favorite, |line| {
                                line.child(icons::icon(
                                    Icon::StarFilled,
                                    metrics.scale(11.0),
                                    theme::swatch(palette::ACCENT),
                                ))
                            }),
                    )
                    // The target gives way first: the age or the protocol after it is
                    // the short part a card is scanned for.
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(metrics.scale(spacing::HAIRLINE))
                            .min_w(px(0.0))
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(
                                div()
                                    .min_w(px(0.0))
                                    .truncate()
                                    .font_family(crate::app::terminal::monospace_family())
                                    .child(SharedString::from(target)),
                            )
                            .children(meta.map(|meta| {
                                div()
                                    .flex_none()
                                    .child(SharedString::from(format!("· {meta}")))
                            })),
                    ),
            )
            .child(
                components::icon_only_button(Icon::More, metrics)
                    .size(metrics.scale(24.0))
                    .opacity(0.0)
                    .group_hover(SharedString::from(more_group), |style| style.opacity(1.0))
                    .id(SharedString::from(format!("{selector}.{number}.more")))
                    .debug_selector(move || format!("{selector}.{number}.more"))
                    .tooltip(components::tooltip(ux.home_card_more))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_host_menu(id, event, window, cx);
                        }),
                    ),
            )
    }

    /// One top-level group as a card: its name, how much it holds and how much of it is
    /// open. Pressing it narrows the list below to the group.
    fn home_group_card(
        &self,
        group: &crate::app::hosts::GroupOverview,
        number: usize,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let ux = copy::ux();
        let count = if group.online > 0 {
            copy::fill(
                ux.home_group_count,
                &[
                    ("count", &group.records.to_string()),
                    ("online", &group.online.to_string()),
                ],
            )
        } else {
            copy::fill(
                ux.home_group_count_idle,
                &[("count", &group.records.to_string())],
            )
        };
        let narrowed = self.hosts.filter().group() == Some(&group.path);
        let path = group.path.clone();
        home_card_frame(metrics)
            .id(SharedString::from(format!("home.group.{number}")))
            .debug_selector(move || format!("home.group.{number}"))
            .when(narrowed, |card| {
                card.border_color(theme::faded(theme::swatch(palette::ACCENT), 0.6))
            })
            .tooltip(components::tooltip(ux.home_group_card_hint))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.pick_group_card(path.clone(), cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(metrics.scale(30.0))
                    .rounded(px(radius::MEDIUM))
                    .bg(theme::swatch(palette::ACCENT_TINT))
                    .child(icons::icon(
                        Icon::Folder,
                        metrics.small_icon_size(),
                        theme::swatch(palette::ACCENT_ON_TINT),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .gap(metrics.scale(1.0))
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics.label_size())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(SharedString::from(group.path.to_canonical())),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .when(group.online > 0, |line| {
                                line.text_color(Tone::Positive.text())
                            })
                            .child(SharedString::from(count)),
                    ),
            )
    }

    /// What the quick-connect bar found: saved records, the unsaved address and the
    /// local shells, with the one Enter opens marked (R1).
    fn quick_results_panel(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let results = self.quick_results();
        let highlight = self.quick_highlight();
        let mut panel = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(1.0))
            .p(metrics.scale(spacing::HAIRLINE))
            .rounded(px(radius::LARGE))
            .bg(theme::swatch(palette::SURFACE))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(|| "home.quick.results".to_owned());
        if results.is_empty() {
            return panel.child(
                div()
                    .px(metrics.scale(spacing::TIGHT + 2.0))
                    .py(metrics.scale(spacing::TIGHT))
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .debug_selector(|| "home.quick.empty".to_owned())
                    .child(ux.home_quick_empty),
            );
        }
        let mut saved = 0usize;
        for (number, result) in results.iter().enumerate() {
            let (icon, title, detail, kind) = match result {
                super::QuickResult::Saved(id) => {
                    let Some(record) = self.book.connection(*id) else {
                        continue;
                    };
                    let detail = format!("{} · {}", record.protocol().label(), record.target());
                    let kind = format!("home.quick.saved.{saved}");
                    saved += 1;
                    (Icon::Plug, record.name().to_owned(), detail, kind)
                }
                super::QuickResult::Temporary(target) => (
                    Icon::ArrowRight,
                    copy::fill(ux.home_quick_temporary, &[("target", &target.label())]),
                    ux.home_quick_temporary_hint.to_owned(),
                    "home.quick.temporary".to_owned(),
                ),
                super::QuickResult::Shell(index) => {
                    let Some(shell) = self.workspace.shells().get(*index) else {
                        continue;
                    };
                    // The shell "+" opens says so, as the menus mark it.
                    let mut detail = format!(
                        "{} · {}",
                        ux.home_quick_shell_hint,
                        super::program_label(shell.program())
                    );
                    if *index == self.workspace.shell_choice() {
                        detail.push_str(" · ");
                        detail.push_str(self.strings().shell_default);
                    }
                    (
                        Icon::Terminal,
                        shell.name().to_owned(),
                        detail,
                        format!("home.quick.shell.{index}"),
                    )
                }
            };
            let lit = number == highlight;
            let hover = theme::swatch(palette::SURFACE_HOVER);
            panel = panel.child(
                div()
                    .id(SharedString::from(format!("home.quick.result.{number}")))
                    .debug_selector(move || format!("home.quick.result.{number}"))
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::SNUG))
                    .min_h(metrics.scale(40.0))
                    .px(metrics.scale(spacing::TIGHT + 2.0))
                    .rounded(px(radius::MEDIUM))
                    .cursor_pointer()
                    .when(lit, |row| row.bg(theme::swatch(palette::SURFACE_SELECTED)))
                    .when(!lit, |row| row.hover(move |style| style.bg(hover)))
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        if this.run_quick_result(Some(number), window, cx) {
                            this.clear_quick_bar(cx);
                        }
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(metrics.scale(26.0))
                            .rounded(px(radius::SMALL))
                            .bg(theme::swatch(palette::ACCENT_TINT))
                            .child(icons::icon(
                                icon,
                                metrics.small_icon_size(),
                                theme::swatch(palette::ACCENT_ON_TINT),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(metrics.label_size())
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                    .debug_selector(move || kind.clone())
                                    .child(SharedString::from(title)),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(metrics.caption_size())
                                    .text_color(theme::swatch(palette::TEXT_MUTED))
                                    .child(SharedString::from(detail)),
                            ),
                    )
                    .when(lit, |row| {
                        row.child(components::keycap(ux.home_quick_enter, metrics))
                    }),
            );
        }
        panel
    }

    // -----------------------------------------------------------------------
    // The group dialog
    // -----------------------------------------------------------------------

    /// Creating, renaming, moving into and deleting a group, as one dialog.
    ///
    /// Deleting asks for a destination first: the hosts move, they are never deleted with
    /// the group (R18).
    pub(super) fn group_dialog_overlay(
        &self,
        window: &gpui::Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let editor = self.hosts.group_editor()?;
        let metrics = self.metrics();
        let ux = copy::ux();
        let strings = self.strings();
        let action = editor.action().clone();
        let selector = action.debug_selector();
        let title = match &action {
            GroupAction::Create => ux.hosts_group_new,
            GroupAction::Rename(_) => ux.hosts_group_rename,
            GroupAction::Move => ux.hosts_group_move,
            GroupAction::Delete(_) => ux.hosts_group_delete,
        };
        let (header, subtitle) = self.modal_header(
            Some(Icon::Folder),
            title.to_owned(),
            None,
            "hosts.group.close",
            cx,
            |this, cx| {
                this.close_group_dialog(cx);
            },
        );

        let mut body = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::LOOSE))
            .debug_selector(move || selector.to_owned())
            .child(
                components::field_row(ux.hosts_group_name_label, metrics).child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .debug_selector(|| "hosts.group.name".to_owned())
                        .children(self.group_name_field.clone()),
                ),
            );
        if matches!(action, GroupAction::Delete(_) | GroupAction::Move) {
            let hosts = match &action {
                // Every connection the group holds: the servers and the desktop or
                // serial records, which move with it.
                GroupAction::Delete(path) => {
                    self.book.hosts_under_group(path).len()
                        + self.book.connections_under_group(path).len()
                }
                _ => self.hosts.selected_count(),
            };
            body = body
                .child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .debug_selector(|| "hosts.group.body".to_owned())
                        .child(SharedString::from(copy::fill(
                            ux.hosts_group_delete_body,
                            &[("count", &hosts.to_string())],
                        ))),
                )
                .child(
                    components::field_row(ux.hosts_group_destination_label, metrics).child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .debug_selector(|| "hosts.group.destination".to_owned())
                            .children(self.group_destination_field.clone()),
                    ),
                )
                .child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .child(ux.hosts_group_destination_hint),
                );
        }
        if let Some(reason) = editor.error() {
            body = body.child(
                components::banner(Tone::Danger, metrics)
                    .debug_selector(|| "hosts.group.error".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(reason.to_owned())),
            );
        }
        let confirm = match &action {
            GroupAction::Create => ux.hosts_group_create_action,
            GroupAction::Rename(_) => ux.hosts_group_rename_action,
            GroupAction::Move => ux.hosts_group_move_action,
            GroupAction::Delete(_) => ux.hosts_group_delete_action,
        };
        let footer = div()
            .flex()
            .justify_end()
            .gap(metrics.scale(spacing::TIGHT))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::LOOSE))
            .child(
                components::compact_button(strings.host_cancel, Variant::Ghost, metrics)
                    .id("hosts.group.cancel")
                    .debug_selector(|| "hosts.group.cancel".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.close_group_dialog(cx);
                    })),
            )
            .child(
                components::compact_button(confirm, Variant::Primary, metrics)
                    .id("hosts.group.confirm")
                    .debug_selector(|| "hosts.group.confirm".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.confirm_group_dialog(cx);
                    })),
            );

        Some(self.modal(
            window,
            "hosts.group.dialog",
            460.0,
            ModalContent {
                header,
                subtitle,
                body: body.into_any_element(),
                footer: Some(footer),
            },
        ))
    }
}

/// The label of a list view, for the count line.
fn view_label(view: crate::app::hosts::HostView, strings: &Strings) -> &'static str {
    view.label(strings)
}

/// One group header inside the list: the chevron, the name and the count.
fn group_header_row(
    group: &HostGroupRow,
    number: usize,
    row_height: gpui::Pixels,
    metrics: Metrics,
    strings: &'static Strings,
    weak: &WeakEntity<ShelluneView>,
) -> Stateful<Div> {
    // The hosts without a group are a place in the list like any group, and they are
    // named as what they are instead of borrowing the "every group" label of the filter.
    let label = group.path.as_ref().map_or_else(
        || strings.hosts_ungrouped.to_owned(),
        GroupPath::to_canonical,
    );
    let chevron = if group.expanded {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    let toggled = weak.clone();
    let path = group.path.clone();
    let clicked = weak.clone();
    let click_path = group.path.clone();
    // A header is one row of the uniform list, so it keeps the row's height exactly
    // and spans the list's width: a section divider the eye can follow, not a
    // floating label. The bar sits at the bottom of its row, so the gap above it
    // separates the group from the one before.
    div()
        .h(row_height)
        .w_full()
        .id(SharedString::from(format!("hosts.group.row.{number}")))
        .debug_selector(move || format!("hosts.group.{number}"))
        .flex()
        .items_end()
        .pb(metrics.scale(spacing::HAIRLINE))
        .child(
            div()
                .id(SharedString::from(format!("hosts.group.bar.{number}")))
                .flex()
                .flex_1()
                .min_w(px(0.0))
                .items_center()
                .gap(metrics.scale(spacing::TIGHT))
                .h(metrics.scale(32.0))
                .pl(metrics.scale(spacing::HAIRLINE + group.depth as f32 * 14.0))
                .pr(metrics.scale(spacing::TIGHT))
                .border_b_1()
                .border_color(theme::swatch(palette::BORDER))
                // The whole header folds its group, not only the chevron.
                .cursor_pointer()
                .on_click(move |_event, _window, cx| {
                    let path = click_path.clone();
                    let _ = clicked.update(cx, |view, cx| view.toggle_group_row(path, cx));
                })
                .child(group_header_body(
                    group, number, label, chevron, path, toggled, metrics,
                )),
        )
}

/// The chevron, name and count of a group header.
fn group_header_body(
    group: &HostGroupRow,
    number: usize,
    label: String,
    chevron: Icon,
    path: Option<GroupPath>,
    toggled: WeakEntity<ShelluneView>,
    metrics: Metrics,
) -> Div {
    div()
        .flex()
        .flex_1()
        .min_w(px(0.0))
        .items_center()
        .gap(metrics.scale(spacing::TIGHT))
        .child(
            components::icon_only_button(chevron, metrics)
                .size(metrics.scale(18.0))
                .id(SharedString::from(format!("hosts.group.{number}.toggle")))
                .debug_selector(move || format!("hosts.group.{number}.toggle"))
                .on_click(move |_event, _window, cx| {
                    cx.stop_propagation();
                    let path = path.clone();
                    let _ = toggled.update(cx, |view, cx| view.toggle_group_row(path, cx));
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_size(metrics.label_size())
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .child(SharedString::from(label)),
        )
        .child(components::badge(group.hosts, Tone::Neutral, metrics))
}

/// One host inside the list: who it is, what it dials, its group and what can be done.
fn host_row_element(
    host: &HostRowView,
    weak: &WeakEntity<ShelluneView>,
    row_height: gpui::Pixels,
    prefix: &'static str,
    metrics: Metrics,
    strings: &'static Strings,
    ux: &'static UxStrings,
) -> Stateful<Div> {
    let number = host.number;
    let id = host.id;
    let selected = host.selected;
    let cursor = host.cursor;
    let show_duplicate = host.show_duplicate;
    let hover = theme::swatch(palette::SURFACE_HOVER);
    let pick = weak.clone();
    let connect = weak.clone();
    let forwards = weak.clone();
    let edit = weak.clone();
    let duplicate = weak.clone();
    let delete = weak.clone();
    let open_row = weak.clone();
    let menu_row = weak.clone();
    // A row is a line on the page, not a card: the list reads as one surface and the
    // pointer, the selection and the keyboard are what lift a row out of it.
    let mut row = div()
        .id(SharedString::from(format!("hosts.row.id.{number}")))
        .debug_selector(move || format!("{prefix}.{number}"))
        .group("host-row")
        .h(row_height)
        .flex()
        .items_center()
        .gap(metrics.scale(spacing::SNUG))
        .w_full()
        .min_w(px(0.0))
        .px(metrics.scale(spacing::TIGHT + 2.0))
        .rounded(px(radius::MEDIUM))
        .border_1()
        // A picked row is a soft tint: picked rows sit next to each other, and an
        // outline on each of them stacked into a grid of borders.
        .when(selected && !cursor, |row| {
            row.bg(theme::swatch(palette::ACCENT_TINT))
                .border_color(theme::clear())
        })
        .when(selected && cursor, |row| {
            row.bg(theme::swatch(palette::ACCENT_TINT))
                .border_color(theme::faded(theme::swatch(palette::ACCENT), 0.5))
        })
        .when(!selected && cursor, |row| {
            row.bg(theme::swatch(palette::SURFACE_SELECTED))
                .border_color(theme::faded(theme::swatch(palette::ACCENT), 0.5))
        })
        .when(!selected && !cursor, |row| {
            row.border_color(theme::clear())
                .hover(move |style| style.bg(hover))
        })
        // A single click points at the row; a double click opens it; a right click
        // offers every action on the record.
        .on_click(move |event, _window, cx| {
            if event.click_count() >= 2 {
                let _ = open_row.update(cx, |view, cx| view.connect_saved_host(id, cx));
            } else {
                let _ = open_row.update(cx, |view, cx| view.point_at_host(id, cx));
            }
        })
        .on_mouse_down(
            gpui::MouseButton::Right,
            move |event: &gpui::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                let _ = menu_row.update(cx, |view, cx| view.open_host_menu(id, event, window, cx));
            },
        )
        // The checkbox shares the avatar's place, as in a mail client: the avatar
        // shows at rest, and the box takes its place under the pointer, on the
        // keyboard's row, or once a selection has started - so picking hosts costs
        // the list no column of empty boxes.
        .child({
            let picking = selected || cursor || host.selecting;
            let avatar = super::host_avatar(host.name.as_ref(), selected, metrics);
            div()
                .relative()
                .flex_none()
                .size(metrics.scale(36.0))
                .child(if picking {
                    avatar.opacity(0.0)
                } else {
                    avatar.group_hover("host-row", |style| style.opacity(0.0))
                })
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            checkbox(selected, metrics)
                                .when(!picking, |boxed| {
                                    boxed
                                        .opacity(0.0)
                                        .group_hover("host-row", |style| style.opacity(1.0))
                                })
                                .id(SharedString::from(format!("hosts.select.{number}")))
                                .debug_selector(move || format!("hosts.select.{number}"))
                                .tooltip(components::tooltip(ux.hosts_select_start))
                                .on_click(move |_event, _window, cx| {
                                    cx.stop_propagation();
                                    let _ = pick
                                        .update(cx, |view, cx| view.toggle_host_selection(id, cx));
                                }),
                        ),
                )
        })
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.0))
                .gap(metrics.scale(1.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(metrics.scale(spacing::HAIRLINE + 2.0))
                        .min_w(px(0.0))
                        .child(
                            div()
                                .truncate()
                                .text_size(metrics.body_size())
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                .child(host.name.clone()),
                        )
                        .when(host.open, |line| {
                            line.child(components::chip(
                                ux.hosts_status_connected,
                                Tone::Positive,
                                metrics,
                            ))
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(metrics.scale(spacing::HAIRLINE + 2.0))
                        .min_w(px(0.0))
                        .child(
                            components::copy_on_click(
                                div()
                                    .min_w(px(0.0))
                                    .truncate()
                                    .font_family(crate::app::terminal::monospace_family())
                                    .text_size(metrics.caption_size())
                                    .text_color(theme::swatch(palette::TEXT_MUTED)),
                                host.target.to_string(),
                            )
                            .child(host.target.clone()),
                        )
                        .children(host.last.clone().map(|last| {
                            let exact = host.last_exact.clone().unwrap_or_default();
                            div()
                                .id(SharedString::from(format!("hosts.last.{number}")))
                                .truncate()
                                .min_w(px(0.0))
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED))
                                .tooltip(components::tooltip(exact))
                                .child(SharedString::from(format!("· {last}")))
                        })),
                ),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(metrics.scale(spacing::HAIRLINE))
                .children(host.group.clone().map(|group| {
                    components::tag(group, metrics)
                        .id(SharedString::from(format!("hosts.group.tag.{number}")))
                })),
        );

    // The tools of a host are quiet icons; the one thing a row is for - connecting - is
    // the labelled button at its end. Every press stops at its button, so a quick
    // double press on an icon never also opens the host underneath it.
    // They appear with the pointer or the keyboard cursor, so a list at rest shows
    // names and the one labelled action, not a column of tool icons per host.
    let tools = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(metrics.scale(2.0))
        .when(!cursor, |tools| {
            tools
                .opacity(0.0)
                .group_hover("host-row", |style| style.opacity(1.0))
        })
        .child(
            components::icon_only_button(Icon::Forward, metrics)
                .size(metrics.scale(26.0))
                .id(SharedString::from(format!("hosts.forwards.{number}")))
                .debug_selector(move || format!("hosts.forwards.{number}"))
                .tooltip(components::tooltip(strings.forward_open))
                .on_click(move |_event, _window, cx| {
                    cx.stop_propagation();
                    let _ = forwards.update(cx, |view, cx| view.open_saved_forwards(id, cx));
                }),
        )
        .child(
            components::icon_only_button(Icon::Pencil, metrics)
                .size(metrics.scale(26.0))
                .id(SharedString::from(format!("hosts.edit.{number}")))
                .debug_selector(move || format!("hosts.edit.{number}"))
                .tooltip(components::tooltip(strings.host_edit))
                .on_click(move |_event, _window, cx| {
                    cx.stop_propagation();
                    let _ = edit.update(cx, |view, cx| view.edit_saved_host(id, cx));
                }),
        )
        .when(show_duplicate, |tools| {
            tools.child(
                components::icon_only_button(Icon::Copy, metrics)
                    .size(metrics.scale(26.0))
                    .id(SharedString::from(format!("hosts.duplicate.{number}")))
                    .debug_selector(move || format!("hosts.duplicate.{number}"))
                    .tooltip(components::tooltip(strings.connection_duplicate))
                    .on_click(move |_event, _window, cx| {
                        cx.stop_propagation();
                        let _ = duplicate
                            .update(cx, |view, cx| view.duplicate_saved_connection(id, cx));
                    }),
            )
        })
        .child(
            components::danger_icon_button(Icon::Trash, metrics)
                .size(metrics.scale(26.0))
                .id(SharedString::from(format!("hosts.delete.{number}")))
                .debug_selector(move || format!("hosts.delete.{number}"))
                .tooltip(components::tooltip(strings.host_delete))
                .on_click(move |_event, _window, cx| {
                    cx.stop_propagation();
                    let _ = delete.update(cx, |view, cx| view.ask_delete_host(id, cx));
                }),
        );
    row = row.child(favorite_star(
        host.favorite,
        cursor,
        format!("hosts.favorite.{number}"),
        id,
        weak,
        metrics,
    ));
    row = row.child(tools);
    row.child(
        components::icon_label_button(Icon::Plug, strings.remote_connect, Variant::Soft, metrics)
            .h(metrics.compact_control_height())
            .text_size(metrics.caption_size())
            .id(SharedString::from(format!("hosts.connect.{number}")))
            .debug_selector(move || format!("hosts.connect.{number}"))
            .tooltip(components::tooltip(ux.hosts_connect_hint))
            .on_click(move |_event, _window, cx| {
                cx.stop_propagation();
                let _ = connect.update(cx, |view, cx| view.connect_saved_host(id, cx));
            }),
    )
}

/// One saved connection of another protocol inside the list.
///
/// The row says what the connection is and what it would reach, and offers the three
/// things that belong to a record: edit it, copy it, delete it. Connecting is the row's
/// one main action, said with the same labelled button a host row ends with, and it
/// starts the record's own protocol instead of opening a form; there is no checkbox,
/// because a batch or group move only takes SSH hosts.
fn connection_row_element(
    connection: &ConnectionRowView,
    weak: &WeakEntity<ShelluneView>,
    row_height: gpui::Pixels,
    metrics: Metrics,
    strings: &'static Strings,
) -> Stateful<Div> {
    let number = connection.number;
    let id = connection.id;
    let cursor = connection.cursor;
    let hover = theme::swatch(palette::SURFACE_HOVER);
    let edit = weak.clone();
    let connect = weak.clone();
    let open_row = weak.clone();
    let menu_row = weak.clone();
    let copy = weak.clone();
    let delete = weak.clone();
    div()
        .id(SharedString::from(format!("hosts.connection.id.{number}")))
        .debug_selector(move || format!("hosts.connection.{number}"))
        .group("host-row")
        .h(row_height)
        .flex()
        .items_center()
        .gap(metrics.scale(spacing::SNUG))
        .w_full()
        .min_w(px(0.0))
        .px(metrics.scale(spacing::TIGHT + 2.0))
        .rounded(px(radius::MEDIUM))
        .border_1()
        .when(cursor, |row| {
            row.bg(theme::swatch(palette::SURFACE_SELECTED))
                .border_color(theme::faded(theme::swatch(palette::ACCENT), 0.5))
        })
        .when(!cursor, |row| {
            row.border_color(theme::clear())
                .hover(move |style| style.bg(hover))
        })
        // A single press points at the row; a double press connects it; a right press
        // opens the record's menu.
        .on_click(move |event, _window, cx| {
            if event.click_count() >= 2 {
                let _ = open_row.update(cx, |view, cx| view.connect_saved_connection(id, cx));
            } else {
                let _ = open_row.update(cx, |view, cx| view.point_at_host(id, cx));
            }
        })
        .on_mouse_down(
            gpui::MouseButton::Right,
            move |event: &gpui::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                let _ = menu_row.update(cx, |view, cx| view.open_host_menu(id, event, window, cx));
            },
        )
        .child(super::host_avatar(connection.name.as_ref(), false, metrics))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.0))
                .gap(metrics.scale(1.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(metrics.scale(spacing::HAIRLINE + 2.0))
                        .min_w(px(0.0))
                        .child(
                            div()
                                .truncate()
                                .text_size(metrics.body_size())
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                .child(connection.name.clone()),
                        )
                        .child(components::tag(connection.protocol.clone(), metrics))
                        // A running session of this record says so, exactly as a host
                        // row does, and a record that was only saved says nothing (R2).
                        .when(connection.open, |line| {
                            line.child(
                                components::chip(
                                    copy::ux().hosts_status_connected,
                                    Tone::Positive,
                                    metrics,
                                )
                                .debug_selector(move || format!("hosts.connection.open.{number}")),
                            )
                        }),
                )
                .child(
                    div().flex().items_center().min_w(px(0.0)).child(
                        components::copy_on_click(
                            div()
                                .truncate()
                                .font_family(crate::app::terminal::monospace_family())
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED)),
                            connection.target.to_string(),
                        )
                        .child(connection.target.clone()),
                    ),
                ),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(metrics.scale(spacing::HAIRLINE))
                .children(
                    connection
                        .group
                        .clone()
                        .map(|group| components::tag(group, metrics)),
                ),
        )
        .child(favorite_star(
            connection.favorite,
            cursor,
            format!("hosts.connection.favorite.{number}"),
            id,
            weak,
            metrics,
        ))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(metrics.scale(2.0))
                .when(!cursor, |tools| {
                    tools
                        .opacity(0.0)
                        .group_hover("host-row", |style| style.opacity(1.0))
                })
                .child(
                    components::icon_only_button(Icon::Pencil, metrics)
                        .size(metrics.scale(26.0))
                        .id(SharedString::from(format!(
                            "hosts.connection.edit.{number}"
                        )))
                        .debug_selector(move || format!("hosts.connection.edit.{number}"))
                        .tooltip(components::tooltip(strings.host_edit))
                        .on_click(move |_event, _window, cx| {
                            cx.stop_propagation();
                            let _ = edit.update(cx, |view, cx| view.edit_saved_connection(id, cx));
                        }),
                )
                .child(
                    components::icon_only_button(Icon::Copy, metrics)
                        .size(metrics.scale(26.0))
                        .id(SharedString::from(format!(
                            "hosts.connection.copy.{number}"
                        )))
                        .debug_selector(move || format!("hosts.connection.copy.{number}"))
                        .tooltip(components::tooltip(strings.connection_duplicate))
                        .on_click(move |_event, _window, cx| {
                            cx.stop_propagation();
                            let _ =
                                copy.update(cx, |view, cx| view.duplicate_saved_connection(id, cx));
                        }),
                )
                .child(
                    components::danger_icon_button(Icon::Trash, metrics)
                        .size(metrics.scale(26.0))
                        .id(SharedString::from(format!(
                            "hosts.connection.delete.{number}"
                        )))
                        .debug_selector(move || format!("hosts.connection.delete.{number}"))
                        .tooltip(components::tooltip(strings.connection_delete))
                        .on_click(move |_event, _window, cx| {
                            cx.stop_propagation();
                            let _ = delete.update(cx, |view, cx| view.ask_delete_host(id, cx));
                        }),
                ),
        )
        .child(
            components::icon_label_button(
                Icon::Plug,
                strings.remote_connect,
                Variant::Soft,
                metrics,
            )
            .h(metrics.compact_control_height())
            .text_size(metrics.caption_size())
            .id(SharedString::from(format!(
                "hosts.connection.connect.{number}"
            )))
            .debug_selector(move || format!("hosts.connection.connect.{number}"))
            .tooltip(components::tooltip(
                crate::app::copy::ux().hosts_connect_hint,
            ))
            .on_click(move |_event, _window, cx| {
                cx.stop_propagation();
                let _ = connect.update(cx, |view, cx| view.connect_saved_connection(id, cx));
            }),
        )
}

/// The frame of one row of the group column, chosen or not.
fn tree_row_frame(chosen: bool, metrics: Metrics) -> Div {
    let hover = theme::swatch(palette::SURFACE_HOVER);
    div()
        .flex()
        .items_center()
        .gap(metrics.scale(spacing::HAIRLINE + 2.0))
        .min_h(metrics.scale(28.0))
        .pr(metrics.scale(spacing::TIGHT))
        .rounded(px(radius::SMALL))
        .cursor_pointer()
        .when(chosen, |row| {
            row.bg(theme::swatch(palette::SURFACE_SELECTED))
        })
        .when(!chosen, |row| row.hover(move |style| style.bg(hover)))
}

/// The name in a row of the group column.
fn tree_row_label(label: String, chosen: bool, metrics: Metrics) -> Div {
    div()
        .flex_1()
        .min_w(px(0.0))
        .truncate()
        .text_size(metrics.label_size())
        .when(chosen, |label| label.font_weight(FontWeight::MEDIUM))
        .text_color(theme::swatch(if chosen {
            palette::TEXT_PRIMARY
        } else {
            palette::TEXT_SECONDARY
        }))
        .child(SharedString::from(label))
}

/// A selection box: a hollow square that fills with the accent and a tick when chosen.
///
/// It is drawn as what it is - a checkbox - rather than borrowing a chevron, which reads
/// as "open this" and hid the fact that rows can be picked for a batch at all.
fn checkbox(checked: bool, metrics: Metrics) -> Div {
    let size = metrics.scale(16.0);
    let hit = metrics.hit_target();
    let border = theme::swatch(palette::ACCENT);
    let mut boxed = div()
        .flex()
        .items_center()
        .justify_center()
        .size(size)
        .rounded(px(4.0))
        .border_1();
    boxed = if checked {
        boxed
            .bg(theme::swatch(palette::ACCENT))
            .border_color(theme::swatch(palette::ACCENT))
            .child(crate::app::icons::icon(
                Icon::Check,
                metrics.scale(12.0),
                theme::swatch(palette::ON_ACCENT),
            ))
    } else {
        boxed
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .group_hover("host-checkbox", move |style| style.border_color(border))
    };
    div()
        .group("host-checkbox")
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(hit)
        .cursor_pointer()
        .child(boxed)
}

/// The frame of one dashboard card: a raised surface, a row of its contents, and the
/// pointer's lift.
fn home_card_frame(metrics: Metrics) -> Div {
    let hover_border = theme::swatch(palette::BORDER_STRONG);
    let hover_bg = theme::swatch(palette::SURFACE_HOVER);
    div()
        .flex()
        .items_center()
        .w_full()
        .min_w(px(0.0))
        .gap(metrics.scale(spacing::TIGHT + 2.0))
        .px(metrics.scale(spacing::TIGHT + 2.0))
        .py(metrics.scale(spacing::HAIRLINE + 2.0))
        .rounded(px(radius::LARGE))
        .bg(theme::swatch(palette::SURFACE_RAISED))
        .border_1()
        .border_color(theme::swatch(palette::BORDER))
        .cursor_pointer()
        .hover(move |style| style.border_color(hover_border).bg(hover_bg))
}

/// The favourite mark of a row: a filled star that stays in view on a favourite, an
/// outline that appears with the pointer or the keyboard cursor on any other (R1).
///
/// The press stops at the star, so marking a host never also points at or opens it.
fn favorite_star(
    favorite: bool,
    cursor: bool,
    selector: String,
    id: HostId,
    weak: &WeakEntity<ShelluneView>,
    metrics: Metrics,
) -> Stateful<Div> {
    let ux = copy::ux();
    let hover = theme::swatch(palette::SURFACE_HOVER);
    let toggled = weak.clone();
    let element_id = SharedString::from(selector.clone());
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(metrics.scale(26.0))
        .rounded(px(radius::SMALL))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .when(!favorite && !cursor, |star| {
            star.opacity(0.0)
                .group_hover("host-row", |style| style.opacity(1.0))
        })
        .child(icons::icon(
            if favorite {
                Icon::StarFilled
            } else {
                Icon::Star
            },
            metrics.small_icon_size(),
            theme::swatch(if favorite {
                palette::ACCENT
            } else {
                palette::TEXT_SECONDARY
            }),
        ))
        .id(element_id)
        .debug_selector(move || selector.clone())
        .tooltip(components::tooltip(if favorite {
            ux.favorite_remove
        } else {
            ux.favorite_add
        }))
        .on_click(move |_event, _window, cx| {
            cx.stop_propagation();
            let _ = toggled.update(cx, |view, cx| view.toggle_favorite(id, cx));
        })
}
