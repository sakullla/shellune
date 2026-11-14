//! The window's frame: one top bar that is the title bar and the tab strip, the
//! hosts home (local shells and saved servers in one place), the shortcut capture layer,
//! and the dialogs that replaced inline forms and side pages: connecting to a host,
//! quick connect, port forwarding and transfers. The settings page itself is
//! ["settings"](super::settings), one category and one item at a time.
//!
//! The sidebar is gone. Everything a user switches between is a tab in the top bar:
//! the hosts home first, then one tab per open terminal. Settings sit at the right
//! end with search, next to the window buttons.

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Div, FontWeight, KeyDownEvent, MouseButton, SharedString, Stateful,
    Window, div, px,
};

use shellune_core::{SessionKind, SessionState, SnippetId};

use super::{
    ELLIPSIS, ShelluneView, SnippetNote, brand_mark, files_focus_ring, host_avatar, may_reconnect,
    program_label, session_tone,
};
use crate::app::components::{self, Variant};
use crate::app::copy;
use crate::app::files::FileSource;
use crate::app::icons::{self, Icon};
use crate::app::nav::{Destination, Platform};
use crate::app::snippets::{SnippetEditor, body_preview};
use crate::app::terminal;
use crate::app::terminal::SnippetOffer;
use crate::app::text_field::TextField;
use crate::app::theme::{self, ThemeChoice, Tone, palette, radius, spacing};
use crate::app::workspace::{AttentionKind, SectionState};

/// Width at which the head's second row still holds its target and its actions on
/// one line.
///
/// Below it the actions take a line of their own inside the same band, which is the
/// shortest that row can be there and leaves the terminal its room (R4).
const CONTEXT_ONE_LINE_WIDTH: f32 = 860.0;

/// A tool of a host or session that opens over the page instead of being a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Saved tunnels, for every host or one.
    Forwards,
    /// Uploads and downloads of every connection.
    Transfers,
    /// The saved shortcut commands, with the editor for one of them (R6).
    Snippets,
}

/// The pieces of one centred dialog, kept together so [`ShelluneView::modal`] stays
/// within the argument limit.
///
/// The subtitle is its own piece and is drawn at the top of the scrolling body.
/// Putting that sentence in the fixed header left the body too short to reach
/// at 200% text (R4).
pub(super) struct ModalContent {
    pub(super) header: Div,
    pub(super) subtitle: Option<Div>,
    pub(super) body: AnyElement,
    pub(super) footer: Option<Div>,
}

impl ShelluneView {
    // -----------------------------------------------------------------------
    // Top bar
    // -----------------------------------------------------------------------

    /// Whether the hosts home is what the content area shows.
    pub(super) fn home_is_shown(&self) -> bool {
        match self.workspace.destination() {
            Destination::Settings => false,
            Destination::LocalShells => !self.shows_terminal(Destination::LocalShells),
            _ => true,
        }
    }

    /// The window's head: the tab strip and, in the same surface under it, what the
    /// current session is doing (R12).
    ///
    /// One band, not two. The tab carries the name, its own colour and its state dot;
    /// this row carries what changes while that tab stays the same - the target, a
    /// state that is not "connected", a bridge or a save that needs an answer - and the
    /// session's actions. Drawing the name again under the tabs is what made the old
    /// window spend two rows on one object.
    pub(super) fn session_header(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .bg(theme::swatch(palette::SURFACE))
            .border_b_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(|| "session.header".to_owned())
            .child(self.top_bar(window, cx))
            .children(self.session_context(cx))
    }

    /// The top row: brand, the hosts tab, the terminal tabs, a new-tab control, the
    /// blank caption that drags the window, search, settings and (on Windows) the
    /// window buttons.
    ///
    /// It draws no surface of its own: it is the first row of the session header, which
    /// owns the band's background and its one bottom edge (R12).
    pub(super) fn top_bar(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let platform = Platform::current();
        let height = metrics.header_height();

        let leading_room = if platform == Platform::MacOs {
            metrics.scale(super::titlebar::TRAFFIC_LIGHT_ROOM)
        } else {
            metrics.scale(spacing::SNUG)
        };
        let brand = self
            .drag_area("brand.drag")
            .flex()
            .flex_none()
            .items_center()
            .h(height)
            .pl(leading_room)
            .pr(metrics.scale(spacing::TIGHT))
            .when(platform != Platform::MacOs, |row| {
                row.child(brand_mark(metrics.scale(22.0), metrics))
            });

        let tabs = div()
            .flex()
            .items_center()
            .flex_initial()
            .min_w(px(0.0))
            .overflow_hidden()
            .gap(metrics.scale(2.0))
            .child(self.home_tab(cx))
            .when(!self.workspace.layout().tabs().is_empty(), |row| {
                row.child(
                    div()
                        .flex_none()
                        .w(px(1.0))
                        .h(metrics.scale(18.0))
                        .mx(metrics.scale(spacing::HAIRLINE))
                        .bg(theme::swatch(palette::BORDER_STRONG)),
                )
            })
            .child(self.tab_row(cx).flex_initial());

        let trailing = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(metrics.scale(2.0))
            .pr(metrics.scale(spacing::HAIRLINE))
            .child(
                self.bar_icon(
                    "palette.open",
                    Icon::Search,
                    match self.keymap.label("palette.open") {
                        Some(shortcut) => format!("{} ({shortcut})", ux.search),
                        None => ux.search.to_owned(),
                    },
                    false,
                )
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.toggle_palette(window, cx);
                })),
            )
            .child(
                self.bar_icon(
                    "nav.settings",
                    Icon::Settings,
                    match crate::app::keymap::page_action(Destination::Settings)
                        .and_then(|spec| self.keymap.label(spec.id))
                    {
                        Some(shortcut) => format!("{} ({shortcut})", ux.settings_title),
                        None => ux.settings_title.to_owned(),
                    },
                    self.workspace.destination() == Destination::Settings,
                )
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.select(Destination::Settings, window, cx);
                })),
            );

        let controls = self.window_controls(window, cx);
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(height)
            .w_full()
            .debug_selector(|| "topbar".to_owned())
            .child(brand)
            .child(tabs)
            // Beside the strip rather than inside it: the strip clips what does not
            // fit, and the list of every tab has to stay reachable when it does (R2).
            .children(self.tab_list_control(cx))
            .child(self.new_tab_control(cx))
            .child(
                self.drag_area("header.drag")
                    .flex_1()
                    .h_full()
                    .min_w(metrics.scale(24.0)),
            )
            .child(trailing)
            .children(controls)
    }

    /// The second row of the head: where the current session stands, and what can be
    /// done to it (R12).
    ///
    /// Nothing here repeats the tab. The name, its colour and its state dot belong to
    /// the tab; this row carries the target that is not the name, the states that are
    /// news (connecting, disconnected, ended), the bridge or the save that needs an
    /// answer, and the actions. The name was drawn here as well, which is exactly the
    /// duplication this row removes.
    fn session_context(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.workspace.destination() != Destination::LocalShells
            || !self.shows_terminal(Destination::LocalShells)
        {
            return None;
        }
        let id = if self.workspace.layout().active_tab().is_some() {
            self.workspace.layout().active_session()
        } else {
            self.active
        }?;
        let session = self.workspace.session(id)?;
        let metrics = self.metrics();
        let strings = self.strings();
        let live = matches!(session.state(), SessionState::Connected);
        // A local shell is described by the program it runs - the whole path, since the
        // tab already carries the short name - and a remote one by what it dialled.
        let remote = matches!(session.kind(), SessionKind::Remote { .. });
        let target = self.target_of(session);
        // A local shell's program is often its own title: the tab already says it, so
        // the row keeps only what the tab does not say.
        let label = self.tab_title(id);
        let target = (target.to_lowercase() != label.to_lowercase()).then_some(target);
        let state_label = copy::session_state_label(session.state(), self.workspace.locale());
        let tone = session_tone(session.state());
        let can_reconnect = may_reconnect(session);
        // Only a connection that can really browse offers the file view: a local
        // session has no server to ask, and a session whose connection ended has no
        // directory left to show.
        let browsable = self.files.can_browse(id);
        let files_open = self.files.is_open(id);
        let unsaved = self.unsaved.iter().any(|(session, _)| *session == id);
        // Terminal and files are two views of the same connection: a switch, not a
        // pair of open and close buttons.
        let files_button = browsable.then(|| {
            let ux = copy::ux();
            let view_segment =
                |selector: &'static str, icon: Icon, label: &'static str, selected: bool| {
                    let color = theme::swatch(if selected {
                        palette::TEXT_PRIMARY
                    } else {
                        palette::TEXT_SECONDARY
                    });
                    components::segment("", selected, metrics)
                        .flex()
                        .items_center()
                        .gap(metrics.scale(spacing::HAIRLINE + 1.0))
                        .child(icons::icon(icon, metrics.small_icon_size(), color))
                        .child(label)
                        .id(selector)
                        .debug_selector(move || selector.to_owned())
                };
            // One line. The shared track wraps so a settings row can shrink, and that
            // same wrap stacked these two segments until the head was tall enough to
            // leave the file pane shorter than the failure it has to show.
            components::segmented(metrics)
                .flex_none()
                .flex_nowrap()
                .child(
                    view_segment(
                        "files.close",
                        Icon::Terminal,
                        ux.settings_terminal,
                        !files_open,
                    )
                    // The way back to the terminal is a stop of the file view's keyboard
                    // walk, so it can be reached with Tab and pressed with Enter (R4). It
                    // answers the ring through the same handler the columns use, because
                    // the walk is one list whichever of its controls holds the keys.
                    .track_focus(&self.files_close_focus)
                    .focus(files_focus_ring)
                    .on_key_down(cx.listener(move |view, event: &KeyDownEvent, window, cx| {
                        if view.files_key(id, FileSource::Remote, event, window, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        if view.files.is_open(id) {
                            view.toggle_files(id, cx);
                            view.reclaim_focus(_window, cx);
                        }
                    })),
                )
                .child(
                    view_segment(
                        "session.files",
                        Icon::Folder,
                        strings.files_open,
                        files_open,
                    )
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        if !view.files.is_open(id) {
                            view.toggle_files(id, cx);
                        }
                    })),
                )
        });

        // One line: what this session is on the left, its actions on the right. The
        // line wraps as a whole, so a narrow window or large text moves the actions
        // under the target instead of squeezing either into a chip (R4).
        let actions_row = div()
            .flex()
            .flex_shrink()
            .min_w(px(0.0))
            .max_w_full()
            .flex_wrap()
            .items_center()
            .justify_end()
            .ml_auto()
            .gap(metrics.scale(spacing::HAIRLINE))
            .children(self.split_buttons(cx))
            .when_some(files_button, |row, button| row.child(button))
            // The two actions that are the reason the user is looking at this row stay
            // on it; everything else is one menu away (R5).
            .when(can_reconnect, |row| {
                row.child(self.icon_text_button(
                    SharedString::from("session.reconnect"),
                    Icon::Refresh,
                    strings.session_reconnect,
                    Variant::Primary,
                    cx,
                    move |view, cx| view.reconnect_session(id, cx),
                ))
            })
            .when(unsaved, |row| {
                row.child(self.icon_text_button(
                    SharedString::from("session.save_host"),
                    Icon::Server,
                    strings.quick_save_host,
                    Variant::Primary,
                    cx,
                    move |view, cx| view.save_quick_host(id, cx),
                ))
            })
            .child(
                self.bar_icon(
                    "session.more",
                    Icon::More,
                    strings.session_more.to_owned(),
                    self.session_more.is_some(),
                )
                // Opened on the press, like the tab menu: the head's button is a
                // control of the shell, and a menu that took the keyboard from the
                // terminal would leave Escape unanswered (R5).
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |view, event: &gpui::MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        view.open_session_more(id, event.position, window, cx);
                    }),
                ),
            );
        let close = components::icon_only_button(Icon::Close, metrics)
            .id("session.close")
            .debug_selector(|| "session.close".to_owned())
            .tooltip(components::tooltip(strings.session_close))
            .on_click(cx.listener(move |view, _event, _window, cx| {
                view.request_close_session(id, cx);
            }));
        // A wide window has room for one line: the target on the left, every action and
        // Close on the right. A narrow one (or large text) keeps Close beside the target
        // and gives the actions a line of their own, which is the shortest this row can
        // be, so the terminal keeps its room (R4).
        let compact = self.viewport_width < metrics.scale(CONTEXT_ONE_LINE_WIDTH);
        let (actions_row, close) = if compact {
            (
                actions_row.w_full(),
                Some(div().flex_none().ml_auto().child(close)),
            )
        } else {
            (
                actions_row
                    .child(
                        components::divider_vertical(metrics).mx(metrics.scale(spacing::HAIRLINE)),
                    )
                    .child(close),
                None,
            )
        };

        let row = div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .w_full()
            .gap_x(metrics.scale(spacing::TIGHT))
            .gap_y(metrics.scale(spacing::HAIRLINE))
            .min_h(metrics.session_bar_height())
            .py(px(1.0))
            .pl(metrics.scale(spacing::SNUG))
            .pr(metrics.scale(spacing::HAIRLINE + 2.0))
            .debug_selector(|| "session.context".to_owned())
            .child(
                div()
                    .flex()
                    .flex_grow()
                    .flex_shrink()
                    .flex_basis(gpui::auto())
                    .min_w(px(0.0))
                    .flex_wrap()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    // What kind of session this is opens the row, so the line is never
                    // empty and a local shell is not mistaken for a server.
                    .child(components::kind_badge(
                        if remote {
                            strings.session_kind_remote
                        } else {
                            strings.session_kind_local
                        },
                        remote,
                        metrics,
                    ))
                    .when_some(target, |row, target| {
                        row.child(
                            div()
                                .flex_none()
                                .max_w_full()
                                .whitespace_normal()
                                .font_family(terminal::monospace_family())
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                .debug_selector(|| "session.target".to_owned())
                                .child(SharedString::from(target)),
                        )
                    })
                    .when(!live, |row| {
                        row.child(components::chip(state_label, tone, metrics))
                    })
                    .when_some(self.x11.get(&id).cloned(), |row, status| {
                        row.child(self.x11_chip(&status, metrics))
                    })
                    .when(unsaved, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .max_w_full()
                                .min_w(px(0.0))
                                .whitespace_normal()
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED))
                                .child(strings.quick_unsaved_note),
                        )
                    }),
            )
            .children(close)
            .child(actions_row);
        Some(row)
    }
    /// A square icon button of the top bar.
    fn bar_icon(
        &self,
        id: &'static str,
        icon: Icon,
        tooltip: String,
        selected: bool,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let size = metrics.scale(30.0);
        components::icon_only_button(icon, metrics)
            .size(size)
            .rounded(px(radius::MEDIUM))
            .when(selected, |button| {
                button.bg(theme::swatch(palette::SURFACE_SELECTED))
            })
            .id(id)
            .debug_selector(move || id.to_owned())
            .tooltip(components::tooltip(SharedString::from(tooltip)))
    }

    /// The pinned first tab: the hosts home.
    fn home_tab(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let selected = self.home_is_shown();
        let hover = theme::swatch(palette::SURFACE_HOVER);
        let count = self.book.hosts().len();
        // The hint reads the binding in force, so a rebound or unbound shortcut is
        // described as it really is (R5).
        let shortcut = crate::app::keymap::page_action(Destination::Hosts)
            .and_then(|spec| self.keymap.label(spec.id));
        let tooltip = match shortcut {
            Some(shortcut) => format!("{} · {shortcut}", strings.nav_hosts),
            None => strings.nav_hosts.to_owned(),
        };
        let mut tab = div()
            .id("nav.hosts")
            .debug_selector(|| "nav.hosts".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .gap(metrics.scale(spacing::HAIRLINE + 2.0))
            .h(metrics.tab_height())
            .px(metrics.scale(spacing::SNUG - 2.0))
            .rounded(px(radius::MEDIUM))
            .text_size(metrics.label_size())
            .cursor_pointer()
            .tooltip(components::tooltip(SharedString::from(tooltip)))
            .on_click(cx.listener(|this, _event, window, cx| {
                this.select(Destination::Hosts, window, cx);
            }));
        tab = if selected {
            tab.bg(theme::swatch(palette::SURFACE_RAISED))
                .border_1()
                .border_color(theme::swatch(palette::BORDER_STRONG))
                .shadow_sm()
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .font_weight(FontWeight::MEDIUM)
        } else {
            tab.border_1()
                .border_color(theme::clear())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .hover(move |style| style.bg(hover))
        };
        tab.child(icons::icon(
            Icon::Server,
            metrics.small_icon_size(),
            theme::swatch(if selected {
                palette::ACCENT
            } else {
                palette::TEXT_MUTED
            }),
        ))
        .child(strings.nav_hosts)
        .when(count > 0, |tab| {
            tab.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(SharedString::from(count.to_string())),
            )
        })
    }

    /// "+" opens the default shell in a new tab; the chevron lists every shell, quick
    /// connect and the saved hosts.
    fn new_tab_control(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let open = self.shell_menu_open;
        let size = metrics.scale(28.0);
        let new_shell = self.keymap.label("shell.new");
        let tooltip = match new_shell {
            Some(shortcut) => format!("{} ({shortcut})", ux.new_tab),
            None => ux.new_tab.to_owned(),
        };
        let control = div()
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .ml(metrics.scale(spacing::HAIRLINE))
            .child(
                components::icon_only_button(Icon::Plus, metrics)
                    .size(size)
                    .rounded_l(px(radius::MEDIUM))
                    .rounded_r(px(0.0))
                    .id("header.primary")
                    .debug_selector(|| "header.primary".to_owned())
                    .tooltip(components::tooltip(SharedString::from(tooltip)))
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.new_local_shell(window, cx);
                    })),
            )
            .child(
                components::icon_only_button(Icon::ChevronDown, metrics)
                    // Its own width, but never narrower than a pointer target (R4).
                    .w(metrics.hit_target())
                    .h(size)
                    .rounded_r(px(radius::MEDIUM))
                    .rounded_l(px(0.0))
                    .when(open, |button| {
                        button.bg(theme::swatch(palette::SURFACE_SELECTED))
                    })
                    .id("shell.menu")
                    .debug_selector(|| "shell.menu".to_owned())
                    .when(!open, |button| {
                        button.tooltip(components::tooltip(ux.new_tab_menu))
                    })
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.shell_menu_open = !this.shell_menu_open;
                        cx.notify();
                    })),
            );
        // Records where the control is drawn, for the menu that hangs from it.
        let anchor = self.shell_menu_anchor.clone();
        control.child(
            gpui::canvas(
                move |bounds, _window, _cx| anchor.set(bounds),
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        )
    }

    // -----------------------------------------------------------------------
    // Hosts home
    // -----------------------------------------------------------------------

    /// Local shells and saved servers on one page: both are ways to open a
    /// terminal, they only connect differently.
    pub(super) fn home_page(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let state = self.workspace.state(Destination::Hosts);

        let heading = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_wrap()
            .gap(metrics.scale(spacing::SNUG))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(spacing::HAIRLINE))
                    .min_w(px(0.0))
                    .child(
                        div()
                            .text_size(metrics.hero_size())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .debug_selector(|| "header.title".to_owned())
                            .child(ux.home_title),
                    )
                    // On a short window the sentence under the title costs the list a row
                    // and says nothing the page does not show; it goes first.
                    .when(self.viewport_height >= metrics.scale(600.0), |column| {
                        column.child(
                            div()
                                .text_size(metrics.body_size())
                                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                .child(ux.home_subtitle),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        components::icon_label_button(
                            Icon::Plug,
                            strings.quick_connect,
                            Variant::Secondary,
                            metrics,
                        )
                        .id("welcome.quick")
                        .debug_selector(|| "welcome.quick".to_owned())
                        .tooltip(components::tooltip(strings.quick_connect_hint))
                        .on_click(cx.listener(
                            |this, _event, window, cx| {
                                this.open_quick_connect(window, cx);
                            },
                        )),
                    )
                    .child(
                        components::icon_label_button(
                            Icon::Plus,
                            strings.primary_hosts,
                            Variant::Primary,
                            metrics,
                        )
                        .id("hosts.new")
                        .debug_selector(|| "hosts.new".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                if this.workspace.state(Destination::Hosts) == SectionState::Failed
                                {
                                    this.reload_hosts(cx);
                                } else {
                                    this.open_create_host(cx);
                                }
                            },
                        )),
                    ),
            );

        // What the page says about itself on top: the heading and the shells of this
        // computer. It is capped and scrolls on its own, so the servers and their filters
        // keep the rest of the window instead of being pushed below the fold at the
        // minimum size (R8).
        let cap = (self.viewport_height * 0.42)
            .max(metrics.scale(120.0))
            .min(metrics.scale(260.0));
        let header = div()
            .id("home.header")
            .flex()
            .flex_col()
            .flex_none()
            .min_h(px(0.0))
            .max_h(cap)
            .overflow_y_scroll()
            .gap(metrics.scale(spacing::SNUG))
            .child(heading)
            .children(
                self.local_error
                    .as_ref()
                    .map(|reason| self.notice_line(reason.clone(), Tone::Danger)),
            )
            .child(self.home_local_section(!self.group_column_fits(), cx));

        let mut column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .max_w(metrics.scale(super::HOME_MAX_WIDTH))
            .mx_auto()
            .px(metrics.scale(super::HOME_SIDE_PADDING))
            .pt(metrics.scale(spacing::SNUG))
            .pb(metrics.scale(spacing::SNUG + 4.0))
            .gap(metrics.scale(spacing::SNUG))
            .debug_selector(|| "home".to_owned())
            .child(header);

        // The empty first run explains itself in a card; every other state shows the
        // servers themselves, which is a virtualized list that has to be given the
        // height left over instead of being scrolled as one long column (R18).
        let welcome = matches!(state, SectionState::Empty | SectionState::Populated)
            && self.book.hosts().is_empty()
            && self.hosts.search().trim().is_empty()
            && !self.import.is_open();
        let hosts = if welcome {
            self.home_welcome(cx)
        } else {
            self.hosts_destination(cx)
        };
        column = column.child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .gap(metrics.scale(spacing::SNUG))
                .child(components::section_label(ux.home_hosts_section, metrics))
                .child(hosts),
        );

        // The page holds the keyboard of whichever destination shows it, so the
        // shortcuts keep working after a click on the page or a page switch.
        let page = div()
            .id("content.scroll")
            .debug_selector(|| "content".to_owned())
            .track_focus(self.destination_focus(self.workspace.destination()))
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .bg(theme::swatch(palette::CANVAS));
        if welcome || matches!(state, SectionState::Loading | SectionState::Failed) {
            // A card or a failure is read by scrolling the page, as before.
            page.overflow_y_scroll().child(column)
        } else {
            page.child(column.flex_1().min_h(px(0.0)).overflow_hidden())
        }
    }

    /// The shells of this computer, as cards that open in one click.
    ///
    /// On a narrow window the same choices are drawn as one row of buttons: the servers
    /// and their filters are the page's main content there, and a card per shell would
    /// push the list below the fold (R8).
    fn home_local_section(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let mut tiles = div()
            .flex()
            .flex_wrap()
            .items_center()
            .w_full()
            .gap(metrics.scale(spacing::TIGHT + 2.0))
            .debug_selector(|| "shell.picker".to_owned());
        for (index, shell) in self.workspace.shells().iter().enumerate() {
            let selected = index == self.workspace.shell_choice();
            if compact {
                tiles = tiles.child(
                    components::compact_button(
                        shell.name().to_owned(),
                        if selected {
                            Variant::Secondary
                        } else {
                            Variant::Ghost
                        },
                        metrics,
                    )
                    .id(("shell.choice", index))
                    .debug_selector(move || format!("shell.choice.{index}"))
                    .tooltip(components::tooltip(ux.home_local_hint))
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        this.launch_choice(index, window, cx);
                    })),
                );
            } else {
                tiles = tiles.child(
                    self.home_card(
                        Icon::Terminal,
                        shell.name().to_owned(),
                        program_label(shell.program()),
                        selected.then_some(strings.shell_default),
                    )
                    .id(("shell.choice", index))
                    .debug_selector(move || format!("shell.choice.{index}"))
                    .tooltip(components::tooltip(ux.home_local_hint))
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        this.launch_choice(index, window, cx);
                    })),
                );
            }
        }
        if compact {
            tiles = tiles.child(
                components::compact_button(
                    format!("{}{ELLIPSIS}", strings.shell_custom_button),
                    Variant::Ghost,
                    metrics,
                )
                .id("shell.custom")
                .debug_selector(|| "shell.custom".to_owned())
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.open_custom_shell(window, cx);
                })),
            );
        } else {
            tiles = tiles.child(
                self.home_card(
                    Icon::FolderOpen,
                    format!("{}{ELLIPSIS}", strings.shell_custom_button),
                    strings.shell_custom_hint.to_owned(),
                    None,
                )
                .id("shell.custom")
                .debug_selector(|| "shell.custom".to_owned())
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.open_custom_shell(window, cx);
                })),
            );
        }
        // The label is what the cards need to be told apart from the servers; the row of
        // buttons sits directly under the heading and needs no second title (R4, R8).
        let mut section = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .when(!compact, |section| {
                section.child(components::section_label(ux.home_local_section, metrics))
            });
        if self.workspace.shells().is_empty() {
            section = section.child(
                components::banner(Tone::Warning, metrics)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(strings.shell_none_found),
            );
        }
        section.child(tiles)
    }

    /// One card of the home page: an icon tile, a name, a detail line and an
    /// optional badge.
    fn home_card(
        &self,
        icon: Icon,
        name: String,
        detail: String,
        badge: Option<&'static str>,
    ) -> Div {
        let metrics = self.metrics();
        let hover_border = theme::swatch(palette::BORDER_STRONG);
        let hover_bg = theme::swatch(palette::SURFACE_HOVER);
        // A launcher, not a feature card: one line tall, so the servers below keep the
        // window. The icon sits on the accent's quiet tint instead of a gradient tile -
        // the accent is for the current object and the primary action, and a row of
        // saturated tiles shouted louder than the button that adds a host.
        div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT + 2.0))
            .w(metrics.scale(212.0))
            .px(metrics.scale(spacing::SNUG - 2.0))
            .py(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .cursor_pointer()
            .hover(move |style| style.border_color(hover_border).bg(hover_bg))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(metrics.scale(30.0))
                    .rounded(px(radius::SMALL + 1.0))
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
                            .flex()
                            .items_center()
                            .gap(metrics.scale(spacing::HAIRLINE + 2.0))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(metrics.body_size())
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                    .child(SharedString::from(name)),
                            )
                            .children(badge.map(|badge| components::tag(badge, metrics))),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(detail)),
                    ),
            )
    }

    /// The first-run card of the servers section: what a host is, and the three
    /// ways to get one.
    fn home_welcome(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let mut column = div().flex().flex_col().gap(metrics.scale(spacing::SNUG));
        if let Some(message) = self.hosts.message() {
            column = column.child(
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
        column.child(
            div()
                .flex()
                .items_center()
                .gap(metrics.scale(spacing::LOOSE))
                .p(metrics.scale(spacing::LOOSE))
                .rounded(px(radius::XLARGE))
                .bg(theme::swatch(palette::SURFACE_RAISED))
                .border_1()
                .border_color(theme::swatch(palette::BORDER))
                .shadow_xs()
                .debug_selector(|| "hosts.empty".to_owned())
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(metrics.scale(56.0))
                        .rounded(px(radius::XLARGE))
                        .bg(super::brand_gradient())
                        .shadow_md()
                        .child(icons::icon(
                            Icon::Server,
                            metrics.scale(26.0),
                            theme::swatch(palette::ON_ACCENT),
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.0))
                        .gap(metrics.scale(spacing::TIGHT))
                        .child(
                            div()
                                .text_size(metrics.title_size())
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                .child(ux.home_welcome_title),
                        )
                        .child(
                            div()
                                .text_size(metrics.label_size())
                                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                .child(ux.home_welcome_body),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap(metrics.scale(spacing::TIGHT))
                                .pt(metrics.scale(spacing::HAIRLINE))
                                .child(
                                    components::icon_label_button(
                                        Icon::Plus,
                                        strings.primary_hosts,
                                        Variant::Primary,
                                        metrics,
                                    )
                                    .id("hosts.empty.new")
                                    .debug_selector(|| "hosts.empty.new".to_owned())
                                    .on_click(cx.listener(
                                        |this, _event, _window, cx| {
                                            this.open_create_host(cx);
                                        },
                                    )),
                                )
                                .child(
                                    components::icon_label_button(
                                        Icon::Import,
                                        ux.home_import,
                                        Variant::Ghost,
                                        metrics,
                                    )
                                    .id("hosts.import.open")
                                    .debug_selector(|| "hosts.import.open".to_owned())
                                    .on_click(cx.listener(
                                        |this, _event, _window, cx| {
                                            this.open_import(cx);
                                        },
                                    )),
                                ),
                        ),
                ),
        )
    }

    /// The capture layer: what the next combination pressed will be bound to.
    ///
    /// It sits over the window and reads the keyboard through an interceptor, so the
    /// combination is seen before any action it is already bound to runs.
    pub(super) fn key_capture_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let capture = self.key_capture.as_ref()?;
        let spec = crate::app::keymap::action(capture.action)?;
        let metrics = self.metrics();
        let ux = copy::ux();
        let locale = self.workspace.locale();
        let platform = self.keymap.platform();
        let viewport = window.viewport_size();
        let width = px(460.0).min(viewport.width - metrics.scale(spacing::LOOSE) * 2.0);

        let current = self.keymap.label(spec.id);
        let card = div()
            .occlude()
            .flex()
            .flex_col()
            .w(width)
            .gap(metrics.scale(spacing::SNUG))
            .p(metrics.scale(spacing::LOOSE))
            .rounded(px(radius::LARGE))
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .border_1()
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .shadow_lg()
            .debug_selector(|| "keys.capture".to_owned())
            .child(
                div()
                    .text_size(metrics.title_size())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(ux.keys_capture_title),
            )
            .child(
                div()
                    .text_size(metrics.body_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(SharedString::from(spec.name(locale))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(
                                current
                                    .clone()
                                    .unwrap_or_else(|| ux.keys_unbound.to_owned()),
                            )),
                    ),
            )
            .when_some(capture.problem.as_ref(), |card, problem| {
                card.child(
                    components::banner(Tone::Danger, metrics)
                        .debug_selector(|| "keys.capture.error".to_owned())
                        .child(SharedString::from(problem.text(locale, platform))),
                )
            })
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(ux.keys_capture_hint),
            )
            .child(
                div().flex().justify_end().child(
                    components::compact_button(ux.keys_capture_cancel, Variant::Ghost, metrics)
                        .id("keys.capture.cancel")
                        .debug_selector(|| "keys.capture.cancel".to_owned())
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.keys_capture_cancel(cx);
                        })),
                ),
            );

        Some(
            super::overlay_layer().child(
                div()
                    .relative()
                    .w(viewport.width)
                    .h(viewport.height)
                    .child(
                        div()
                            .id("keys.capture.backdrop")
                            .absolute()
                            .inset_0()
                            .occlude()
                            .on_any_mouse_down(cx.listener(|this, _event, _window, cx| {
                                this.keys_capture_cancel(cx);
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(card),
                    ),
            ),
        )
    }
    /// A segmented control whose segments call `pick` with their index.
    pub(super) fn segments(
        &self,
        id: &'static str,
        options: Vec<(String, bool)>,
        cx: &mut Context<Self>,
        pick: impl Fn(&mut Self, usize, &mut Context<Self>) + Clone + 'static,
    ) -> Div {
        let metrics = self.metrics();
        // The track's own floor is the sum of its segments' floors, capped at what a
        // settings row has at the minimum window. Without it the track measured its
        // labels' natural widths, which are narrower than the floors of short labels
        // ("Full"), and the last segment wrapped under the others in a wide window.
        let floor = options
            .iter()
            .map(|(label, _)| components::segment_reserved_width(label, metrics))
            .fold(px(0.0), |sum, width| sum + width)
            + metrics.scale(2.0) * options.len().saturating_sub(1) as f32
            + metrics.scale(3.0) * 2.0
            + px(2.0);
        let mut control = components::segmented(metrics)
            .min_w(floor.min(components::settings_row_content_cap(metrics)))
            .debug_selector(move || id.to_owned());
        for (index, (label, selected)) in options.into_iter().enumerate() {
            let pick = pick.clone();
            let reserved = components::segment_reserved_width(&label, metrics);
            control = control.child(
                components::segment(label, selected, metrics)
                    .min_w(reserved)
                    .max_w_full()
                    .flex_shrink()
                    .whitespace_normal()
                    .h(gpui::auto())
                    .min_h(metrics.compact_control_height())
                    .id(SharedString::from(format!("{id}.{index}")))
                    .debug_selector(move || format!("{id}.{index}"))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        pick(this, index, cx);
                    })),
            );
        }
        control
    }

    /// Sets the theme and repaints the terminals in it.
    pub(super) fn set_theme(&mut self, choice: ThemeChoice, cx: &mut Context<Self>) {
        self.theme_choice = choice;
        self.repaint_terminals(cx);
        cx.notify();
    }

    /// Sets the terminal font size for new terminals and every open one.
    pub(super) fn set_terminal_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        self.workspace.set_local_font_size(size);
        let size = self.workspace.local_font_size();
        let panes: Vec<_> = self.panes.iter().map(|open| open.pane.clone()).collect();
        for pane in panes {
            pane.update(cx, |pane, cx| pane.set_font_size(size, cx));
        }
        cx.notify();
    }

    /// Switches one background reminder on or off (R7).
    ///
    /// The choice is written with the other preferences on the next draw, so it
    /// survives a restart. Turning a reminder off drops the marks it already made and
    /// touches nothing else: no bell byte is eaten, no block is rewritten and no reason
    /// for a disconnect is hidden.
    pub(super) fn set_attention_notice(
        &mut self,
        kind: AttentionKind,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.set_attention(kind, on) {
            self.persist_preferences();
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Dialogs
    // -----------------------------------------------------------------------

    /// A centred dialog over a dimmed window. A press on the dimmed part does not
    /// close it: what was typed is kept until the user closes it on purpose.
    pub(super) fn modal(
        &self,
        window: &Window,
        id: &'static str,
        width: f32,
        content: ModalContent,
    ) -> Div {
        let ModalContent {
            header,
            subtitle,
            body,
            footer,
        } = content;
        let metrics = self.metrics();
        let viewport = window.viewport_size();
        // The inset stays a small edge gap. Scaling it with the text turned a 200%
        // minimum window into a short dialog, and the failure's next line was drawn
        // past the client edge where it could not be read (R4).
        let margin = metrics.scale(spacing::LOOSE).min(px(16.0));
        let width = metrics.scale(width).min(viewport.width - margin * 2.0);
        let max_height = viewport.height - margin * 2.0;
        let dialog = div()
            .id(id)
            .debug_selector(move || id.to_owned())
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
            .child(
                div()
                    .id(SharedString::from(format!("{id}.body")))
                    .debug_selector(move || format!("{id}.body"))
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .children(subtitle)
                    .child(body),
            )
            .children(footer.map(|footer| footer.flex_none()));
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
                        .id(SharedString::from(format!("{id}.backdrop")))
                        .absolute()
                        .inset_0()
                        .bg(gpui::rgba(0x0508_1470))
                        .occlude()
                        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                            cx.stop_propagation();
                        }),
                )
                .child(dialog),
        )
    }

    /// The header of a dialog: an optional icon tile, a title, a subtitle and a
    /// close button.
    pub(super) fn modal_header(
        &self,
        icon: Option<Icon>,
        title: String,
        subtitle: Option<String>,
        close_id: &'static str,
        cx: &mut Context<Self>,
        close: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> (Div, Option<Div>) {
        self.modal_header_with(icon, title, subtitle, None, close_id, cx, close)
    }

    /// [`Self::modal_header`] with a badge under the title.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn modal_header_with(
        &self,
        icon: Option<Icon>,
        title: String,
        subtitle: Option<String>,
        badge: Option<Div>,
        close_id: &'static str,
        cx: &mut Context<Self>,
        close: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> (Div, Option<Div>) {
        let metrics = self.metrics();
        let lead = icon.map(|icon| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(metrics.scale(32.0))
                .rounded(px(radius::MEDIUM))
                .bg(theme::swatch(palette::ACCENT_TINT))
                .child(icons::icon(
                    icon,
                    metrics.icon_size(),
                    theme::swatch(palette::ACCENT),
                ))
        });
        self.modal_header_lead(lead, title, subtitle, badge, close_id, cx, close)
    }

    /// The header of a dialog with any leading tile: a host's avatar, or an icon.
    ///
    /// The title and the badge share one column beside the tile. The subtitle is
    /// returned separately and drawn at the top of the scrolling body: beside the
    /// icon it wrapped one glyph per line, and in the fixed header a full sentence
    /// at 200% text left the body 24px tall, so the editor could not be scrolled
    /// into view (R4). A title with a badge wraps on spaces: at 720×480 and 200% the
    /// column is narrower than one word, and truncate would cut a glyph with no
    /// ellipsis.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn modal_header_lead(
        &self,
        lead: Option<Div>,
        title: String,
        subtitle: Option<String>,
        badge: Option<Div>,
        close_id: &'static str,
        cx: &mut Context<Self>,
        close: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> (Div, Option<Div>) {
        let metrics = self.metrics();
        let wrapping = badge.is_some();
        let title = div()
            .min_w(px(0.0))
            .text_size(metrics.heading_size())
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .when(wrapping, |title| title.w_full().whitespace_normal())
            .when(!wrapping, |title| title.w_full().truncate())
            .child(SharedString::from(title));
        let column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .gap(metrics.scale(spacing::HAIRLINE))
            .child(title)
            .children(badge.map(|badge| div().flex().min_w(px(0.0)).child(badge)));
        let bar = div()
            .flex()
            .flex_none()
            .w_full()
            .min_w(px(0.0))
            .when(wrapping, |bar| bar.items_start())
            .when(!wrapping, |bar| bar.items_center())
            .gap(metrics.scale(spacing::SNUG))
            .pl(metrics.scale(spacing::LOOSE))
            .pr(metrics.scale(spacing::BASE))
            .pt(metrics.scale(spacing::SNUG + 2.0))
            .pb(metrics.scale(spacing::TIGHT))
            .children(lead)
            .child(column)
            .child(
                components::icon_only_button(Icon::Close, metrics)
                    .id(close_id)
                    .debug_selector(move || close_id.to_owned())
                    .tooltip(components::tooltip(copy::ux().tool_close))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        close(this, cx);
                    })),
            );
        let subtitle = subtitle.map(|subtitle| {
            div()
                // Whole words across the dialog. A single-line truncate cut the
                // sentence through a glyph and drew no ellipsis (R4).
                .w_full()
                .min_w(px(0.0))
                .whitespace_normal()
                .px(metrics.scale(spacing::LOOSE))
                .pb(metrics.scale(spacing::TIGHT))
                .text_size(metrics.label_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .debug_selector(|| "modal.subtitle".to_owned())
                .child(SharedString::from(subtitle))
        });
        (bar, subtitle)
    }

    /// The connection being set up, or the quick-connect form, as a dialog.
    pub(super) fn connection_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        if self.quick_open {
            let (header, subtitle) = self.modal_header(
                Some(Icon::Plug),
                strings.quick_connect_title.to_owned(),
                Some(ux.quick_hint.to_owned()),
                "quick.close",
                cx,
                |this, cx| {
                    this.quick_open = false;
                    this.quick_error = None;
                    cx.notify();
                },
            );
            let body = div()
                .px(metrics.scale(spacing::LOOSE))
                .pb(metrics.scale(spacing::LOOSE))
                .child(self.quick_panel(cx))
                .into_any_element();
            return Some(self.modal(
                window,
                "quick.dialog",
                520.0,
                ModalContent {
                    header,
                    subtitle,
                    body,
                    footer: None,
                },
            ));
        }
        if !(self.showing_connection_form && self.remote.is_some()) {
            return None;
        }
        let panel = self.remote.as_ref()?;
        let host = panel.host();
        let (status_label, tone) = super::remote_status_label(panel.status(), strings);
        // The hint is about choosing a sign-in; while the server asks a question or
        // the connection runs, it would only be in the way.
        // A failure already says what happened and what to do next. The sign-in hint
        // on top of it pushed that next line past the window (R4).
        let choosing = matches!(panel.status(), crate::app::remote::RemoteStatus::Idle);
        let failed = matches!(panel.status(), crate::app::remote::RemoteStatus::Failed(_));
        let (header, subtitle) = self.modal_header_lead(
            Some(host_avatar(host.name(), false, metrics)),
            format!("{} {}", ux.connect_title, host.name()),
            Some(host.target()),
            Some(components::chip(status_label, tone, metrics)),
            "remote.close",
            cx,
            |this, cx| this.close_remote(cx),
        );
        // What the attempt is really doing, and whether it is waiting for the user:
        // the stage is the transport's own state, not a progress animation that would
        // keep claiming to connect while a fingerprint or a ProxyCommand waits (R17).
        let asking = panel.status().is_asking();
        // The strip is about an attempt that is in flight or waiting for the user. A
        // finished attempt already says what happened in the failure banner, and an
        // idle form has not started anything, so neither gets a line claiming a stage
        // it does not have (R17).
        let show_stage = panel.status().is_busy() || asking;
        let waiting_on = match panel.status() {
            crate::app::remote::RemoteStatus::VerifyingHost(_) => {
                Some(strings.remote_wait_identity)
            }
            crate::app::remote::RemoteStatus::ConfirmingProxy { .. } => {
                Some(strings.remote_proxy_title)
            }
            crate::app::remote::RemoteStatus::ConfirmingX11 { .. } => {
                Some(strings.x11_trusted_title)
            }
            _ => None,
        };
        let body = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w(px(0.0))
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::LOOSE))
            .when(show_stage, |body| {
                body.child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap(metrics.scale(spacing::TIGHT))
                        .debug_selector(|| "remote.stage".to_owned())
                        .child(
                            div()
                                .flex_none()
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED))
                                .child(strings.remote_stage_title),
                        )
                        .child(components::status_dot(tone, metrics))
                        .child(
                            div()
                                .text_size(metrics.label_size())
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(tone.text())
                                .debug_selector(|| "remote.stage.value".to_owned())
                                .child(status_label),
                        )
                        .when(asking, |row| {
                            row.child(
                                components::chip(strings.remote_waiting, Tone::Warning, metrics)
                                    .debug_selector(|| "remote.waiting".to_owned()),
                            )
                        }),
                )
            })
            .children(waiting_on.map(|detail| {
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .debug_selector(|| "remote.waiting.detail".to_owned())
                    .child(SharedString::from(detail))
            }))
            // The address the attempt dials, spelled out beside the host's name so the
            // dialog never says "connecting" without saying to what (R17).
            .when(show_stage, |body| {
                body.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(metrics.scale(spacing::TIGHT))
                        .min_w(px(0.0))
                        .debug_selector(|| "remote.target".to_owned())
                        .child(
                            div()
                                .flex_none()
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED))
                                .child(strings.remote_target_label),
                        )
                        .child(
                            div()
                                .min_w(px(0.0))
                                .truncate()
                                .font_family(crate::app::terminal::monospace_family())
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                .child(SharedString::from(host.target())),
                        ),
                )
            })
            .when(choosing, |body| {
                body.child(
                    div()
                        .text_size(metrics.label_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .child(ux.connect_hint),
                )
            })
            .child(self.remote_panel(cx))
            .child(self.other_attempts(cx))
            .into_any_element();
        // A connection that is already running can go on without the dialog: it keeps
        // its tab, and clicking that tab brings the dialog back. A failure is finished,
        // so that button would only take the room the reason and the next step need.
        // An attempt that is really in flight can also be ended here, which is the
        // only place it can be ended while the dialog is still what is on screen.
        let can_background = self.form_session.is_some() && !failed;
        let can_cancel = panel.status().is_busy() || asking;
        let footer = (can_background || can_cancel).then(|| {
            div()
                .flex()
                .justify_end()
                .flex_wrap()
                .gap(metrics.scale(spacing::TIGHT))
                .px(metrics.scale(spacing::LOOSE))
                .py(metrics.scale(spacing::SNUG))
                .border_t_1()
                .border_color(theme::swatch(palette::BORDER))
                .when(can_cancel, |row| {
                    row.child(self.text_button(
                        SharedString::from("remote.cancel"),
                        strings.remote_cancel_attempt,
                        Variant::Secondary,
                        cx,
                        |view, cx| view.close_remote(cx),
                    ))
                })
                .when(can_background, |row| {
                    row.child(self.text_button(
                        SharedString::from("remote.hide"),
                        ux.connect_background,
                        Variant::Ghost,
                        cx,
                        |view, cx| view.hide_connection(cx),
                    ))
                })
        });
        Some(self.modal(
            window,
            "remote.dialog",
            600.0,
            ModalContent {
                header,
                subtitle,
                body,
                footer,
            },
        ))
    }

    /// Importing hosts from an SSH configuration, as a dialog: read, preview, then
    /// save only what the preview offered.
    pub(super) fn import_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        if !self.import.is_open() {
            return None;
        }
        let metrics = self.metrics();
        let strings = self.strings();
        let (header, subtitle) = self.modal_header(
            Some(Icon::Import),
            strings.import_title.to_owned(),
            Some(strings.import_read_only.to_owned()),
            "hosts.import.dialog.close",
            cx,
            |this, cx| this.close_import(cx),
        );
        let body = div()
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::LOOSE))
            .child(self.import_panel(cx))
            .into_any_element();
        Some(self.modal(
            window,
            "hosts.import.dialog",
            720.0,
            ModalContent {
                header,
                subtitle,
                body,
                footer: None,
            },
        ))
    }

    /// Starting any program by its path: a field, one line on what it takes, and
    /// the reason when the last try did not start.
    pub(super) fn custom_shell_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if !self.custom_shell_open {
            return None;
        }
        let metrics = self.metrics();
        let ux = copy::ux();
        let (header, subtitle) = self.modal_header(
            Some(Icon::FolderOpen),
            ux.custom_shell_title.to_owned(),
            None,
            "shell.custom.close",
            cx,
            |this, cx| {
                this.custom_shell_open = false;
                this.restore_focus_pending = true;
                cx.notify();
            },
        );
        let body = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::LOOSE))
            .child(
                div()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(ux.custom_shell_body),
            )
            .child(
                div()
                    .debug_selector(|| "shell.path".to_owned())
                    // Escape closes the dialog; without this the field would take it
                    // to empty itself, and the path just typed would be lost.
                    .capture_key_down(cx.listener(
                        |this, event: &gpui::KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                cx.stop_propagation();
                                this.close_custom_shell(window, cx);
                            }
                        },
                    ))
                    .child(self.shell_path.clone()),
            )
            .children(
                self.local_error
                    .as_ref()
                    .map(|reason| self.notice_line(reason.clone(), Tone::Danger)),
            )
            .into_any_element();
        let can_run = !self.shell_path.read(cx).text().trim().is_empty();
        let footer = div()
            .flex()
            .justify_end()
            .gap(metrics.scale(spacing::TIGHT))
            .px(metrics.scale(spacing::LOOSE))
            .py(metrics.scale(spacing::SNUG))
            .border_t_1()
            .border_color(theme::swatch(palette::BORDER))
            .child(self.text_button(
                SharedString::from("shell.custom.cancel"),
                ux.fwd_cancel,
                Variant::Ghost,
                cx,
                |this, cx| {
                    this.custom_shell_open = false;
                    this.restore_focus_pending = true;
                    cx.notify();
                },
            ))
            .child(
                components::button(ux.custom_shell_run, Variant::Primary, metrics)
                    .id("shell.custom.run")
                    .debug_selector(|| "shell.custom.run".to_owned())
                    .when(!can_run, |button| button.opacity(0.5))
                    .when(can_run, |button| {
                        button.on_click(cx.listener(|this, _event, window, cx| {
                            let text = this.shell_path.read(cx).text().to_owned();
                            if this.start_custom_shell(text, window, cx) {
                                this.shell_path
                                    .update(cx, |field, cx| field.set_text("", cx));
                            }
                        }))
                    }),
            );
        Some(self.modal(
            window,
            "shell.custom.dialog",
            520.0,
            ModalContent {
                header,
                subtitle,
                body,
                footer: Some(footer),
            },
        ))
    }

    /// Port forwarding or transfers, opened from a host or a session.
    pub(super) fn tool_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let tool = self.tool?;
        let metrics = self.metrics();
        let ux = copy::ux();
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.tool = None;
            this.forwards.close_editor();
            this.pending_exposure = None;
            // A question about one rule is about a page that is no longer on screen.
            this.forward_confirm = None;
            this.snippet_panel.keep();
            this.snippet_panel.end_edit();
            cx.notify();
        };
        match tool {
            Tool::Forwards => {
                let subtitle = match self.forwards.scope() {
                    crate::app::forwarding::ForwardScope::All => ux.tool_forwards_all.to_owned(),
                    crate::app::forwarding::ForwardScope::Host(id) => format!(
                        "{} {}",
                        ux.tool_forwards_host,
                        self.book.host(id).map_or("?", |host| host.name())
                    ),
                };
                let (header, hint) = self.modal_header(
                    Some(Icon::Forward),
                    ux.tool_forwards_title.to_owned(),
                    Some(subtitle),
                    "forwards.dialog.close",
                    cx,
                    close,
                );
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(spacing::SNUG))
                    .px(metrics.scale(spacing::LOOSE))
                    .pb(metrics.scale(spacing::LOOSE))
                    .child(self.forward_rows(
                        div().flex().flex_col().gap(metrics.scale(spacing::TIGHT)),
                        cx,
                    ))
                    .into_any_element();
                Some(self.modal(
                    window,
                    "forwards.dialog",
                    680.0,
                    ModalContent {
                        header,
                        subtitle: hint,
                        body,
                        footer: None,
                    },
                ))
            }
            Tool::Transfers => {
                let (header, subtitle) = self.modal_header(
                    Some(Icon::Transfer),
                    ux.tool_transfers_title.to_owned(),
                    Some(ux.tool_transfers_subtitle.to_owned()),
                    "transfers.dialog.close",
                    cx,
                    close,
                );
                let state = self.workspace.state(Destination::Transfers);
                let card = self.section_card(Destination::Transfers, state);
                let list = if state == SectionState::Populated {
                    self.rows_for(Destination::Transfers, card, cx)
                } else {
                    card
                };
                let body = div()
                    .px(metrics.scale(spacing::LOOSE))
                    .pb(metrics.scale(spacing::LOOSE))
                    .child(list)
                    .into_any_element();
                Some(self.modal(
                    window,
                    "transfers.dialog",
                    680.0,
                    ModalContent {
                        header,
                        subtitle,
                        body,
                        footer: None,
                    },
                ))
            }
            Tool::Snippets => Some(self.snippet_tool(window, cx, close)),
        }
    }

    /// Puts the connection dialog away while its attempt keeps running in its tab.
    pub(super) fn hide_connection(&mut self, cx: &mut Context<Self>) {
        self.park_focused_form();
        // The form now lives on its attempt; nothing is on screen until its tab asks
        // for it again.
        self.form_session = None;
        self.showing_connection_form = false;
        cx.notify();
    }

    /// Opens a tool over the current page.
    pub(super) fn open_tool(&mut self, tool: Tool, cx: &mut Context<Self>) {
        self.tool = Some(tool);
        self.shell_menu_open = false;
        cx.notify();
    }

    /// The page shown while no terminal is open on the terminals destination.
    #[allow(dead_code)]
    pub(super) fn terminals_empty(&self) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        components::state_card_with_icon(
            Tone::Neutral,
            Some(Icon::Terminal),
            ux.terminals_empty_title,
            ux.terminals_empty_body,
            None,
            metrics,
        )
    }

    /// Whether the hosts home fills the content area, for a test.
    #[cfg(test)]
    pub fn home_is_shown_for_test(&self) -> bool {
        self.home_is_shown()
    }

    /// The tool open over the page, for a test.
    #[cfg(test)]
    pub fn tool_for_test(&self) -> Option<Tool> {
        self.tool
    }
}

// ---------------------------------------------------------------------------
// Shortcut commands (R6)
// ---------------------------------------------------------------------------

impl ShelluneView {
    /// Opens the manager of the saved commands, on the list or on a new command.
    ///
    /// Opening it reads nothing new: the library was read when the window started, and a
    /// file that could not be read then is still reported here. Nothing is inserted into a
    /// terminal by opening the manager.
    pub(crate) fn open_snippets(&mut self, new: bool, cx: &mut Context<Self>) {
        self.open_tool(Tool::Snippets, cx);
        self.snippet_panel.keep();
        if new {
            self.open_snippet_editor(None, cx);
        } else {
            self.close_snippet_editor(cx);
            // The list takes the keyboard, so its arrow keys and Enter work without a
            // pointer. The editor's first field takes it instead when one is opened.
            self.snippet_focus_pending = Some(self.snippet_focus.clone());
        }
        cx.notify();
    }

    /// Opens the editor on one command, or on a command that has not been written yet.
    ///
    /// The fields are made per opening, so they start from what is really saved (or are
    /// empty for a new one) and their placeholders are in the language on screen. The name
    /// field takes the keyboard on the next draw, which is where a dialog's first field
    /// belongs.
    fn open_snippet_editor(&mut self, id: Option<SnippetId>, cx: &mut Context<Self>) {
        let ux = copy::ux();
        let (name_text, body_text) = match id.and_then(|id| self.snippets.get(id).cloned()) {
            Some(snippet) => (snippet.name().to_owned(), snippet.body().to_owned()),
            None => (String::new(), String::new()),
        };
        let name = cx.new(|cx| TextField::with_text(ux.snippet_name_placeholder, &name_text, cx));
        let body = cx.new(|cx| {
            let mut field = TextField::with_text(ux.snippet_body_placeholder, &body_text, cx);
            // A command may be a short block, so the body keeps its line breaks; Enter
            // still saves, which is what keeps the dialog on one keyboard path.
            field.set_keeps_line_breaks(true, cx);
            field
        });
        // Enter in either field saves what is typed. The field keeps its text so a refused
        // draft stays on screen beside the reason it was refused.
        let view = cx.entity().downgrade();
        for field in [name.clone(), body.clone()] {
            field.update(cx, |field, _cx| {
                let view = view.clone();
                field.on_submit(move |_text, _window, app| {
                    view.update(app, |view, cx| {
                        view.save_snippet_editor(cx);
                        false
                    })
                    .unwrap_or(false)
                });
            });
            // The dialog redraws as the draft changes, so the length warning appears while
            // the text can still be shortened rather than only after a refused save.
            cx.observe(&field, |_view, _field, cx| cx.notify()).detach();
        }
        let handle = name.read(cx).focus_handle().clone();
        self.snippet_panel.begin_edit(SnippetEditor {
            id,
            name,
            body,
            problem: None,
        });
        // The keyboard goes to the first field on the next draw, which is the only place
        // with a window to give it to.
        self.snippet_focus_pending = Some(handle);
        cx.notify();
    }

    /// Puts the editor away without saving anything. What was typed is dropped.
    pub(super) fn close_snippet_editor(&mut self, cx: &mut Context<Self>) {
        if self.snippet_panel.end_edit().is_some() {
            self.snippet_list_focus();
            cx.notify();
        }
    }

    /// Gives the manager's list the keyboard again, once the editor it replaced is gone.
    ///
    /// The field that had the keyboard belongs to the editor, so leaving focus on it would
    /// leave the dialog with nothing to type into; the list is what the next key press is
    /// about.
    fn snippet_list_focus(&mut self) {
        if self.tool == Some(Tool::Snippets) {
            self.snippet_focus_pending = Some(self.snippet_focus.clone());
        }
    }

    /// Saves what the editor holds, or says why it cannot be saved.
    ///
    /// The library writes the file first and adopts the change only once the file has it,
    /// so a failed save leaves the list as it was and the draft on screen.
    fn save_snippet_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.snippet_panel.editor() else {
            return;
        };
        let id = editor.id;
        let name_field = editor.name.clone();
        let body_field = editor.body.clone();
        let name = name_field.read(cx).text().to_owned();
        let body = body_field.read(cx).text().to_owned();
        let locale = self.workspace.locale();
        let outcome = match id {
            Some(id) => self.snippets.replace(id, &name, &body),
            None => self.snippets.create(&name, &body).map(|_id| ()),
        };
        match outcome {
            Ok(()) => {
                self.snippet_panel.end_edit();
                self.snippet_list_focus();
                self.toast(Tone::Positive, copy::ux().snippet_saved, None, cx);
            }
            Err(error) => {
                let reason = error.text(locale);
                if let Some(editor) = self.snippet_panel.editor_mut() {
                    editor.problem = Some(reason);
                }
            }
        }
        cx.notify();
    }

    /// Deletes the command the question was asked about.
    fn confirm_snippet_delete(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.snippet_panel.pending_delete() else {
            return;
        };
        self.snippet_panel.keep();
        let locale = self.workspace.locale();
        match self.snippets.remove(id) {
            Ok(true) => {
                let count = self.snippets.snippets().len();
                self.snippet_panel
                    .select(self.snippet_panel.selected(), count);
                self.toast(Tone::Positive, copy::ux().snippet_deleted, None, cx);
            }
            Ok(false) => {}
            Err(error) => {
                // The command is still in the library, because the file still holds it.
                self.toast(Tone::Danger, error.text(locale), None, cx);
            }
        }
        cx.notify();
    }

    /// Offers one saved command to the terminal on screen (R6).
    ///
    /// Inserting types the body and submits nothing; running types it and sends one line
    /// break. Both go through the pane's own paste rule, so a multi-line or high-risk body
    /// is confirmed first and cancelling writes nothing. Without a terminal on screen
    /// nothing is created and nothing is reported as run: the window says what to open
    /// first.
    ///
    /// What the corner says follows what the pane actually did. A body that reached the
    /// pane is reported as inserted or sent; a body held behind the paste prompt is not,
    /// because nothing has been written yet and cancelling writes nothing — the prompt in
    /// the pane is the only place that completion can come from.
    pub(crate) fn use_snippet(&mut self, id: SnippetId, execute: bool, cx: &mut Context<Self>) {
        let ux = copy::ux();
        let Some(body) = self.snippets.get(id).map(|s| s.body().to_owned()) else {
            self.toast(
                Tone::Danger,
                ux.snippet_problem_unknown.to_owned(),
                None,
                cx,
            );
            return;
        };
        let Some(pane) = self.active_pane().cloned() else {
            self.toast(
                Tone::Warning,
                ux.snippet_no_terminal.to_owned(),
                Some(ux.snippet_no_terminal_body.to_owned()),
                cx,
            );
            return;
        };
        // A pane whose shell has ended keeps its last screen; typing into it would go
        // nowhere, so nothing is reported as inserted or run.
        if !pane.read(cx).state().accepts_input() {
            self.toast(
                Tone::Warning,
                ux.snippet_terminal_ended.to_owned(),
                Some(ux.snippet_terminal_ended_body.to_owned()),
                cx,
            );
            return;
        }
        let offer = pane.update(cx, |pane, cx| pane.stage_snippet(&body, execute, cx));
        match offer {
            SnippetOffer::Written => {
                self.settle_snippet_note(cx);
                let title = if execute {
                    ux.snippet_sent
                } else {
                    ux.snippet_inserted
                };
                self.toast(Tone::Positive, title, None, cx);
            }
            SnippetOffer::Waiting => {
                // Nothing has been written: the corner points at the prompt instead of
                // reporting a command that did not go anywhere yet, and the note leaves
                // when the prompt is answered.
                self.settle_snippet_note(cx);
                let toast = self.toast(
                    Tone::Neutral,
                    ux.snippet_waiting.to_owned(),
                    Some(copy::fill(
                        ux.snippet_waiting_body,
                        &[(
                            "cancel",
                            self.workspace.locale().strings().terminal_paste_cancel,
                        )],
                    )),
                    cx,
                );
                if let Some(session) = self.active {
                    self.snippet_note = Some(SnippetNote { session, toast });
                }
            }
            // A stored body with nothing in it cannot type anything, so nothing is claimed.
            SnippetOffer::Empty => {
                self.settle_snippet_note(cx);
                self.toast(
                    Tone::Warning,
                    ux.snippet_problem_body_empty.to_owned(),
                    None,
                    cx,
                );
            }
        }
    }

    /// Takes the "waiting for confirmation" note away, if one is up.
    fn settle_snippet_note(&mut self, cx: &mut Context<Self>) {
        if let Some(note) = self.snippet_note.take() {
            self.dismiss_toast(note.toast, cx);
        }
    }

    /// Takes the note away as soon as the prompt it describes has been answered.
    ///
    /// The answer is given in the terminal — confirmed or cancelled — and the terminal is
    /// what holds the command, so the note leaves the corner with the prompt. What is left
    /// on screen is then the plain fact: a confirmed command is in the pane, and a
    /// cancelled one was never written.
    pub(super) fn settle_snippet_note_if_answered(&mut self, cx: &mut Context<Self>) {
        let Some(note) = self.snippet_note else {
            return;
        };
        let waiting = self
            .pane_for(note.session)
            .is_some_and(|pane| pane.read(cx).awaits_paste_confirmation());
        if !waiting {
            self.settle_snippet_note(cx);
        }
    }

    /// The keys the manager reads before its fields do.
    ///
    /// The list is walkable: Up and Down move the highlighted command and Enter inserts it
    /// (the safe of its two actions), so a command can be used without a pointer. Escape is
    /// the window's own "close what floats" action, which takes one layer at a time — the
    /// editor, then the deletion question, then the dialog — so a single press never throws
    /// a draft away.
    fn snippet_panel_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        // While the editor is open the keys belong to its fields.
        if self.snippet_panel.is_editing() {
            return;
        }
        let len = self.snippets.snippets().len();
        match key {
            "down" => {
                cx.stop_propagation();
                self.snippet_panel.step(1, len);
                cx.notify();
            }
            "up" => {
                cx.stop_propagation();
                self.snippet_panel.step(-1, len);
                cx.notify();
            }
            "enter" if self.snippet_panel.pending_delete().is_none() => {
                cx.stop_propagation();
                if let Some(snippet) = self.snippets.snippets().get(self.snippet_panel.selected()) {
                    let id = snippet.id();
                    self.use_snippet(id, false, cx);
                }
            }
            _ => {}
        }
    }

    /// The manager: the list of saved commands, the editor and the deletion question.
    fn snippet_tool(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
        close: impl Fn(&mut Self, &mut Context<Self>) + Clone + 'static,
    ) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let count = self.snippets.snippets().len();
        let subtitle = if count == 0 {
            ux.snippet_manage_hint.to_owned()
        } else {
            format!(
                "{} · {}",
                copy::fill(ux.snippet_count, &[("count", &count.to_string())]),
                ux.snippet_manage_hint
            )
        };
        let close_header = close.clone();
        let (header, hint) = self.modal_header(
            Some(Icon::Terminal),
            ux.snippet_manage_title.to_owned(),
            Some(subtitle),
            "snippets.dialog.close",
            cx,
            move |this, cx| close_header(this, cx),
        );

        let mut body = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::SNUG))
            .track_focus(&self.snippet_focus)
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                this.snippet_panel_key(event, window, cx);
            }));

        // A file that could not be read is named here, and no save is offered over it.
        if let Some(problem) = self.snippets.read_problem() {
            let path = self.snippets.store().path().display().to_string();
            let reason = copy::fill(ux.snippet_unreadable_body, &[("path", &path)]);
            body = body
                .child(self.notice_line(ux.snippet_unreadable_title.to_owned(), Tone::Danger))
                .child(
                    components::banner(Tone::Danger, metrics)
                        .debug_selector(|| "snippets.unreadable".to_owned())
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .child(SharedString::from(reason)),
                )
                .child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .child(SharedString::from(problem.to_owned())),
                );
        }
        if let Some(problem) = self.snippets.write_problem() {
            body = body.child(self.notice_line(
                copy::fill(ux.snippet_save_failed_detail, &[("reason", problem)]),
                Tone::Warning,
            ));
        }

        if let Some(editor) = self.snippet_panel.editor() {
            body = body.child(self.snippet_editor_form(editor, cx));
        } else {
            // An empty library says what to do next; a library that could not be read does
            // not, because its file is the thing to deal with and the banner above says so.
            if count == 0 && self.snippets.can_write() {
                body = body.child(
                    components::state_card_with_icon(
                        Tone::Neutral,
                        Some(Icon::Terminal),
                        ux.snippet_empty_title,
                        ux.snippet_empty_body,
                        None,
                        metrics,
                    )
                    .debug_selector(|| "snippets.empty".to_owned()),
                );
            }
            let mut list = div()
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::HAIRLINE))
                .debug_selector(|| "snippets.list".to_owned());
            for index in 0..count {
                list = list.child(self.snippet_row(index, cx));
            }
            body = body.child(list);
            if let Some(question) = self.snippet_delete_question(cx) {
                body = body.child(question);
            }
        }

        // Where the commands live, and what must not be kept in one, in the words the
        // editor does not hide behind a link.
        body = body.child(
            div()
                .debug_selector(|| "snippets.storage".to_owned())
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .child(ux.snippet_storage_note),
        );

        let new_button = self
            .text_button(
                SharedString::from("snippets.new"),
                ux.snippet_new,
                Variant::Primary,
                cx,
                |this, cx| {
                    this.snippet_panel.keep();
                    this.open_snippet_editor(None, cx);
                },
            )
            .when(!self.snippets.can_write(), |button| button.opacity(0.5));
        let close_footer = close.clone();
        let footer = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_wrap()
            .gap(metrics.scale(spacing::TIGHT))
            .px(metrics.scale(spacing::LOOSE))
            .py(metrics.scale(spacing::SNUG))
            .border_t_1()
            .border_color(theme::swatch(palette::BORDER))
            .child(new_button)
            .child(self.text_button(
                SharedString::from("snippets.dialog.footer.close"),
                ux.tool_close,
                Variant::Ghost,
                cx,
                move |this, cx| close_footer(this, cx),
            ));

        self.modal(
            window,
            "snippets.dialog",
            640.0,
            ModalContent {
                header,
                subtitle: hint,
                body: body.into_any_element(),
                footer: Some(footer),
            },
        )
    }

    /// One saved command: what it is, what it runs and what can be done with it.
    fn snippet_row(&self, index: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let ux = copy::ux();
        let Some(snippet) = self.snippets.snippets().get(index) else {
            return div().id(SharedString::from(format!("snippet.row.{index}")));
        };
        let id = snippet.id();
        let lines = snippet.line_count();
        let selected = self.snippet_panel.selected() == index;
        let hover = theme::swatch(palette::SURFACE_HOVER);
        // The name and the body are one line each, ellipsized when the row is narrow: a
        // column that is allowed to wrap instead is squeezed to a few pixels by the four
        // actions beside it, and at 200% text in the minimum window that turned each
        // command's preview into a ribbon of single characters thousands of pixels tall
        // (R4). The share the row keeps for them is a floor, so the actions wrap under
        // them as a group rather than leaving no room for what the row is about.
        let mut detail = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(metrics.scale(160.0))
            .gap(px(1.0))
            .child(
                div()
                    .truncate()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(snippet.name().to_owned())),
            )
            .child(
                div()
                    .debug_selector({
                        let selector = format!("snippet.preview.{index}");
                        move || selector.clone()
                    })
                    .truncate()
                    .font_family(crate::app::terminal::monospace_family())
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(SharedString::from(body_preview(snippet.body(), 96))),
            );
        if lines > 1 {
            detail = detail.child(
                components::chip(
                    copy::fill(ux.snippet_line_count, &[("count", &lines.to_string())]),
                    Tone::Neutral,
                    metrics,
                )
                .debug_selector({
                    let selector = format!("snippet.lines.{index}");
                    move || selector.clone()
                }),
            );
        }
        div()
            .id(SharedString::from(format!("snippet.row.{index}")))
            .debug_selector({
                let selector = format!("snippet.row.{index}");
                move || selector.clone()
            })
            .flex()
            .items_center()
            .flex_wrap()
            .gap(metrics.scale(spacing::TIGHT))
            .px(metrics.scale(spacing::TIGHT))
            .py(metrics.scale(spacing::HAIRLINE + 2.0))
            .rounded(px(radius::SMALL))
            .when(selected, |row| {
                row.bg(theme::swatch(palette::SURFACE_SELECTED))
            })
            .hover(move |style| style.bg(hover))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                let len = this.snippets.snippets().len();
                this.snippet_panel.select(index, len);
                cx.notify();
            }))
            .child(detail)
            // What can be done with the command stays together: at 200% text the four
            // labels are wider than the row, so they take a line of their own below the
            // name and body instead of splitting the name into a column of characters.
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    // Filling in is the ordinary action and carries the row's weight;
                    // running submits the command, so it stays the quieter one and says
                    // what it will do before it is pressed (R17).
                    .child(self.snippet_row_button(
                        format!("snippet.insert.{index}"),
                        ux.snippet_insert,
                        ux.snippet_fill_hint,
                        id,
                        false,
                        cx,
                    ))
                    .child(self.snippet_row_button(
                        format!("snippet.run.{index}"),
                        ux.snippet_run,
                        ux.snippet_run_hint,
                        id,
                        true,
                        cx,
                    ))
                    .child(self.text_button(
                        SharedString::from(format!("snippet.edit.{index}")),
                        ux.snippet_edit,
                        Variant::Ghost,
                        cx,
                        move |this, cx| this.open_snippet_editor(Some(id), cx),
                    ))
                    .child(self.text_button(
                        SharedString::from(format!("snippet.delete.{index}")),
                        ux.snippet_delete,
                        Variant::Ghost,
                        cx,
                        move |this, cx| {
                            this.snippet_panel.ask_delete(id);
                            let len = this.snippets.snippets().len();
                            this.snippet_panel.select(index, len);
                            cx.notify();
                        },
                    )),
            )
    }

    /// The fill-in or run button of one row.
    ///
    /// Filling in writes the command and runs nothing, so it is the row's ordinary
    /// action; running also submits it, so it is drawn quieter and explains itself in a
    /// tooltip. The two are never the same control in two moods: a person choosing one
    /// can tell from the words what will happen to the terminal (R17).
    fn snippet_row_button(
        &self,
        selector: String,
        label: &'static str,
        hint: &'static str,
        id: SnippetId,
        execute: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        self.text_button(
            SharedString::from(selector),
            label,
            if execute {
                Variant::Ghost
            } else {
                Variant::Secondary
            },
            cx,
            move |this, cx| this.use_snippet(id, execute, cx),
        )
        .tooltip(components::tooltip(hint))
    }

    /// The editor of one command: its name, its body and what to do with a refused draft.
    fn snippet_editor_form(&self, editor: &SnippetEditor, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let form = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "snippet.editor".to_owned())
            .child(
                components::section_label(
                    if editor.is_new() {
                        ux.snippet_new
                    } else {
                        ux.snippet_edit_title
                    },
                    metrics,
                )
                .debug_selector(|| "snippet.editor.title".to_owned()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(ux.snippet_field_name),
                    )
                    .child(
                        div()
                            .h(metrics.control_height())
                            .debug_selector(|| "snippet.editor.name".to_owned())
                            .child(editor.name.clone()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(ux.snippet_field_body),
                    )
                    .child(
                        div()
                            .h(metrics.control_height())
                            .debug_selector(|| "snippet.editor.body".to_owned())
                            .child(editor.body.clone()),
                    ),
            )
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(ux.snippet_editor_keys),
            )
            // A draft that is already too long says so while it is being written: an empty
            // name or body is not reported before anything is typed, because a new command
            // starts empty on purpose.
            .children(
                crate::app::snippets::draft_problem(
                    editor.name.read(cx).text(),
                    editor.body.read(cx).text(),
                )
                .filter(|problem| {
                    !matches!(
                        problem,
                        shellune_core::SnippetProblem::EmptyName
                            | shellune_core::SnippetProblem::EmptyBody
                    )
                })
                .map(|problem| {
                    self.notice_line(
                        copy::snippet_problem_text(&problem, self.workspace.locale()),
                        Tone::Warning,
                    )
                    .debug_selector(|| "snippet.editor.limit".to_owned())
                }),
            )
            .children(
                editor
                    .problem
                    .as_ref()
                    .map(|problem| self.notice_line(problem.clone(), Tone::Danger)),
            );
        // A library whose file could not be read offers no save: the change would have
        // nowhere honest to go, and the banner above says which file to repair.
        let writable = self.snippets.can_write();
        let save = self
            .text_button(
                SharedString::from("snippet.editor.save"),
                ux.snippet_save,
                Variant::Primary,
                cx,
                |this, cx| this.save_snippet_editor(cx),
            )
            .when(!writable, |button| button.opacity(0.5));
        let cancel = self.text_button(
            SharedString::from("snippet.editor.cancel"),
            ux.fwd_cancel,
            Variant::Ghost,
            cx,
            |this, cx| this.close_snippet_editor(cx),
        );
        form.child(
            div()
                .flex()
                .items_center()
                .gap(metrics.scale(spacing::TIGHT))
                .child(save)
                .child(cancel),
        )
    }

    /// The question that stands between a command and its deletion.
    fn snippet_delete_question(&self, cx: &mut Context<Self>) -> Option<Div> {
        let id = self.snippet_panel.pending_delete()?;
        let snippet = self.snippets.get(id)?;
        let metrics = self.metrics();
        let ux = copy::ux();
        let name = snippet.name().to_owned();
        Some(
            div()
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::TIGHT))
                .p(metrics.scale(spacing::SNUG))
                .rounded(px(radius::SMALL))
                .border_1()
                .border_color(theme::swatch(palette::DANGER))
                .bg(theme::swatch(palette::DANGER_TINT))
                .debug_selector(|| "snippet.delete.question".to_owned())
                .child(
                    div()
                        .text_size(metrics.label_size())
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .child(ux.snippet_delete_title),
                )
                .child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .child(SharedString::from(copy::fill(
                            ux.snippet_delete_body,
                            &[("name", &name)],
                        ))),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(metrics.scale(spacing::TIGHT))
                        .child(self.text_button(
                            SharedString::from("snippet.delete.yes"),
                            ux.snippet_delete_yes,
                            Variant::Danger,
                            cx,
                            |this, cx| this.confirm_snippet_delete(cx),
                        ))
                        .child(self.text_button(
                            SharedString::from("snippet.delete.no"),
                            ux.snippet_delete_no,
                            Variant::Ghost,
                            cx,
                            |this, cx| {
                                this.snippet_panel.keep();
                                cx.notify();
                            },
                        )),
                ),
        )
    }
}
