//! The hosts home (ADR-3): a sidebar of places, one toolbar with the page's only search,
//! and one virtualized list.
//!
//! The page is drawn from one row model (crate::app::hosts::HostList): the hosts and the
//! other connections the place, the search and the filters keep, flattened into rows that
//! name a HostId. The rows are drawn through gpui's uniform_list, which asks for the slice
//! it is about to show instead of the whole list, so five thousand saved hosts cost the
//! elements of the visible screen and nothing more (R18, ADR-8).
//!
//! Three properties hold at every window width:
//!
//! * a row is identified by its host, never by its position, so a search, a sort or a
//!   reload cannot move a selection onto another host;
//! * there is one search on the page. It filters the list as it is typed, and text that
//!   reads as `user@host[:port]` puts a "connect to …" row on top that Enter dials;
//! * nothing here asks a server anything. The connection state the rows show and the
//!   status filter narrows by are read from the sessions this window already has and the
//!   recent-connection log the host file carries.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Div, IntoElement, Pixels, Point, SharedString, Stateful, WeakEntity,
    Window, div, px, uniform_list,
};
use shellune_core::{GroupPath, HostId, ProtocolKind, SessionKind, SessionState};

use super::shell::ModalContent;
use crate::app::components::{self, ControlState, Variant};
use crate::app::copy::{self, Strings, UxStrings};
use crate::app::hosts::{
    GroupAction, HomeSection, HostList, HostListRow, HostProtocolFilter, HostSort,
    HostStatusFilter, HostView, QuickTarget, SelectionScope, SidebarGroupRow, format_unix,
    parse_quick_target,
};
use crate::app::icons::{self, Icon};
use crate::app::nav::Destination;
use crate::app::theme::{self, Metrics, Tone, palette, radius, spacing};
use crate::app::view::ShelluneView;
use crate::app::workspace::SectionState;

/// How wide the sidebar is when it shows its words, before zoom.
pub(crate) const SIDEBAR_WIDTH: f32 = 220.0;

/// How wide the sidebar is when it is folded to its icons, before zoom.
pub(crate) const SIDEBAR_FOLDED_WIDTH: f32 = 56.0;

/// The window width below which the sidebar starts folded.
///
/// Narrower than this the words would take more room than the hosts they navigate; the
/// icons reach the same places with one press, and the fold button opens the words again.
const SIDEBAR_FOLD_BELOW: f32 = 900.0;

/// How far one level of the group tree is indented, before zoom.
const GROUP_INDENT: f32 = 12.0;

/// The edge of a group's open/close chevron, before zoom.
const CHEVRON_SIZE: f32 = 18.0;

/// The hover group of a sidebar group row, which reveals the row's "⋯".
const GROUP_ROW: &str = "hosts-tree-row";

/// The window height below which the page scrolls as one column, before zoom.
const SHORT_HEIGHT: f32 = 320.0;

/// Below this width a row leaves out its duplicate tool, before zoom.
const ROW_DUPLICATE_MIN_WIDTH: f32 = 760.0;

/// Below this width a row leaves out its group tag and its time, before zoom.
const ROW_META_MIN_WIDTH: f32 = 600.0;

/// The width of a row's group-tag column, before zoom. A longer tag ends in an ellipsis
/// inside it, so every row's tag starts and its time ends at the same x.
const ROW_TAG_WIDTH: f32 = 128.0;

/// The width of a row's last-connected column, before zoom: room for the longest
/// relative time ("59 min ago", "29 days ago") in either language.
const ROW_TIME_WIDTH: f32 = 88.0;

/// The least room a row keeps for its name and address before its tools give way.
const ROW_TEXT_MIN_WIDTH: f32 = 96.0;

/// The narrowest the search is drawn, before zoom.
const SEARCH_MIN_WIDTH: f32 = 160.0;

/// The widest the page title is drawn: a long group name gives way to the search.
const TITLE_MAX_WIDTH: f32 = 240.0;

/// How wide the toolbar's menus are, before zoom.
const MENU_WIDTH: f32 = 232.0;

/// A menu of the home page: the toolbar's two and a group's own.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HomeMenu {
    /// The status filter and the sort, under the toolbar's funnel.
    Filter,
    /// Importing and batch tasks, under the toolbar's "⋯".
    More,
    /// Renaming or deleting one group, where the sidebar row was right-pressed.
    Group(GroupPath, Point<Pixels>),
}

/// One row of the list, as the window draws it.
enum SnapshotRow {
    /// The unsaved `user@host[:port]` the search reads as an address.
    Direct(DirectRowView),
    /// A saved host.
    Host(HostRowView),
    /// A saved connection of another protocol.
    Connection(ConnectionRowView),
}

/// The "connect to …" row on top of a search that reads as an address.
struct DirectRowView {
    /// "Connect to user@host:port".
    title: SharedString,
    /// That nothing is saved.
    hint: SharedString,
    /// Whether Enter in the search opens it: no row has the keyboard cursor.
    enter: bool,
}

/// One connection of another protocol, with what its row shows (R2, R18).
///
/// It has no checkbox: a selection is a batch or group-move target, and both are SSH
/// operations, so a desktop or serial record is never silently pulled into one.
struct ConnectionRowView {
    id: HostId,
    /// The row's number among the connection rows, for its selector.
    number: usize,
    name: SharedString,
    /// The protocol, as the tag beside the name says it.
    protocol: SharedString,
    target: SharedString,
    group: Option<SharedString>,
    /// Whether this window already holds a running session for the record.
    open: bool,
    /// Whether the keyboard cursor is on this row.
    cursor: bool,
    /// Whether the record is marked as a favourite (R1).
    favorite: bool,
    /// Whether the window is wide enough for the copy tool, as for a host row, so both
    /// kinds of row keep their columns at the same x.
    show_duplicate: bool,
    /// Whether the window is wide enough for the tag and time columns.
    show_meta: bool,
    /// Whether any SSH host is currently selected, showing the SSH-only multi-select boundary.
    selecting: bool,
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
    /// The group the host is in, when it is in one and the row has room for it.
    group: Option<SharedString>,
    /// Whether this window already has a session to it.
    open: bool,
    /// When the book last recorded a connection to it, said relatively ("3 hours ago"),
    /// when it ever did and the row has room for it.
    last: Option<SharedString>,
    /// The exact stamp of that connection and how many there were, for the hover.
    last_exact: Option<SharedString>,
    /// Whether the user picked it.
    selected: bool,
    /// Whether the keyboard cursor is on this row.
    cursor: bool,
    /// Whether the row has room for the duplicate tool beside edit and delete.
    show_duplicate: bool,
    /// Whether the window is wide enough for the tag and time columns.
    show_meta: bool,
    /// Whether any host is picked, which keeps every checkbox in view: once a
    /// selection has started, picking the next host must not need a hover first.
    selecting: bool,
    /// Whether the host is marked as a favourite (R1).
    favorite: bool,
    /// The chip and its tooltip when the private key file the host names is not on this
    /// computer: a record that came from another machine keeps its path and says so,
    /// rather than failing only once a connection is tried (R9).
    key_missing: Option<(SharedString, SharedString)>,
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
    /// never started is not drawn as online.
    pub(super) fn connection_is_open(&self, id: HostId) -> bool {
        self.workspace.sessions().items().iter().any(|session| {
            matches!(
                session.kind(),
                SessionKind::Rdp { host } | SessionKind::Vnc { host } | SessionKind::Serial { host }
                    if *host == id
            ) && matches!(session.state(), SessionState::Connected)
        })
    }

    /// Whether any saved record (SSH, RDP, VNC or Serial) has an open session in this window (R11).
    pub(super) fn record_is_open(&self, id: HostId) -> bool {
        self.host_is_open(id) || self.connection_is_open(id)
    }

    // -----------------------------------------------------------------------
    // The places, the search and the menus
    // -----------------------------------------------------------------------

    /// Whether the sidebar is folded to its icons: what the user chose last, or, until
    /// they chose, whether the window is narrow.
    #[must_use]
    pub fn home_sidebar_folded(&self) -> bool {
        self.home_sidebar_fold
            .unwrap_or_else(|| self.viewport_width < self.metrics().scale(SIDEBAR_FOLD_BELOW))
    }

    /// Folds the sidebar to its icons, or opens its words again.
    pub(super) fn toggle_home_sidebar(&mut self, cx: &mut Context<Self>) {
        self.home_sidebar_fold = Some(!self.home_sidebar_folded());
        cx.notify();
    }

    /// Shows one place of the sidebar: every record, the favourites, the recent hosts or
    /// one group. The list starts again from its top and the keyboard cursor is dropped,
    /// since the row it was on may not be in the new place.
    pub fn show_home_section(&mut self, section: HomeSection, cx: &mut Context<Self>) {
        self.hosts.set_section(&section);
        if let HomeSection::Group(path) = &section {
            self.hosts.set_expanded(path, true);
        }
        self.hosts_cursor = None;
        self.home_menu = None;
        self.hosts_scroll
            .scroll_to_item(0, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    /// The address the search reads as, when it reads as one (`user@host[:port]`).
    pub(super) fn home_direct_target(&self) -> Option<QuickTarget> {
        parse_quick_target(self.hosts.search())
    }

    /// Whether the list draws the "connect to …" row on top of its records: the search
    /// reads as an address and something else is listed under it. With nothing listed,
    /// the empty state offers the same connection as its action instead.
    pub(super) fn home_direct_row(&self, list: &HostList) -> Option<QuickTarget> {
        self.home_direct_target().filter(|_| !list.is_empty())
    }

    /// Whether the "connect to …" row is on screen now, for a key that has no list.
    pub(super) fn home_direct_row_shown(&self) -> bool {
        self.home_direct_target().is_some()
            && !self
                .hosts
                .list(&self.book, &self.groups, &|id| self.record_is_open(id))
                .is_empty()
    }

    /// Dials the address the search reads as, without saving it (ADR-3).
    ///
    /// The search is emptied first, so the list is whole again when the page comes back.
    /// Enter is handled inside the field, which empties its own text when told the press
    /// was used, so `from_field` leaves the field alone; a press on a row empties it
    /// here. Returns whether there was an address to dial.
    pub(super) fn connect_home_direct(
        &mut self,
        from_field: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(target) = self.home_direct_target() else {
            return false;
        };
        if !from_field {
            self.hosts_search
                .update(cx, |field, cx| field.set_text(String::new(), cx));
        }
        self.hosts.set_search(String::new());
        self.hosts_cursor = None;
        self.connect_quick_target(&target, window, cx);
        cx.notify();
        true
    }

    /// Gives the keyboard to the page's search, with what it holds kept.
    pub(super) fn focus_hosts_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.hosts_search.read(cx).focus_handle().clone();
        window.focus(&handle);
        cx.notify();
    }

    /// Opens one of the home page's menus, or closes it when it is the one open.
    pub(super) fn toggle_home_menu(&mut self, menu: HomeMenu, cx: &mut Context<Self>) {
        let same = matches!(
            (&self.home_menu, &menu),
            (Some(HomeMenu::Filter), HomeMenu::Filter) | (Some(HomeMenu::More), HomeMenu::More)
        );
        self.shell_menu_open = false;
        self.host_menu = None;
        self.home_menu = if same { None } else { Some(menu) };
        cx.notify();
    }

    /// Closes the connection form a test opened from the list, as its own close does.
    #[cfg(test)]
    pub fn hide_connection_for_test(&mut self, cx: &mut Context<Self>) {
        self.hide_connection(cx);
    }

    /// Whether one of the home page's menus is open.
    #[cfg(test)]
    #[must_use]
    pub fn home_menu_open(&self) -> bool {
        self.home_menu.is_some()
    }

    /// Closes the home page's menu, when one is open.
    pub(super) fn close_home_menu(&mut self, cx: &mut Context<Self>) -> bool {
        if self.home_menu.take().is_some() {
            cx.notify();
            true
        } else {
            false
        }
    }

    /// The title of the page: the name of the place the list shows.
    fn home_title(&self) -> String {
        let strings = self.strings();
        match self.hosts.section() {
            HomeSection::All => strings.hosts_view_all.to_owned(),
            HomeSection::Favorites => copy::ux().home_favorites_section.to_owned(),
            HomeSection::Recent => strings.hosts_view_recent.to_owned(),
            HomeSection::Group(path) => path.to_canonical(),
        }
    }

    // -----------------------------------------------------------------------
    // The page
    // -----------------------------------------------------------------------

    /// The home page (ADR-3): the sidebar of places on the left, and the toolbar with the
    /// one search over the one list on the right.
    ///
    /// A host file that is still being read or could not be read is a state card with
    /// its reason and a retry, because "no hosts" and "the file is broken" are different
    /// answers; every other state is the sidebar and the list, a first run included,
    /// whose list is the welcome.
    pub(super) fn home_page(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let state = self.workspace.state(Destination::Hosts);
        // The page holds the keyboard of whichever destination shows it, so the
        // shortcuts keep working after a click on the page or a page switch.
        let page = div()
            .id("content.scroll")
            .debug_selector(|| "content".to_owned())
            .track_focus(self.destination_focus(self.workspace.destination()))
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .bg(theme::swatch(super::PAGE_FLOOR));
        if matches!(state, SectionState::Loading | SectionState::Failed) {
            let strings = self.strings();
            let mut card = self.section_card(Destination::Hosts, state);
            if state == SectionState::Failed {
                card = card.child(self.text_button(
                    SharedString::from("hosts.retry"),
                    strings.hosts_retry,
                    Variant::Primary,
                    cx,
                    |view, cx| view.reload_hosts(cx),
                ));
            }
            return page.flex_col().overflow_y_scroll().child(
                div()
                    .debug_selector(|| "home".to_owned())
                    .flex()
                    .flex_col()
                    .w_full()
                    .gap(metrics.scale(spacing::SNUG))
                    .p(metrics.scale(spacing::LOOSE))
                    .children(
                        self.local_error
                            .as_ref()
                            .map(|reason| self.notice_line(reason.clone(), Tone::Danger)),
                    )
                    .child(card),
            );
        }
        let list = self
            .hosts
            .list(&self.book, &self.groups, &|id| self.record_is_open(id));
        // Behind the fading start the sidebar arrives first and the list just after it
        // (R3); at any other time both are simply there.
        page.flex_row()
            .child(self.home_entrance(
                self.home_sidebar(&list, window, cx),
                "startup.page.sidebar",
                0,
            ))
            .child(self.home_entrance(self.home_main(&list, window, cx), "startup.page.list", 1))
    }

    /// The sidebar: every record, the favourites, the recent hosts, the groups, and the
    /// local terminal at its foot. Folded, it is a rail of the same places as icons.
    fn home_sidebar(&self, list: &HostList, window: &Window, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let folded = self.home_sidebar_folded();
        let section = self.hosts.section();
        let width = metrics.scale(if folded {
            SIDEBAR_FOLDED_WIDTH
        } else {
            SIDEBAR_WIDTH
        });

        let toggle = components::icon_only_button(Icon::Sidebar, metrics)
            .id("home.sidebar.toggle")
            .debug_selector(|| "home.sidebar.toggle".to_owned())
            .tooltip(components::tooltip(if folded {
                ux.home_sidebar_expand
            } else {
                ux.home_sidebar_collapse
            }))
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.toggle_home_sidebar(cx);
            }));

        let mut bar = div()
            .debug_selector(move || {
                if folded {
                    "home.sidebar.folded".to_owned()
                } else {
                    "home.sidebar".to_owned()
                }
            })
            .flex()
            .flex_col()
            .flex_none()
            .w(width)
            .min_h_0()
            .gap(metrics.scale(spacing::HALF))
            .p(metrics.scale(spacing::TIGHT))
            .bg(theme::swatch(palette::SURFACE))
            .border_r_1()
            .border_color(theme::swatch(palette::BORDER))
            .when(folded, |bar| bar.items_center())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .w_full()
                    .when(folded, |row| row.justify_center())
                    .when(!folded, |row| row.justify_end())
                    .pb(metrics.scale(spacing::HAIRLINE))
                    .child(toggle),
            );

        let ring = theme::swatch(palette::FOCUS_RING);
        let places = [
            (
                0usize,
                HomeSection::All,
                Icon::Server,
                strings.hosts_view_all,
                list.total(),
            ),
            (
                1usize,
                HomeSection::Favorites,
                Icon::Star,
                ux.home_favorites_section,
                self.book.favorite_hosts().len(),
            ),
            (
                2usize,
                HomeSection::Recent,
                Icon::Clock,
                strings.hosts_view_recent,
                self.book.recent_hosts().len(),
            ),
        ];
        for (slot, place, icon, label, count) in places {
            let chosen = section == place;
            let selector = place.debug_selector();
            let focus = self.sidebar_place_focus[slot].clone();
            let focus_click = focus.clone();
            let item = if folded {
                components::sidebar_icon_item(icon, chosen, metrics)
            } else {
                components::sidebar_item(icon, label, Some(count), chosen, metrics)
            };
            bar = bar.child(
                item.id(selector)
                    .debug_selector(move || selector.to_owned())
                    .track_focus(&focus)
                    .border_1()
                    .border_color(theme::clear())
                    .focus(move |style| style.border_color(ring))
                    .when(folded, |item| item.tooltip(components::tooltip(label)))
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        window.focus(&focus_click);
                        this.show_home_section(place.clone(), cx);
                    })),
            );
        }

        bar = bar.child(
            div()
                .flex_none()
                .w_full()
                .my(metrics.scale(spacing::HAIRLINE))
                .border_t_1()
                .border_color(theme::swatch(palette::BORDER)),
        );

        if folded {
            // The groups are one folder on the rail: pressing it opens the words, where
            // the tree is, rather than squeezing a tree into the width of an icon.
            let chosen = matches!(section, HomeSection::Group(_));
            bar = bar.child(
                components::sidebar_icon_item(Icon::Folder, chosen, metrics)
                    .id("home.sidebar.groups")
                    .debug_selector(|| "home.sidebar.groups".to_owned())
                    .tooltip(components::tooltip(ux.hosts_groups_title))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.home_sidebar_fold = Some(false);
                        cx.notify();
                    })),
            );
        } else {
            bar = bar.child(self.home_group_tree(&section, window, cx));
        }

        // The local terminal sits at the sidebar's foot: it is this computer, not a
        // place of the list (ADR-3).
        let local_open = self.shell_menu_open && self.shell_menu_home;
        let anchor = self.home_local_anchor.clone();
        let local = if folded {
            components::sidebar_icon_item(Icon::Terminal, local_open, metrics)
        } else {
            components::sidebar_item(
                Icon::Terminal,
                ux.home_local_button,
                None,
                local_open,
                metrics,
            )
        };
        let local_focus = self.sidebar_place_focus[3].clone();
        let local_focus_click = local_focus.clone();
        bar.when(folded, |bar| bar.child(div().flex_1().min_h_0()))
            .child(
            div()
                .relative()
                .flex()
                .flex_none()
                .w_full()
                .when(folded, |slot| slot.justify_center())
                .child(
                    local
                        .id("home.local")
                        .debug_selector(|| "home.local".to_owned())
                        .track_focus(&local_focus)
                        .border_1()
                        .border_color(theme::clear())
                        .focus(move |style| style.border_color(ring))
                        .when(!local_open, |item| {
                            item.tooltip(components::tooltip(if folded {
                                ux.home_local_button
                            } else {
                                ux.home_local_hint
                            }))
                        })
                        .on_click(cx.listener(move |this, _event, window, cx| {
                            window.focus(&local_focus_click);
                            this.home_menu = None;
                            this.toggle_home_local_menu(cx);
                        })),
                )
                // Records where the entry is drawn, for the menu that opens beside it.
                .child(
                    gpui::canvas(
                        move |bounds, _window, _cx| anchor.set(bounds),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                ),
        )
    }

    /// The groups of the sidebar: heading with "+", independent group locator, and a
    /// virtualized `uniform_list` tree with O(1) focus (R9, R10, ADR-6).
    fn home_group_tree(
        &self,
        section: &HomeSection,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let heading = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .w_full()
            .pl(metrics.scale(spacing::TIGHT))
            .child(components::section_label(ux.hosts_groups_title, metrics))
            .child(
                components::icon_only_button(Icon::Plus, metrics)
                    .id("hosts.group.new")
                    .debug_selector(|| "hosts.group.new".to_owned())
                    .tooltip(components::tooltip(ux.hosts_group_new))
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.home_menu = None;
                        this.open_group_dialog(GroupAction::Create, window, cx);
                    })),
            );

        let locator = div()
            .flex()
            .flex_none()
            .w_full()
            .min_w_0()
            .debug_selector(|| "hosts.groups.search".to_owned())
            .child(self.group_search.clone());

        let rows = self.sidebar_group_rows();
        let group_query_active = !self.hosts.group_query().trim().is_empty();
        let host_narrowing = !self.hosts.search().trim().is_empty()
            || self.hosts.filter().status() != HostStatusFilter::Any
            || self.hosts.filter().protocol() != HostProtocolFilter::All;

        let list_area = if rows.is_empty() {
            let mut empty_box = div()
                .id("hosts.groups.scroll")
                .debug_selector(|| "hosts.groups".to_owned())
                .track_focus(&self.group_list_focus)
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .gap(metrics.scale(spacing::HALF));
            if group_query_active {
                empty_box = empty_box.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(metrics.scale(spacing::HALF))
                        .px(metrics.scale(spacing::TIGHT))
                        .py(metrics.scale(spacing::HAIRLINE))
                        .debug_selector(|| "hosts.groups.nomatch".to_owned())
                        .child(
                            div()
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED))
                                .child(ux.hosts_group_no_match),
                        )
                        .child(
                            components::compact_button(
                                ux.hosts_group_search_clear,
                                Variant::Ghost,
                                metrics,
                            )
                            .id("hosts.groups.nomatch.clear")
                            .debug_selector(|| "hosts.groups.nomatch.clear".to_owned())
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.clear_group_query(window, cx);
                            })),
                        ),
                );
            } else if !host_narrowing {
                empty_box = empty_box.child(
                    div()
                        .px(metrics.scale(spacing::TIGHT))
                        .py(metrics.scale(spacing::HAIRLINE))
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .debug_selector(|| "hosts.groups.empty".to_owned())
                        .child(ux.hosts_group_empty),
                );
            }
            empty_box
        } else {
            let rows_rc = Rc::new(rows);
            let count = rows_rc.len();
            let weak = cx.entity().downgrade();
            let section_clone = section.clone();
            let list_focused = self.group_list_focus.is_focused(window)
                || self.group_search.read(cx).focus_handle().is_focused(window);
            let focused_path = self.focused_group_path.clone();
            let drawn = self.group_rows_drawn.clone();
            let row_height = metrics.control_height() + metrics.scale(spacing::HALF);
            let vlist = uniform_list("hosts.groups.list", count, move |range, _window, _cx| {
                drawn.set(drawn.get().max(range.len()));
                let mut items: Vec<AnyElement> = Vec::with_capacity(range.len());
                for index in range {
                    let row = &rows_rc[index];
                    let is_focused =
                        list_focused && focused_path.as_ref() == Some(&row.path);
                    items.push(
                        group_tree_row_element(
                            row,
                            index,
                            &section_clone,
                            is_focused,
                            row_height,
                            &weak,
                            metrics,
                        )
                        .into_any_element(),
                    );
                }
                items
            })
            .track_scroll(self.group_scroll.clone())
            .flex_1()
            .h_full()
            .w_full()
            .min_h_0()
            .debug_selector(|| "hosts.groups.list".to_owned());

            div()
                .id("hosts.groups.scroll")
                .debug_selector(|| "hosts.groups".to_owned())
                .track_focus(&self.group_list_focus)
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(vlist)
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .gap(metrics.scale(spacing::HALF))
            .child(heading)
            .child(locator)
            .child(list_area)
    }

    /// The virtualized sidebar group rows (R9, R10).
    pub(super) fn sidebar_group_rows(&self) -> Vec<SidebarGroupRow> {
        self.hosts
            .sidebar_group_rows(&self.book, &self.groups, &|id| self.record_is_open(id))
    }

    /// Migrates `focused_group_path` after a group rename/delete/reload so it always
    /// points at a visible group (or `None` when no group is visible) (R10).
    pub(super) fn migrate_group_focus_after_change(&mut self, previous_index: Option<usize>) {
        let drawn = self.drawn_group_paths();
        if let Some(current) = &self.focused_group_path {
            if let Some(idx) = drawn.iter().position(|path| path == current) {
                self.group_scroll
                    .scroll_to_item(idx, gpui::ScrollStrategy::Center);
                return;
            }
            let next = previous_index
                .and_then(|idx| drawn.get(idx))
                .or_else(|| drawn.last())
                .cloned();
            self.focused_group_path = next.clone();
            if let Some(target) = next
                && let Some(idx) = drawn.iter().position(|path| *path == target)
            {
                self.group_scroll
                    .scroll_to_item(idx, gpui::ScrollStrategy::Center);
            }
        }
    }

    /// Keeps `focused_group_path` aligned with the visible rows when group or host search changes.
    pub(super) fn sync_focused_group_after_query(&mut self) {
        let drawn = self.drawn_group_paths();
        if drawn.is_empty() {
            self.focused_group_path = None;
            return;
        }
        let still_visible = self
            .focused_group_path
            .as_ref()
            .is_some_and(|path| drawn.contains(path));
        if !still_visible {
            self.focused_group_path = drawn.first().cloned();
            self.group_scroll
                .scroll_to_item(0, gpui::ScrollStrategy::Top);
        }
    }

    /// Clears the independent sidebar group locator without touching host search/filters/sort,
    /// and migrates keyboard focus safely back into the visible group tree (R9, R10).
    pub(super) fn clear_group_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.group_search
            .update(cx, |field, cx| field.set_text(String::new(), cx));
        self.hosts.set_group_query(String::new());
        let drawn = self.drawn_group_paths();
        let target = self
            .focused_group_path
            .as_ref()
            .filter(|path| drawn.contains(path))
            .cloned()
            .or_else(|| match self.hosts.section() {
                HomeSection::Group(path) if drawn.contains(&path) => Some(path),
                _ => drawn.first().cloned(),
            });
        if let Some(path) = target {
            self.focus_group_row(&path, window, cx);
        } else {
            self.focused_group_path = None;
            window.focus(&self.sidebar_place_focus[0].clone());
            cx.notify();
        }
    }

    /// Activates the highlighted (or first) group from the sidebar group locator on Enter (R9).
    pub(super) fn activate_located_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let drawn = self.drawn_group_paths();
        let target = self
            .focused_group_path
            .as_ref()
            .filter(|path| drawn.contains(path))
            .cloned()
            .or_else(|| drawn.first().cloned());
        if let Some(path) = target {
            self.show_home_section(HomeSection::Group(path.clone()), cx);
            self.focus_group_row(&path, window, cx);
        }
    }

    /// The group that currently holds virtual keyboard focus, for compatibility with tests.
    #[cfg(test)]
    pub fn group_focus_paths_for_test(&self) -> Vec<GroupPath> {
        self.focused_group_path.iter().cloned().collect()
    }

    /// The sidebar group whose row holds the keys, if one is currently visible.
    pub(super) fn focused_group(&self, window: &Window) -> Option<GroupPath> {
        if !self.group_list_focus.is_focused(window) {
            return None;
        }
        let path = self.focused_group_path.as_ref()?;
        self.drawn_group_paths()
            .contains(path)
            .then(|| path.clone())
    }

    /// The groups the sidebar draws, top to bottom: the rows the keyboard walks.
    pub(super) fn drawn_group_paths(&self) -> Vec<GroupPath> {
        self.sidebar_group_rows()
            .into_iter()
            .map(|row| row.path)
            .collect()
    }

    /// Gives the keys to one sidebar group row and scrolls it into view (R10).
    pub(super) fn focus_group_row(
        &mut self,
        path: &GroupPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focused_group_path = Some(path.clone());
        window.focus(&self.group_list_focus);
        let drawn = self.drawn_group_paths();
        if let Some(idx) = drawn.iter().position(|candidate| candidate == path) {
            self.group_scroll
                .scroll_to_item(idx, gpui::ScrollStrategy::Center);
        }
        cx.notify();
    }

    /// Shift+Tab from the search or the list: the keys go to the sidebar, on the group
    /// shown or else the first one (when groups are drawn), or the active sidebar place.
    pub(super) fn focus_sidebar_groups(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let drawn = if self.home_sidebar_folded() {
            Vec::new()
        } else {
            self.drawn_group_paths()
        };
        let target = match self.hosts.section() {
            HomeSection::Group(shown) if drawn.contains(&shown) => Some(shown),
            _ => drawn.first().cloned(),
        };
        if let Some(target) = target {
            self.focus_group_row(&target, window, cx);
            return true;
        }
        let place_slot = match self.hosts.section() {
            HomeSection::All | HomeSection::Group(_) => 0,
            HomeSection::Favorites => 1,
            HomeSection::Recent => 2,
        };
        window.focus(&self.sidebar_place_focus[place_slot].clone());
        cx.notify();
        true
    }

    /// The keys of the sidebar's places and group rows while one holds the keyboard (ADR-3):
    /// ↑↓ walk All, Favorites, Recent, Groups and Local terminal; Left/Right collapse and
    /// expand a group branch; F2 renames a group; Delete asks to delete a group; Enter/Space
    /// activates the focused entry; Tab goes back to the search and Escape to the list.
    pub(super) fn home_group_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        #[derive(Clone, PartialEq, Eq)]
        enum SidebarStop {
            Place(usize, HomeSection),
            Group(GroupPath),
            Local,
        }

        let group_search_focused = self.group_search.read(cx).focus_handle().is_focused(window);
        if group_search_focused {
            if self.workspace.destination() != Destination::Hosts || self.hosts_keys_blocked() {
                return false;
            }
            let modifiers = &event.keystroke.modifiers;
            if modifiers.number_of_modifiers() != 0 {
                return false;
            }
            let key = event.keystroke.key.as_str();
            if matches!(key, "up" | "down") {
                let drawn = self.drawn_group_paths();
                if drawn.is_empty() {
                    return true;
                }
                let current_idx = self
                    .focused_group_path
                    .as_ref()
                    .and_then(|p| drawn.iter().position(|c| c == p));
                let next_idx = match (current_idx, key) {
                    (Some(idx), "down") => (idx + 1).min(drawn.len() - 1),
                    (Some(idx), "up") => idx.saturating_sub(1),
                    (None, "down") => 0,
                    (None, _) => drawn.len() - 1,
                    _ => 0,
                };
                self.focused_group_path = Some(drawn[next_idx].clone());
                self.group_scroll
                    .scroll_to_item(next_idx, gpui::ScrollStrategy::Center);
                cx.notify();
                return true;
            }
            return false;
        }

        let focused_place = self
            .sidebar_place_focus
            .iter()
            .position(|handle| handle.is_focused(window));
        let focused_group = self.focused_group(window);
        let group_list_focused = self.group_list_focus.is_focused(window);
        if focused_place.is_none() && focused_group.is_none() && group_list_focused {
            if self.workspace.destination() != Destination::Hosts || self.hosts_keys_blocked() {
                return false;
            }
            let modifiers = &event.keystroke.modifiers;
            if modifiers.number_of_modifiers() != 0 {
                return false;
            }
            let key = event.keystroke.key.as_str();
            let drawn = if self.home_sidebar_folded() {
                Vec::new()
            } else {
                self.drawn_group_paths()
            };
            match key {
                "tab" => {
                    self.focus_hosts_search(window, cx);
                    return true;
                }
                "escape" => {
                    let handle = self.destination_focus(Destination::Hosts).clone();
                    window.focus(&handle);
                    cx.notify();
                    return true;
                }
                "down" => {
                    if let Some(first) = drawn.first().cloned() {
                        self.focus_group_row(&first, window, cx);
                    } else {
                        window.focus(&self.sidebar_place_focus[3].clone());
                        cx.notify();
                    }
                    return true;
                }
                "up" => {
                    if let Some(last) = drawn.last().cloned() {
                        self.focus_group_row(&last, window, cx);
                    } else {
                        window.focus(&self.sidebar_place_focus[2].clone());
                        cx.notify();
                    }
                    return true;
                }
                "enter" | "space" => {
                    if let Some(first) = drawn.first().cloned() {
                        self.focus_group_row(&first, window, cx);
                        self.show_home_section(HomeSection::Group(first), cx);
                        return true;
                    }
                    return false;
                }
                _ => return false,
            }
        }
        let current = match (focused_place, focused_group) {
            (Some(0), _) => SidebarStop::Place(0, HomeSection::All),
            (Some(1), _) => SidebarStop::Place(1, HomeSection::Favorites),
            (Some(2), _) => SidebarStop::Place(2, HomeSection::Recent),
            (Some(3), _) => SidebarStop::Local,
            (_, Some(path)) => SidebarStop::Group(path),
            _ => return false,
        };
        if self.workspace.destination() != Destination::Hosts || self.hosts_keys_blocked() {
            return false;
        }
        let modifiers = &event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        if key == "tab" && modifiers.number_of_modifiers() == 0 {
            self.focus_hosts_search(window, cx);
            return true;
        }
        if modifiers.number_of_modifiers() != 0 {
            return false;
        }

        let mut stops = vec![
            SidebarStop::Place(0, HomeSection::All),
            SidebarStop::Place(1, HomeSection::Favorites),
            SidebarStop::Place(2, HomeSection::Recent),
        ];
        let drawn = if self.home_sidebar_folded() {
            Vec::new()
        } else {
            self.drawn_group_paths()
        };
        for path in &drawn {
            stops.push(SidebarStop::Group(path.clone()));
        }
        stops.push(SidebarStop::Local);

        match (&current, key) {
            (SidebarStop::Group(path), "f2") => {
                self.open_group_dialog(GroupAction::Rename(path.clone()), window, cx);
                true
            }
            (SidebarStop::Group(path), "delete" | "backspace") => {
                self.open_group_dialog(GroupAction::Delete(path.clone()), window, cx);
                true
            }
            (SidebarStop::Group(path), "left") => {
                let all_paths = crate::app::hosts::drawn_paths(&self.book, &self.groups);
                let has_children = all_paths
                    .iter()
                    .any(|candidate| candidate.parent().as_ref() == Some(path));
                if has_children && self.hosts.is_expanded(path) {
                    self.hosts.set_expanded(path, false);
                    cx.notify();
                } else if let Some(parent) = path.parent() {
                    self.focus_group_row(&parent, window, cx);
                }
                true
            }
            (SidebarStop::Group(path), "right") => {
                let all_paths = crate::app::hosts::drawn_paths(&self.book, &self.groups);
                let has_children = all_paths
                    .iter()
                    .any(|candidate| candidate.parent().as_ref() == Some(path));
                if has_children && !self.hosts.is_expanded(path) {
                    self.hosts.set_expanded(path, true);
                    cx.notify();
                }
                true
            }
            (SidebarStop::Group(path), "enter" | "space") => {
                self.show_home_section(HomeSection::Group(path.clone()), cx);
                true
            }
            (SidebarStop::Place(_, section), "enter" | "space") => {
                self.show_home_section(section.clone(), cx);
                true
            }
            (SidebarStop::Local, "enter" | "space") => {
                self.home_menu = None;
                self.toggle_home_local_menu(cx);
                true
            }
            (_, "up" | "down") => {
                if let Some(here) = stops.iter().position(|stop| *stop == current) {
                    let next = if key == "up" {
                        here.saturating_sub(1)
                    } else {
                        (here + 1).min(stops.len() - 1)
                    };
                    match &stops[next] {
                        SidebarStop::Place(slot, _) => {
                            window.focus(&self.sidebar_place_focus[*slot].clone());
                            cx.notify();
                        }
                        SidebarStop::Group(path) => {
                            self.focus_group_row(path, window, cx);
                        }
                        SidebarStop::Local => {
                            window.focus(&self.sidebar_place_focus[3].clone());
                            cx.notify();
                        }
                    }
                }
                true
            }
            (_, "escape") => {
                let handle = self.destination_focus(Destination::Hosts).clone();
                window.focus(&handle);
                cx.notify();
                true
            }
            _ => false,
        }
    }

    /// The right side of the page: the toolbar, what the page has to say, and the list.
    fn home_main(&self, list: &HostList, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let ux = copy::ux();
        let short = self.viewport_height < metrics.scale(SHORT_HEIGHT);

        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .when(!short, |body| body.min_h_0())
            .gap(metrics.scale(spacing::TIGHT))
            .px(metrics.scale(spacing::SNUG))
            .pt(metrics.scale(spacing::TIGHT))
            .pb(metrics.scale(spacing::SNUG))
            .children(
                self.local_error
                    .as_ref()
                    .map(|reason| self.notice_line(reason.clone(), Tone::Danger)),
            );
        // While the host dialog is open, its own refusals are shown inside it.
        if let Some(message) = self
            .hosts
            .message()
            .filter(|message| !(message.failed() && self.host_fields.is_some()))
        {
            body = body.child(
                components::banner(
                    if message.failed() {
                        Tone::Danger
                    } else {
                        Tone::Positive
                    },
                    metrics,
                )
                .debug_selector(|| "hosts.message".to_owned())
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .child(SharedString::from(message.text().to_owned())),
            );
        }
        if let Some(problem) = &self.group_problem {
            body = body.child(
                components::banner(Tone::Danger, metrics)
                    .debug_selector(|| "hosts.group.problem".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(problem.clone())),
            );
        }
        // A list narrowed or ordered by the filter menu says so, and offers one press to
        // drop it: a filter that hides hosts without naming itself is how a list looks
        // empty for no reason (R4, R11).
        let status = self.hosts.filter().status();
        let protocol = self.hosts.filter().protocol();
        let sort_applies = self.hosts.section() != HomeSection::Recent;
        let sort = self.hosts.sort();
        let sort_active = sort_applies && sort != HostSort::Saved;
        if status != HostStatusFilter::Any
            || protocol != HostProtocolFilter::All
            || sort_active
        {
            let mut chips = div()
                .flex()
                .flex_none()
                .items_center()
                .flex_wrap()
                .gap(metrics.scale(spacing::TIGHT))
                .debug_selector(|| "hosts.filters".to_owned());
            if protocol != HostProtocolFilter::All {
                chips = chips.child(
                    components::chip(format!("{} ×", protocol.label(ux)), Tone::Accent, metrics)
                        .id("hosts.filter.protocol")
                        .debug_selector(|| "hosts.filter.protocol".to_owned())
                        .cursor_pointer()
                        .tooltip(components::tooltip(ux.hosts_filter_clear))
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.hosts.set_protocol_filter(HostProtocolFilter::All);
                            this.hosts_cursor = None;
                            cx.notify();
                        })),
                );
            }
            if status != HostStatusFilter::Any {
                chips = chips.child(
                    components::chip(format!("{} ×", status.label(ux)), Tone::Accent, metrics)
                        .id("hosts.filter.status")
                        .debug_selector(|| "hosts.filter.status".to_owned())
                        .cursor_pointer()
                        .tooltip(components::tooltip(ux.hosts_filter_clear))
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.hosts.set_status_filter(HostStatusFilter::Any);
                            this.hosts_cursor = None;
                            cx.notify();
                        })),
                );
            }
            if sort_active {
                chips = chips.child(
                    components::chip(
                        format!("{} · {} ×", ux.home_sort_label, sort.label(ux)),
                        Tone::Neutral,
                        metrics,
                    )
                    .id("hosts.filter.sort")
                    .debug_selector(|| "hosts.filter.sort".to_owned())
                    .cursor_pointer()
                    .tooltip(components::tooltip(ux.hosts_filter_clear))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.hosts.set_sort(HostSort::Saved);
                        cx.notify();
                    })),
                );
            }
            body = body.child(
                chips.child(
                    components::compact_button(ux.hosts_filter_clear, Variant::Ghost, metrics)
                        .id("hosts.filter.clear")
                        .debug_selector(|| "hosts.filter.clear".to_owned())
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.hosts.set_status_filter(HostStatusFilter::Any);
                            this.hosts.set_protocol_filter(HostProtocolFilter::All);
                            this.hosts.set_sort(HostSort::Saved);
                            this.hosts_cursor = None;
                            cx.notify();
                        })),
                ),
            );
        }
        if !self.hosts.selected().is_empty() {
            body = body.child(self.hosts_selection_strip(list, cx));
        }
        body = body.child(self.hosts_body(list, cx));

        div()
            .id("home.scroll")
            .track_scroll(&self.home_scroll)
            .debug_selector(|| "home".to_owned())
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            // A short window scrolls the toolbar and the list as one column, so the list
            // keeps the height it needs instead of being squeezed under the toolbar.
            .when(short, |column| {
                column.min_h_0().overflow_y_scroll().overflow_x_hidden()
            })
            .when(!short, |column| column.min_h_0().overflow_hidden())
            .child(self.home_toolbar(list, window, cx))
            .child(body)
    }

    /// The toolbar of the list: the place and its count, the search, the filter menu,
    /// the "⋯" menu and the one primary action, a new host (ADR-3).
    fn home_toolbar(&self, list: &HostList, window: &Window, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let title = div()
            .flex()
            .flex_none()
            .items_baseline()
            .gap(metrics.scale(spacing::TIGHT))
            .max_w(metrics.scale(TITLE_MAX_WIDTH))
            .min_w_0()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(metrics.title_size())
                    .font_weight(metrics.title_weight())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "header.title".to_owned())
                    .child(components::line_text(self.home_title())),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "header.count".to_owned())
                    .child(SharedString::from(list.matched_len().to_string())),
            );

        let focused = self.hosts_search.read(cx).focus_handle().is_focused(window);
        let empty = self.hosts.search().is_empty();
        let search = div()
            .flex_1()
            .min_w(metrics.scale(SEARCH_MIN_WIDTH))
            .debug_selector(|| "hosts.search".to_owned())
            .child(components::search_field(
                self.hosts_search.clone().into_any_element(),
                None,
                (empty && !focused).then(|| SharedString::from("/")),
                if focused {
                    ControlState::Focused
                } else {
                    ControlState::Resting
                },
                metrics,
            ));

        let narrowing = self.hosts.filter().status() != HostStatusFilter::Any
            || self.hosts.filter().protocol() != HostProtocolFilter::All
            || (self.hosts.section() != HomeSection::Recent
                && self.hosts.sort() != HostSort::Saved);
        let filter_open = self.home_menu == Some(HomeMenu::Filter);
        let filter_anchor = self.home_filter_anchor.clone();
        let filter = div()
            .relative()
            .flex_none()
            .child(
                components::toggle_icon_button(Icon::Filter, narrowing || filter_open, metrics)
                    .id("hosts.filter")
                    .debug_selector(|| "hosts.filter".to_owned())
                    .when(!filter_open, |button| {
                        button.tooltip(components::tooltip(ux.home_filter))
                    })
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.toggle_home_menu(HomeMenu::Filter, cx);
                    })),
            )
            .child(
                gpui::canvas(
                    move |bounds, _window, _cx| filter_anchor.set(bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            );
        let more_open = self.home_menu == Some(HomeMenu::More);
        let more_anchor = self.home_more_anchor.clone();
        let more = div()
            .relative()
            .flex_none()
            .child(
                components::toggle_icon_button(Icon::More, more_open, metrics)
                    .id("hosts.more")
                    .debug_selector(|| "hosts.more".to_owned())
                    .when(!more_open, |button| {
                        button.tooltip(components::tooltip(ux.home_card_more))
                    })
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.toggle_home_menu(HomeMenu::More, cx);
                    })),
            )
            .child(
                gpui::canvas(
                    move |bounds, _window, _cx| more_anchor.set(bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            );

        components::toolbar(metrics)
            .debug_selector(|| "hosts.toolbar".to_owned())
            .flex_wrap()
            .child(title)
            .child(search)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(metrics.scale(spacing::HALF))
                    .child(filter)
                    .child(more)
                    .child({
                        let shortcut =
                            if crate::app::Platform::current() == crate::app::Platform::MacOs {
                                "⌘N"
                            } else {
                                "Ctrl+N"
                            };
                        let new_tip = format!("{} ({shortcut})", strings.primary_hosts);
                        components::icon_label_button(
                            Icon::Plus,
                            strings.primary_hosts,
                            Variant::Primary,
                            metrics,
                        )
                        .h(metrics.compact_control_height())
                        .id("hosts.new")
                        .debug_selector(|| "hosts.new".to_owned())
                        .tooltip(components::tooltip(SharedString::from(new_tip)))
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.home_menu = None;
                                this.open_create_host(cx);
                            },
                        ))
                    }),
            )
    }

    /// The menu open over the home page, if one is: the filter, the "⋯" or a group's.
    pub(super) fn home_menu_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let menu = self.home_menu.clone()?;
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let viewport = window.viewport_size();
        let below = |anchor: gpui::Bounds<Pixels>| {
            gpui::point(
                anchor.left(),
                anchor.bottom() + metrics.scale(spacing::HAIRLINE),
            )
        };
        let (card, position) = match menu {
            HomeMenu::Filter => {
                let mut card = components::menu_card("hosts.filter.menu", metrics)
                    .w(metrics.scale(MENU_WIDTH))
                    .child(components::menu_section(ux.hosts_protocol_label, metrics));
                for protocol in HostProtocolFilter::ALL {
                    let chosen = self.hosts.filter().protocol() == protocol;
                    card = card.child(
                        components::menu_entry(
                            protocol.label(ux),
                            protocol.debug_selector(),
                            None,
                            true,
                            false,
                            metrics,
                        )
                        .children(chosen.then(|| menu_check(metrics)))
                        .on_click(cx.listener(
                            move |this, _event, _window, cx| {
                                this.hosts.set_protocol_filter(protocol);
                                this.hosts_cursor = None;
                                this.close_home_menu(cx);
                            },
                        )),
                    );
                }
                card = card
                    .child(components::menu_divider(metrics))
                    .child(components::menu_section(ux.hosts_status_label, metrics));
                for status in HostStatusFilter::ALL {
                    let chosen = self.hosts.filter().status() == status;
                    card = card.child(
                        components::menu_entry(
                            status.label(ux),
                            status.debug_selector(),
                            None,
                            true,
                            false,
                            metrics,
                        )
                        .children(chosen.then(|| menu_check(metrics)))
                        .on_click(cx.listener(
                            move |this, _event, _window, cx| {
                                this.hosts.set_status_filter(status);
                                this.hosts_cursor = None;
                                this.close_home_menu(cx);
                            },
                        )),
                    );
                }
                if self.hosts.section() != HomeSection::Recent {
                    card = card
                        .child(components::menu_divider(metrics))
                        .child(components::menu_section(ux.home_sort_label, metrics));
                    for sort in HostSort::ALL {
                        let chosen = self.hosts.sort() == sort;
                        card = card.child(
                            components::menu_entry(
                                sort.label(ux),
                                sort.debug_selector(),
                                None,
                                true,
                                false,
                                metrics,
                            )
                            .children(chosen.then(|| menu_check(metrics)))
                            .on_click(cx.listener(
                                move |this, _event, _window, cx| {
                                    this.hosts.set_sort(sort);
                                    this.close_home_menu(cx);
                                },
                            )),
                        );
                    }
                }
                (card, below(self.home_filter_anchor.get()))
            }
            HomeMenu::More => {
                let mut card = components::menu_card("hosts.more.menu", metrics)
                    .w(metrics.scale(MENU_WIDTH))
                    .child(
                        components::menu_entry(
                            strings.import_open,
                            "hosts.import",
                            Some(Icon::Import),
                            true,
                            false,
                            metrics,
                        )
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.close_home_menu(cx);
                                this.open_import(cx);
                            },
                        )),
                    )
                    .child(
                        components::menu_entry(
                            ux.hosts_batch_open,
                            "hosts.batch.open.list",
                            Some(Icon::Layers),
                            true,
                            false,
                            metrics,
                        )
                        .tooltip(components::tooltip(ux.hosts_batch_open_hint))
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.close_home_menu(cx);
                                this.open_batch_preview(cx);
                            },
                        )),
                    );
                // While a group is the place on screen, the same rename and delete the
                // group's right-click menu offers are here too, so they have a visible
                // button and not only a right press.
                if let HomeSection::Group(path) = self.hosts.section() {
                    let rename = path.clone();
                    let delete = path;
                    card = card
                        .child(components::menu_divider(metrics))
                        .child(
                            components::menu_entry(
                                ux.hosts_group_rename,
                                "hosts.more.group.rename",
                                Some(Icon::Pencil),
                                true,
                                false,
                                metrics,
                            )
                            .on_click(cx.listener(
                                move |this, _event, window, cx| {
                                    this.close_home_menu(cx);
                                    this.open_group_dialog(
                                        GroupAction::Rename(rename.clone()),
                                        window,
                                        cx,
                                    );
                                },
                            )),
                        )
                        .child(
                            components::menu_entry(
                                ux.hosts_group_delete,
                                "hosts.more.group.delete",
                                Some(Icon::Trash),
                                true,
                                true,
                                metrics,
                            )
                            .on_click(cx.listener(
                                move |this, _event, window, cx| {
                                    this.close_home_menu(cx);
                                    this.open_group_dialog(
                                        GroupAction::Delete(delete.clone()),
                                        window,
                                        cx,
                                    );
                                },
                            )),
                        );
                }
                (card, below(self.home_more_anchor.get()))
            }
            HomeMenu::Group(path, position) => {
                let rename = path.clone();
                let delete = path;
                let card = components::menu_card("hosts.group.menu", metrics)
                    .child(
                        components::menu_entry(
                            ux.hosts_group_rename,
                            "hosts.group.rename.open",
                            Some(Icon::Pencil),
                            true,
                            false,
                            metrics,
                        )
                        .on_click(cx.listener(
                            move |this, _event, window, cx| {
                                this.close_home_menu(cx);
                                this.open_group_dialog(
                                    GroupAction::Rename(rename.clone()),
                                    window,
                                    cx,
                                );
                            },
                        )),
                    )
                    .child(
                        components::menu_entry(
                            ux.hosts_group_delete,
                            "hosts.group.delete.open",
                            Some(Icon::Trash),
                            true,
                            true,
                            metrics,
                        )
                        .on_click(cx.listener(
                            move |this, _event, window, cx| {
                                this.close_home_menu(cx);
                                this.open_group_dialog(
                                    GroupAction::Delete(delete.clone()),
                                    window,
                                    cx,
                                );
                            },
                        )),
                    );
                (card, position)
            }
        };
        Some(
            super::overlay_layer().child(
                div()
                    .relative()
                    .w(viewport.width)
                    .h(viewport.height)
                    .child(
                        div()
                            .id("home.menu.backdrop")
                            .absolute()
                            .inset_0()
                            .occlude()
                            .on_any_mouse_down(cx.listener(|this, _event, _window, cx| {
                                this.close_home_menu(cx);
                            })),
                    )
                    .child(
                        gpui::anchored()
                            .position(position)
                            .snap_to_window_with_margin(metrics.scale(spacing::TIGHT))
                            .child(
                                div()
                                    .occlude()
                                    .child(components::menu_enter(card, "home.menu.enter")),
                            ),
                    ),
            ),
        )
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
            .flex_none()
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
                    .font_weight(metrics.medium_weight())
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
            .child(
                div()
                    .flex_none()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "hosts.selection.scope_note".to_owned())
                    .child(ux.hosts_selection_ssh_only),
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
            .child(
                components::compact_button(ux.hosts_delete_selected, Variant::Danger, metrics)
                    .id("hosts.select.delete")
                    .debug_selector(|| "hosts.select.delete".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.ask_batch_delete_hosts(cx);
                    })),
            )
    }

    /// The list, or what an empty one says.
    fn hosts_body(&self, list: &HostList, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let rows = self.host_rows_snapshot(list);
        let short = self.viewport_height < metrics.scale(SHORT_HEIGHT);
        let body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .when(!short, |body| body.min_h_0())
            // The number of records listed is part of the selector, so a test can tell
            // "the search narrowed the list" from "the list did not change" without
            // reading text, and a person reading the manifest sees the same count.
            .debug_selector({
                let shown = list.matched_len();
                move || format!("hosts.list.{shown}")
            });
        if rows.is_empty() {
            body.child(self.hosts_empty_list(cx))
        } else {
            body.child(self.hosts_virtual_list(rows, cx))
        }
    }

    /// What a list with no row says: the reason, and what would change it (R1, R3).
    ///
    /// A book with no record at all is the welcome: a new host, an import, the local
    /// terminal or a backup. A search that finds nothing offers to connect to what was
    /// typed when it reads as an address, and to clear the search otherwise. An empty
    /// place says how something gets there. None is a sentence with no next step.
    fn hosts_empty_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let note = |selector: &'static str,
                    icon: Icon,
                    title: SharedString,
                    hint: Option<SharedString>,
                    action: AnyElement| {
            div()
                .w_full()
                .debug_selector(move || selector.to_owned())
                .child(components::empty_state(
                    icon,
                    title,
                    hint,
                    Some(action),
                    metrics,
                ))
                .into_any_element()
        };
        let searching = !self.hosts.search().trim().is_empty();
        let book_empty = self.book.hosts().is_empty() && self.book.connections().is_empty();
        if book_empty && !searching {
            return self.home_welcome(cx);
        }
        if searching
            || self.hosts.filter().status() != HostStatusFilter::Any
            || self.hosts.filter().protocol() != HostProtocolFilter::All
        {
            let clear = components::button_sized(
                ux.hosts_filter_clear,
                Variant::Secondary,
                components::ButtonSize::Compact,
                metrics,
            )
            .id("hosts.nomatch.clear")
            .debug_selector(|| "hosts.nomatch.clear".to_owned())
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.clear_host_narrowing(cx);
            }));
            let action = match self.home_direct_target() {
                Some(target) => div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        components::icon_label_button(
                            Icon::ArrowRight,
                            copy::fill(ux.home_quick_temporary, &[("target", &target.label())]),
                            Variant::Primary,
                            metrics,
                        )
                        .id("hosts.nomatch.connect")
                        .debug_selector(|| "hosts.nomatch.connect".to_owned())
                        .tooltip(components::tooltip(ux.home_quick_temporary_hint))
                        .on_click(cx.listener(
                            |this, _event, window, cx| {
                                this.connect_home_direct(false, window, cx);
                            },
                        )),
                    )
                    .child(clear)
                    .into_any_element(),
                None => clear.into_any_element(),
            };
            return note(
                "hosts.nomatch",
                Icon::Search,
                SharedString::from(strings.hosts_no_match),
                Some(SharedString::from(ux.hosts_no_match_hint)),
                action,
            );
        }
        let all = components::button_sized(
            strings.hosts_view_all,
            Variant::Secondary,
            components::ButtonSize::Compact,
            metrics,
        );
        match self.hosts.section() {
            HomeSection::Recent => note(
                "hosts.recent.empty",
                Icon::Clock,
                SharedString::from(strings.hosts_recent_empty),
                None,
                all.id("hosts.recent.all")
                    .debug_selector(|| "hosts.recent.all".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.show_home_section(HomeSection::All, cx);
                    }))
                    .into_any_element(),
            ),
            HomeSection::Favorites => note(
                "hosts.favorites.empty",
                Icon::Star,
                SharedString::from(ux.home_favorites_empty),
                Some(SharedString::from(ux.home_favorites_empty_hint)),
                all.id("hosts.favorites.all")
                    .debug_selector(|| "hosts.favorites.all".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.show_home_section(HomeSection::All, cx);
                    }))
                    .into_any_element(),
            ),
            HomeSection::Group(_) => note(
                "hosts.group.empty",
                Icon::Folder,
                SharedString::from(ux.hosts_group_empty_body),
                None,
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        components::icon_label_button(
                            Icon::Plus,
                            strings.primary_hosts,
                            Variant::Primary,
                            metrics,
                        )
                        .h(metrics.compact_control_height())
                        .id("hosts.group.new_host")
                        .debug_selector(|| "hosts.group.new_host".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.open_create_host(cx);
                            },
                        )),
                    )
                    .child(
                        all.id("hosts.group.all")
                            .debug_selector(|| "hosts.group.all".to_owned())
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.show_home_section(HomeSection::All, cx);
                            })),
                    )
                    .into_any_element(),
            ),
            HomeSection::All => note(
                "hosts.group.empty",
                Icon::Folder,
                SharedString::from(strings.hosts_no_match),
                None,
                all.id("hosts.group.all")
                    .debug_selector(|| "hosts.group.all".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.show_home_section(HomeSection::All, cx);
                    }))
                    .into_any_element(),
            ),
        }
    }

    /// The welcome of a book with no record: what a host is, and the ways to get one or
    /// to start without one - a new host, an import, the local terminal, a backup.
    fn home_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let actions = div()
            .flex()
            .flex_wrap()
            .justify_center()
            .gap(metrics.scale(spacing::TIGHT))
            .child(
                components::icon_label_button(
                    Icon::Plus,
                    strings.primary_hosts,
                    Variant::Primary,
                    metrics,
                )
                .id("hosts.empty.new")
                .debug_selector(|| "hosts.empty.new".to_owned())
                .on_click(cx.listener(|this, _event, _window, cx| {
                    this.open_create_host(cx);
                })),
            )
            .child(
                components::icon_label_button(
                    Icon::Import,
                    ux.home_import,
                    Variant::Secondary,
                    metrics,
                )
                .id("hosts.import.open")
                .debug_selector(|| "hosts.import.open".to_owned())
                .on_click(cx.listener(|this, _event, _window, cx| {
                    this.open_import(cx);
                })),
            )
            .child(
                components::icon_label_button(
                    Icon::Terminal,
                    ux.home_local_button,
                    Variant::Secondary,
                    metrics,
                )
                .id("hosts.empty.local")
                .debug_selector(|| "hosts.empty.local".to_owned())
                .on_click(cx.listener(|this, _event, window, cx| {
                    let choice = this.workspace.shell_choice();
                    this.launch_choice(choice, window, cx);
                })),
            )
            // A new machine with a backup elsewhere starts here: the file chooser, then
            // the same preview the settings page shows (R5, R6).
            .child(
                components::icon_label_button(
                    Icon::FolderOpen,
                    copy::backup_for(self.workspace.locale()).home_restore,
                    Variant::Ghost,
                    metrics,
                )
                .id("hosts.empty.restore")
                .debug_selector(|| "hosts.empty.restore".to_owned())
                .on_click(cx.listener(|this, _event, _window, cx| {
                    this.choose_backup_import(cx);
                })),
            );
        let welcome = div()
            .w_full()
            .debug_selector(|| "hosts.empty".to_owned())
            .child(components::empty_state(
                Icon::Server,
                ux.home_welcome_title,
                Some(SharedString::from(ux.home_welcome_body)),
                Some(actions.into_any_element()),
                metrics,
            ));
        theme::enter(welcome, "hosts.empty.enter", theme::Enter::Rise)
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
        let prefix = if self.hosts.view() == HostView::Recent {
            "hosts.recent"
        } else {
            "hosts.row"
        };
        let min_list_height = if self.viewport_height < metrics.scale(SHORT_HEIGHT) {
            row_height * 2.0
        } else {
            px(0.0)
        };
        uniform_list("hosts.rows", rows.len(), move |range, _window, _cx| {
            drawn.set(drawn.get().max(range.len()));
            let mut items: Vec<AnyElement> = Vec::with_capacity(range.len());
            for row in &rows[range] {
                let element = match row {
                    SnapshotRow::Direct(direct) => {
                        direct_row_element(direct, &weak, row_height, metrics, ux)
                    }
                    SnapshotRow::Host(host) => {
                        host_row_element(host, &weak, row_height, prefix, metrics, strings, ux)
                    }
                    SnapshotRow::Connection(connection) => {
                        connection_row_element(connection, &weak, row_height, metrics, strings, ux)
                    }
                };
                items.push(element.into_any_element());
            }
            items
        })
        .track_scroll(self.hosts_scroll.clone())
        .flex_1()
        .min_h(min_list_height)
        .min_w_0()
        .debug_selector(|| "hosts.rows".to_owned())
        .into_any_element()
    }

    /// Reads every row of the list once, for the frame about to be drawn.
    fn host_rows_snapshot(&self, list: &HostList) -> Rc<Vec<SnapshotRow>> {
        let metrics = self.metrics();
        let ux = copy::ux();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let cursor = self.active_hosts_cursor();
        let show_duplicate = self.viewport_width >= metrics.scale(ROW_DUPLICATE_MIN_WIDTH);
        let show_meta = self.viewport_width >= metrics.scale(ROW_META_MIN_WIDTH);
        let mut rows = Vec::with_capacity(list.len() + 1);
        if let Some(target) = self.home_direct_row(list) {
            rows.push(SnapshotRow::Direct(DirectRowView {
                title: SharedString::from(copy::fill(
                    ux.home_quick_temporary,
                    &[("target", &target.label())],
                )),
                hint: SharedString::from(ux.home_quick_temporary_hint),
                enter: cursor.is_none(),
            }));
        }
        let mut hosts = 0usize;
        let mut connections = 0usize;
        for row in list.rows() {
            match row {
                HostListRow::Group(_) => {}
                HostListRow::Host(host) => {
                    let Some(saved) = self.book.host(host.id) else {
                        continue;
                    };
                    let recent = self.book.recent_of(host.id);
                    rows.push(SnapshotRow::Host(HostRowView {
                        id: host.id,
                        number: hosts,
                        name: SharedString::from(saved.name().to_owned()),
                        target: SharedString::from(saved.target()),
                        group: show_meta
                            .then(|| saved.group_text().map(SharedString::from))
                            .flatten(),
                        open: self.host_is_open(host.id),
                        last: recent.as_ref().filter(|_| show_meta).map(|recent| {
                            SharedString::from(copy::relative_time(
                                recent.last_connected_unix(),
                                now,
                                copy::Locale::current(),
                            ))
                        }),
                        last_exact: recent.as_ref().map(|recent| {
                            SharedString::from(format!(
                                "{} {} · {} {}",
                                self.strings().host_last_connected,
                                format_unix(recent.last_connected_unix()),
                                recent.connections(),
                                self.strings().host_connection_count
                            ))
                        }),
                        selected: self.hosts.selected().contains(host.id),
                        cursor: cursor == Some(host.id),
                        show_duplicate,
                        show_meta,
                        selecting: !self.hosts.selected().is_empty(),
                        favorite: self.book.is_favorite(host.id),
                        key_missing: saved
                            .identity_file()
                            .filter(|path| self.key_presence.missing(path))
                            .map(|path| {
                                let backup = copy::backup_for(self.workspace.locale());
                                (
                                    SharedString::from(backup.host_key_missing),
                                    SharedString::from(copy::fill(
                                        backup.host_key_missing_tip,
                                        &[("path", path)],
                                    )),
                                )
                            }),
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
                        group: show_meta
                            .then(|| saved.group_text().map(SharedString::from))
                            .flatten(),
                        open: self.connection_is_open(row.id),
                        cursor: cursor == Some(row.id),
                        favorite: self.book.is_favorite(row.id),
                        show_duplicate,
                        show_meta,
                        selecting: !self.hosts.selected().is_empty(),
                    }));
                    connections += 1;
                }
            }
        }
        Rc::new(rows)
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
                        .min_w_0()
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
                            .min_w_0()
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
        // The shared dialog foot: a hairline over it, the actions on the right and the
        // answer last. Deleting a group cannot be taken back, so that answer is the
        // danger button rather than the primary one (R5, R6).
        let confirm_variant = if matches!(action, GroupAction::Delete(_)) {
            Variant::Danger
        } else {
            Variant::Primary
        };
        let footer = components::modal_footer(metrics)
            .child(
                components::compact_button(strings.host_cancel, Variant::Ghost, metrics)
                    .h(metrics.control_height())
                    .id("hosts.group.cancel")
                    .debug_selector(|| "hosts.group.cancel".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.close_group_dialog(cx);
                    })),
            )
            .child(
                components::compact_button(confirm, confirm_variant, metrics)
                    .h(metrics.control_height())
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

/// One virtualized group row inside `hosts.groups.list` (R9, R10, ADR-6).
///
/// Carries no per-row `FocusHandle`: focus lives on the single container `group_list_focus`
/// and is drawn via `is_focused`, keeping focus handles `O(1)` at any tree size.
fn group_tree_row_element(
    group: &SidebarGroupRow,
    number: usize,
    section: &HomeSection,
    is_focused: bool,
    row_height: Pixels,
    weak: &WeakEntity<ShelluneView>,
    metrics: Metrics,
) -> Stateful<Div> {
    let path = group.path.clone();
    let chosen = matches!(section, HomeSection::Group(shown) if *shown == path);
    let label = if group.show_full_path {
        path.to_canonical()
    } else {
        path.segments().last().cloned().unwrap_or_default()
    };
    let chevron = if group.expanded {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    let toggle_path = path.clone();
    let pick_path = path.clone();
    let more_path = path.clone();
    let menu_path = path;
    let toggle_weak = weak.clone();
    let pick_weak = weak.clone();
    let more_weak = weak.clone();
    let menu_weak = weak.clone();
    let ring = theme::swatch(palette::FOCUS_RING);
    let more = components::icon_only_button(Icon::More, metrics)
        .size(metrics.scale(CHEVRON_SIZE))
        .invisible()
        .group_hover(GROUP_ROW, |style| style.visible())
        .id(SharedString::from(format!("hosts.tree.{number}.more")))
        .debug_selector(move || format!("hosts.tree.{number}.more"))
        .tooltip(components::tooltip(copy::ux().home_card_more))
        .on_click(move |event: &gpui::ClickEvent, _window, cx| {
            cx.stop_propagation();
            let _ = more_weak.update(cx, |this, cx| {
                this.shell_menu_open = false;
                this.host_menu = None;
                this.home_menu = Some(HomeMenu::Group(more_path.clone(), event.position()));
                cx.notify();
            });
        });
    let toggle = if group.has_children {
        components::icon_only_button(chevron, metrics)
            .size(metrics.scale(CHEVRON_SIZE))
            .id(SharedString::from(format!("hosts.tree.{number}.toggle")))
            .debug_selector(move || format!("hosts.tree.{number}.toggle"))
            .on_click(move |_event, _window, cx| {
                cx.stop_propagation();
                let _ = toggle_weak.update(cx, |this, cx| {
                    this.toggle_group_row(Some(toggle_path.clone()), cx);
                });
            })
            .into_any_element()
    } else {
        div()
            .flex_none()
            .size(metrics.scale(CHEVRON_SIZE))
            .into_any_element()
    };
    div()
        .id(SharedString::from(format!("hosts.tree.{number}")))
        .debug_selector(move || format!("hosts.tree.{number}"))
        .group(GROUP_ROW)
        .flex()
        .flex_none()
        .items_center()
        .gap(metrics.scale(spacing::HALF))
        .h(row_height)
        .w_full()
        .min_w_0()
        .pl(metrics.scale(GROUP_INDENT * group.depth as f32))
        .border_1()
        .border_color(if is_focused { ring } else { theme::clear() })
        .rounded(px(radius::MEDIUM))
        .child(toggle)
        .child(
            components::sidebar_item(Icon::Folder, label, Some(group.hosts), chosen, metrics)
                .flex_1(),
        )
        .child(more)
        .on_click(move |_event, window, cx| {
            let _ = pick_weak.update(cx, |this, cx| {
                this.focus_group_row(&pick_path, window, cx);
                this.show_home_section(HomeSection::Group(pick_path.clone()), cx);
            });
        })
        .on_mouse_down(
            gpui::MouseButton::Right,
            move |event: &gpui::MouseDownEvent, _window, cx| {
                cx.stop_propagation();
                let _ = menu_weak.update(cx, |this, cx| {
                    this.shell_menu_open = false;
                    this.host_menu = None;
                    this.home_menu = Some(HomeMenu::Group(menu_path.clone(), event.position));
                    cx.notify();
                });
            },
        )
}

/// The check that marks the current choice of a menu.
fn menu_check(metrics: Metrics) -> gpui::Svg {
    icons::icon(
        Icon::Check,
        metrics.small_icon_size(),
        theme::swatch(palette::ACCENT),
    )
}

/// The "connect to …" row on top of a search that reads as an address: one press, or
/// Enter in the search, dials it without saving it (ADR-3).
fn direct_row_element(
    direct: &DirectRowView,
    weak: &WeakEntity<ShelluneView>,
    row_height: Pixels,
    metrics: Metrics,
    ux: &'static UxStrings,
) -> Stateful<Div> {
    let connect = weak.clone();
    let state = if direct.enter {
        ControlState::Selected
    } else {
        ControlState::Resting
    };
    components::list_row_frame(state, metrics)
        .h(row_height)
        .id("hosts.direct")
        .debug_selector(|| "hosts.direct".to_owned())
        .cursor_pointer()
        .on_click(move |_event, window, cx| {
            let _ = connect.update(cx, |view, cx| view.connect_home_direct(false, window, cx));
        })
        .child(components::leading_icon(
            Icon::ArrowRight,
            Tone::Accent,
            metrics,
        ))
        .child(row_text(
            direct.title.clone(),
            direct.hint.clone(),
            false,
            None,
            metrics,
        ))
        .when(direct.enter, |row| {
            row.child(components::kbd(ux.home_quick_enter, metrics))
        })
}

/// The two lines of a row: the name with what is said beside it, and the address, each
/// ending in an ellipsis where the row is too narrow.
fn row_text(
    title: SharedString,
    subtitle: SharedString,
    monospace: bool,
    beside_title: Option<AnyElement>,
    metrics: Metrics,
) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w(metrics.scale(ROW_TEXT_MIN_WIDTH))
        .gap(metrics.scale(spacing::HALF))
        .child(
            div()
                .flex()
                .items_center()
                .gap(metrics.scale(spacing::NARROW))
                .min_w_0()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(metrics.body_size())
                        .font_weight(metrics.medium_weight())
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .child(components::line_text(title)),
                )
                .children(beside_title),
        )
        .child(
            div().flex().min_w_0().child(
                div()
                    .min_w_0()
                    .truncate()
                    .when(monospace, |line| {
                        line.font_family(crate::app::terminal::monospace_family())
                    })
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(components::line_text(subtitle)),
            ),
        )
}

/// The quiet facts at a row's right end: the group tag, when it was last reached and
/// the state dot.
///
/// They stand in columns of their own: the tag column is left-aligned and the time
/// column right-aligned, both of a fixed width, and a row without a tag or a time leaves
/// its column empty rather than letting the rest slide left (ADR-3). In a window too
/// narrow for them both columns are left out of every row alike.
///
/// The dot is drawn only while this window holds a running session to the record: a
/// record that was only saved has no state to show, and an "offline" it never had would
/// be a guess (R2). Its place is kept either way, so the columns line up.
#[allow(clippy::too_many_arguments)]
fn row_meta(
    number: usize,
    prefix: &'static str,
    show_meta: bool,
    group: Option<SharedString>,
    last: Option<(SharedString, SharedString)>,
    open: bool,
    metrics: Metrics,
    ux: &'static UxStrings,
) -> Div {
    let dot = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(metrics.small_icon_size());
    let tag_column = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_start()
        .w(metrics.scale(ROW_TAG_WIDTH))
        .min_w_0()
        .children(group.map(|group| {
            components::tag(group, metrics)
                .max_w_full()
                .debug_selector(move || format!("{prefix}.tag.{number}"))
        }));
    let time_column = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_end()
        .w(metrics.scale(ROW_TIME_WIDTH))
        .min_w_0()
        .children(last.map(|(last, exact)| {
            div()
                .id(SharedString::from(format!("{prefix}.last.{number}")))
                .debug_selector(move || format!("{prefix}.last.{number}"))
                .min_w_0()
                .truncate()
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .tooltip(components::tooltip(exact))
                .child(components::line_text(last))
        }));
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(metrics.scale(spacing::TIGHT))
        .when(show_meta, |meta| meta.child(tag_column).child(time_column))
        .child(if open {
            dot.id(SharedString::from(format!("{prefix}.state.{number}")))
                .debug_selector(move || format!("{prefix}.state.{number}"))
                .tooltip(components::tooltip(ux.hosts_status_connected))
                .child(components::status_dot(Tone::Positive, metrics))
                .into_any_element()
        } else {
            dot.into_any_element()
        })
}

/// The commands a row reveals under the pointer and on the keyboard's row.
fn row_tools() -> Div {
    div().flex().flex_none().items_center().gap(px(0.0))
}

/// The place the tools stand in: it gives way before the name does, so a narrow or
/// zoomed window clips the quiet icons from the left and keeps the name and the
/// Connect button at the end; everything stays on the record's right-press menu.
fn row_tools_slot(tools: Div, cursor: bool) -> Div {
    div()
        .flex()
        .flex_shrink()
        .justify_end()
        .min_w_0()
        .overflow_hidden()
        .when(!cursor, |slot| {
            slot.opacity(0.0)
                .group_hover(components::ROW_GROUP, |style| style.opacity(1.0))
        })
        .child(tools)
}

/// One host inside the list: who it is, what it dials, its group, when it was reached,
/// its state, and what can be done with it.
fn host_row_element(
    host: &HostRowView,
    weak: &WeakEntity<ShelluneView>,
    row_height: Pixels,
    prefix: &'static str,
    metrics: Metrics,
    strings: &'static Strings,
    ux: &'static UxStrings,
) -> Stateful<Div> {
    let number = host.number;
    let id = host.id;
    let selected = host.selected;
    let cursor = host.cursor;
    let pick = weak.clone();
    let connect = weak.clone();
    let edit = weak.clone();
    let duplicate = weak.clone();
    let delete = weak.clone();
    let open_row = weak.clone();
    let menu_row = weak.clone();
    let state = if cursor {
        ControlState::Selected
    } else {
        ControlState::Resting
    };
    // The checkbox shares the avatar's place, as in a mail client: the avatar shows at
    // rest, and the box takes its place under the pointer, on the keyboard's row, or
    // once a selection has started - so picking hosts costs the list no column of empty
    // boxes.
    let picking = selected || cursor || host.selecting;
    let avatar = super::host_avatar(host.name.as_ref(), selected, metrics);
    let leading = div()
        .relative()
        .flex_none()
        .child(if picking {
            avatar.opacity(0.0)
        } else {
            avatar.group_hover(components::ROW_GROUP, |style| style.opacity(0.0))
        })
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    components::checkbox_target(selected, metrics)
                        .when(!picking, |boxed| {
                            boxed
                                .opacity(0.0)
                                .group_hover(components::ROW_GROUP, |style| style.opacity(1.0))
                        })
                        .id(SharedString::from(format!("hosts.select.{number}")))
                        .debug_selector(move || format!("hosts.select.{number}"))
                        .tooltip(components::tooltip(ux.hosts_select_start))
                        .on_click(move |_event, _window, cx| {
                            cx.stop_propagation();
                            let _ = pick.update(cx, |view, cx| view.toggle_host_selection(id, cx));
                        }),
                ),
        );
    let beside = {
        let mut badges = div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::HALF))
            .min_w_0()
            .child(
                components::tag(ProtocolKind::Ssh.label(), metrics)
                    .debug_selector(move || format!("hosts.protocol.badge.{number}")),
            );
        if let Some((label, tip)) = host.key_missing.clone() {
            badges = badges.child(
                div()
                    .id(SharedString::from(format!("hosts.key-missing.{number}")))
                    .debug_selector(move || format!("hosts.key-missing.{number}"))
                    .min_w_0()
                    .flex_shrink()
                    .tooltip(components::tooltip(tip))
                    .child(components::chip_within(label, Tone::Warning, metrics)),
            );
        }
        Some(badges.into_any_element())
    };
    let last = host
        .last
        .clone()
        .map(|last| (last, host.last_exact.clone().unwrap_or_default()));

    // The tools of a host are quiet icons and the one thing a row is for - connecting -
    // is the labelled button at its end. Every press stops at its button, so a quick
    // double press on an icon never also opens the host underneath it.
    let tools = row_tools()
        .child(
            components::icon_only_button(Icon::Pencil, metrics)
                .id(SharedString::from(format!("hosts.edit.{number}")))
                .debug_selector(move || format!("hosts.edit.{number}"))
                .tooltip(components::tooltip(strings.host_edit))
                .on_click(move |_event, _window, cx| {
                    cx.stop_propagation();
                    let _ = edit.update(cx, |view, cx| view.edit_saved_host(id, cx));
                }),
        )
        .when(host.show_duplicate, |tools| {
            tools.child(
                components::icon_only_button(Icon::Copy, metrics)
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
                .id(SharedString::from(format!("hosts.delete.{number}")))
                .debug_selector(move || format!("hosts.delete.{number}"))
                .tooltip(components::tooltip(strings.host_delete))
                .on_click(move |_event, _window, cx| {
                    cx.stop_propagation();
                    let _ = delete.update(cx, |view, cx| view.ask_delete_host(id, cx));
                }),
        )
        .child(
            div().pl(metrics.scale(spacing::HALF)).child(
                components::compact_button(strings.remote_connect, Variant::Secondary, metrics)
                    .id(SharedString::from(format!("hosts.connect.{number}")))
                    .debug_selector(move || format!("hosts.connect.{number}"))
                    .tooltip(components::tooltip(ux.hosts_connect_hint))
                    .on_click(move |_event, _window, cx| {
                        cx.stop_propagation();
                        let _ = connect.update(cx, |view, cx| view.connect_saved_host(id, cx));
                    }),
            ),
        );

    components::list_row_frame(state, metrics)
        .h(row_height)
        .id(SharedString::from(format!("hosts.row.id.{number}")))
        .debug_selector(move || format!("{prefix}.{number}"))
        // A picked row is a soft tint: picked rows sit next to each other, and an
        // outline on each of them would stack into a grid of borders.
        .when(selected, |row| row.bg(theme::swatch(palette::ACCENT_TINT)))
        // A single press points at the row; a double press opens it; a right press
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
        .child(leading)
        .child(row_text(
            host.name.clone(),
            host.target.clone(),
            true,
            beside,
            metrics,
        ))
        .child(row_meta(
            number,
            "hosts",
            host.show_meta,
            host.group.clone(),
            last,
            host.open,
            metrics,
            ux,
        ))
        .child(favorite_star(
            host.favorite,
            cursor,
            format!("hosts.favorite.{number}"),
            id,
            weak,
            metrics,
        ))
        .child(row_tools_slot(tools, cursor))
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
    row_height: Pixels,
    metrics: Metrics,
    strings: &'static Strings,
    ux: &'static UxStrings,
) -> Stateful<Div> {
    let number = connection.number;
    let id = connection.id;
    let cursor = connection.cursor;
    let edit = weak.clone();
    let connect = weak.clone();
    let open_row = weak.clone();
    let menu_row = weak.clone();
    let copy = weak.clone();
    let delete = weak.clone();
    let state = if cursor {
        ControlState::Selected
    } else {
        ControlState::Resting
    };
    let protocol = components::tag(connection.protocol.clone(), metrics).into_any_element();
    let tools = row_tools()
        .child(
            components::icon_only_button(Icon::Pencil, metrics)
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
        // Like a host row's duplicate, the copy gives way in a narrow window (it stays on
        // the right-press menu), so both kinds of row keep the same tools and columns.
        .when(connection.show_duplicate, |tools| {
            tools.child(
                components::icon_only_button(Icon::Copy, metrics)
                    .id(SharedString::from(format!(
                        "hosts.connection.copy.{number}"
                    )))
                    .debug_selector(move || format!("hosts.connection.copy.{number}"))
                    .tooltip(components::tooltip(strings.connection_duplicate))
                    .on_click(move |_event, _window, cx| {
                        cx.stop_propagation();
                        let _ = copy.update(cx, |view, cx| view.duplicate_saved_connection(id, cx));
                    }),
            )
        })
        .child(
            components::danger_icon_button(Icon::Trash, metrics)
                .id(SharedString::from(format!(
                    "hosts.connection.delete.{number}"
                )))
                .debug_selector(move || format!("hosts.connection.delete.{number}"))
                .tooltip(components::tooltip(strings.connection_delete))
                .on_click(move |_event, _window, cx| {
                    cx.stop_propagation();
                    let _ = delete.update(cx, |view, cx| view.ask_delete_host(id, cx));
                }),
        )
        .child(
            div().pl(metrics.scale(spacing::HALF)).child(
                components::compact_button(strings.remote_connect, Variant::Secondary, metrics)
                    .id(SharedString::from(format!(
                        "hosts.connection.connect.{number}"
                    )))
                    .debug_selector(move || format!("hosts.connection.connect.{number}"))
                    .tooltip(components::tooltip(ux.hosts_connect_hint))
                    .on_click(move |_event, _window, cx| {
                        cx.stop_propagation();
                        let _ =
                            connect.update(cx, |view, cx| view.connect_saved_connection(id, cx));
                    }),
            ),
        );
    components::list_row_frame(state, metrics)
        .h(row_height)
        .id(SharedString::from(format!("hosts.connection.id.{number}")))
        .debug_selector(move || format!("hosts.connection.{number}"))
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
        .child(
            div()
                .id(SharedString::from(format!(
                    "hosts.connection.leading.{number}"
                )))
                .flex_none()
                .when(connection.selecting, |slot| {
                    slot.opacity(0.45)
                        .tooltip(components::tooltip(ux.hosts_selection_ssh_only))
                })
                .child(super::host_avatar(connection.name.as_ref(), false, metrics)),
        )
        .child(row_text(
            connection.name.clone(),
            connection.target.clone(),
            true,
            Some(protocol),
            metrics,
        ))
        // A running session of this record says so, exactly as a host row does (R2).
        .child(row_meta(
            number,
            "hosts.connection",
            connection.show_meta,
            connection.group.clone(),
            None,
            connection.open,
            metrics,
            ux,
        ))
        .child(favorite_star(
            connection.favorite,
            cursor,
            format!("hosts.connection.favorite.{number}"),
            id,
            weak,
            metrics,
        ))
        .child(row_tools_slot(tools, cursor))
}

/// The edge of a row's favourite star target, before zoom.
const STAR_TARGET_SIZE: f32 = 26.0;

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
        .size(metrics.scale(STAR_TARGET_SIZE))
        .rounded(px(radius::SMALL))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .when(!favorite && !cursor, |star| {
            star.opacity(0.0)
                .group_hover(components::ROW_GROUP, |style| style.opacity(1.0))
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
