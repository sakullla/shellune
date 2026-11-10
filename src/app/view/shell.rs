//! The window's frame: one top bar that is the title bar and the tab strip, the
//! hosts home (local shells and saved servers in one place), the settings page,
//! and the dialogs that replaced inline forms and side pages: connecting to a host,
//! quick connect, port forwarding and transfers.
//!
//! The sidebar is gone. Everything a user switches between is a tab in the top bar:
//! the hosts home first, then one tab per open terminal. Settings sit at the right
//! end with search, next to the window buttons.

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Div, FontWeight, MouseButton, SharedString, Stateful, Window, div, px,
};

use shellune_core::SnippetId;

use super::{ELLIPSIS, ShelluneView, SnippetNote, brand_mark, host_avatar, program_label};
use crate::app::components::{self, Variant};
use crate::app::copy::{self, Locale};
use crate::app::icons::{self, Icon};
use crate::app::nav::{Destination, Platform};
use crate::app::snippets::{SnippetEditor, body_preview};
use crate::app::terminal::SnippetOffer;
use crate::app::text_field::TextField;
use crate::app::theme::{self, Motion, ThemeChoice, Tone, Zoom, palette, radius, spacing};
use crate::app::workspace::{AttentionKind, SectionState};

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

    /// The top row: brand, the hosts tab, the terminal tabs, a new-tab control, the
    /// blank caption that drags the window, search, settings and (on Windows) the
    /// window buttons.
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
            .bg(theme::swatch(palette::SURFACE))
            .border_b_1()
            .border_color(theme::swatch(palette::BORDER))
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
            tab.bg(theme::swatch(palette::CANVAS))
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
            .items_end()
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
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .debug_selector(|| "header.title".to_owned())
                            .child(ux.home_title),
                    )
                    .child(
                        div()
                            .text_size(metrics.body_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(ux.home_subtitle),
                    ),
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

        let mut column = div()
            .flex()
            .flex_col()
            .w_full()
            .max_w(metrics.scale(960.0))
            .mx_auto()
            .px(metrics.scale(spacing::LOOSE + 8.0))
            .py(metrics.scale(spacing::LOOSE + 4.0))
            .gap(metrics.scale(spacing::LOOSE))
            .debug_selector(|| "home".to_owned())
            .child(heading)
            .children(
                self.local_error
                    .as_ref()
                    .map(|reason| self.notice_line(reason.clone(), Tone::Danger)),
            )
            .child(self.home_local_section(cx));

        let hosts = match state {
            SectionState::Loading | SectionState::Failed => self.hosts_destination(cx),
            SectionState::Empty | SectionState::Populated => {
                if self.book.hosts().is_empty()
                    && self.hosts.search().trim().is_empty()
                    && !self.import.is_open()
                {
                    self.home_welcome(cx)
                } else {
                    self.hosts_destination(cx)
                }
            }
        };
        column = column.child(
            div()
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::SNUG))
                .child(components::section_label(ux.home_hosts_section, metrics))
                .child(hosts),
        );

        // The page holds the keyboard of whichever destination shows it, so the
        // shortcuts keep working after a click on the page or a page switch.
        div()
            .id("content.scroll")
            .debug_selector(|| "content".to_owned())
            .track_focus(self.destination_focus(self.workspace.destination()))
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .bg(theme::swatch(palette::CANVAS))
            .child(column)
    }

    /// The shells of this computer, as compact cards that open in one click.
    fn home_local_section(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let mut tiles = div()
            .flex()
            .flex_wrap()
            .w_full()
            .gap(metrics.scale(spacing::SNUG))
            .debug_selector(|| "shell.picker".to_owned());
        for (index, shell) in self.workspace.shells().iter().enumerate() {
            let selected = index == self.workspace.shell_choice();
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
        let mut section = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .child(components::section_label(ux.home_local_section, metrics));
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
        let hover_border = theme::swatch(palette::ACCENT);
        let hover_bg = theme::swatch(palette::SURFACE_RAISED);
        div()
            .group("home-card")
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::SNUG))
            .w(metrics.scale(210.0))
            .p(metrics.scale(spacing::SNUG))
            .rounded(px(radius::LARGE))
            .bg(theme::swatch(palette::SURFACE))
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
                    .size(metrics.scale(36.0))
                    .rounded(px(radius::MEDIUM))
                    .bg(theme::swatch(palette::ACCENT_TINT))
                    .child(icons::icon(
                        icon,
                        metrics.icon_size(),
                        theme::swatch(palette::ACCENT),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .gap(metrics.scale(2.0))
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
                            .children(
                                badge.map(|badge| components::chip(badge, Tone::Accent, metrics)),
                            ),
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
                .bg(theme::swatch(palette::SURFACE))
                .border_1()
                .border_color(theme::swatch(palette::BORDER))
                .debug_selector(|| "hosts.empty".to_owned())
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(metrics.scale(56.0))
                        .rounded(px(radius::XLARGE))
                        .bg(theme::swatch(palette::ACCENT_TINT))
                        .child(icons::icon(
                            Icon::Server,
                            metrics.scale(26.0),
                            theme::swatch(palette::ACCENT),
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

    // -----------------------------------------------------------------------
    // Settings
    // -----------------------------------------------------------------------

    /// Appearance, language, terminal and data, as labelled rows with the control
    /// on the right.
    pub(super) fn settings_page(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();

        let theme_choice = self.theme_choice;
        let theme_control = self.segments(
            "settings.theme",
            ThemeChoice::ALL
                .iter()
                .map(|choice| {
                    let label = match choice {
                        ThemeChoice::System => strings.theme_system,
                        ThemeChoice::Light => strings.theme_light,
                        ThemeChoice::Dark => strings.theme_dark,
                    };
                    (label.to_owned(), *choice == theme_choice)
                })
                .collect(),
            cx,
            |view, index, cx| view.set_theme(ThemeChoice::ALL[index], cx),
        );
        let scheme = self.terminal_scheme();
        let scheme_control = self.segments(
            "settings.scheme",
            crate::app::theme::TerminalScheme::ALL
                .iter()
                .map(|choice| {
                    let label = match choice {
                        crate::app::theme::TerminalScheme::FollowApp => ux.settings_scheme_app,
                        crate::app::theme::TerminalScheme::Dark => strings.theme_dark,
                        crate::app::theme::TerminalScheme::Light => strings.theme_light,
                    };
                    (label.to_owned(), *choice == scheme)
                })
                .collect(),
            cx,
            |view, index, cx| {
                view.set_terminal_scheme(crate::app::theme::TerminalScheme::ALL[index], cx);
            },
        );
        let locale = self.workspace.locale();
        let language_control = self.segments(
            "settings.language",
            Locale::ALL
                .iter()
                .map(|candidate| (candidate.endonym().to_owned(), *candidate == locale))
                .collect(),
            cx,
            |view, index, cx| {
                if view.workspace.locale() != Locale::ALL[index] {
                    view.toggle_locale(cx);
                }
            },
        );
        let zoom = self.workspace.zoom();
        let zoom_control = self.segments(
            "settings.zoom",
            Zoom::ALL
                .iter()
                .map(|candidate| (candidate.label().to_owned(), *candidate == zoom))
                .collect(),
            cx,
            |view, index, cx| {
                view.workspace.set_zoom(Zoom::ALL[index]);
                cx.notify();
            },
        );
        let motion = self.workspace.motion();
        let motion_control = self.segments(
            "settings.motion",
            [Motion::Full, Motion::Reduced]
                .iter()
                .map(|candidate| {
                    (
                        if candidate.allows_animation() {
                            strings.motion_full
                        } else {
                            strings.motion_reduced
                        }
                        .to_owned(),
                        *candidate == motion,
                    )
                })
                .collect(),
            cx,
            |view, index, cx| {
                let wanted = if index == 0 {
                    Motion::Full
                } else {
                    Motion::Reduced
                };
                view.workspace.set_motion(wanted);
                cx.notify();
            },
        );
        // The two background reminders are switched one by one (R7): knowing that a long
        // command in another tab ended is not the same wish as hearing about every bell.
        // Both only decide whether a tab is marked, so turning one off leaves the bell
        // byte, the command blocks and every disconnect reason exactly as they were.
        let notifications = self.workspace.attention_settings();
        let notices_label = |on: bool| if on { ux.notice_on } else { ux.notice_off }.to_owned();
        let bell_control = self.segments(
            "settings.notice.bell",
            vec![
                (notices_label(notifications.bell), notifications.bell),
                (notices_label(!notifications.bell), !notifications.bell),
            ],
            cx,
            |view, index, cx| {
                view.set_attention_notice(AttentionKind::Bell, index == 0, cx);
            },
        );
        let finished_control = self.segments(
            "settings.notice.finished",
            vec![
                (
                    notices_label(notifications.finished),
                    notifications.finished,
                ),
                (
                    notices_label(!notifications.finished),
                    !notifications.finished,
                ),
            ],
            cx,
            |view, index, cx| {
                view.set_attention_notice(AttentionKind::CommandFinished, index == 0, cx);
            },
        );
        let font = self.workspace.local_font_size();
        let font_control = div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::HAIRLINE))
            .child(
                components::icon_button("A−", metrics)
                    .id("settings.font.less")
                    .debug_selector(|| "settings.font.less".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.set_terminal_font_size(this.workspace.local_font_size() - 1.0, cx);
                    })),
            )
            .child(
                div()
                    .min_w(metrics.scale(44.0))
                    .text_center()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "settings.font.value".to_owned())
                    .child(SharedString::from(format!("{font:.0} px"))),
            )
            .child(
                components::icon_button("A+", metrics)
                    .id("settings.font.more")
                    .debug_selector(|| "settings.font.more".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.set_terminal_font_size(this.workspace.local_font_size() + 1.0, cx);
                    })),
            );
        let choice = self.workspace.shell_choice();
        let shell_control = self.segments(
            "settings.shell",
            self.workspace
                .shells()
                .iter()
                .enumerate()
                .map(|(index, shell)| (shell.name().to_owned(), index == choice))
                .collect(),
            cx,
            |view, index, cx| {
                view.workspace.select_shell(index);
                cx.notify();
            },
        );

        let page = div()
            .flex()
            .flex_col()
            .w_full()
            .max_w(metrics.scale(760.0))
            .mx_auto()
            .px(metrics.scale(spacing::LOOSE + 8.0))
            .py(metrics.scale(spacing::LOOSE + 4.0))
            .gap(metrics.scale(spacing::LOOSE))
            .debug_selector(|| "settings".to_owned())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(spacing::HAIRLINE))
                    .child(
                        div()
                            .text_size(metrics.hero_size())
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .debug_selector(|| "header.title".to_owned())
                            .child(ux.settings_title),
                    )
                    .child(
                        div()
                            .text_size(metrics.body_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(ux.settings_summary),
                    ),
            )
            .child(self.settings_group(
                "appearance",
                ux.settings_appearance,
                vec![
                    self.settings_row(
                        "theme",
                        ux.settings_theme,
                        ux.settings_theme_hint,
                        theme_control,
                    ),
                    self.settings_row(
                        "language",
                        ux.settings_language,
                        ux.settings_language_hint,
                        language_control,
                    ),
                    self.settings_row(
                        "text-size",
                        ux.settings_text_size,
                        ux.settings_text_size_hint,
                        zoom_control,
                    ),
                    self.settings_row(
                        "motion",
                        ux.settings_motion,
                        ux.settings_motion_hint,
                        motion_control,
                    ),
                ],
            ))
            .child(self.settings_group(
                "terminal",
                ux.settings_terminal,
                vec![
                    self.settings_row(
                        "scheme",
                        ux.settings_scheme,
                        ux.settings_scheme_hint,
                        scheme_control,
                    ),
                    self.settings_row(
                        "font-size",
                        ux.settings_font_size,
                        ux.settings_font_size_hint,
                        font_control,
                    ),
                    self.settings_row(
                        "default-shell",
                        ux.settings_default_shell,
                        ux.settings_default_shell_hint,
                        shell_control,
                    ),
                ],
            ))
            .child(self.settings_group(
                "notices",
                ux.settings_notices,
                vec![
                    self.settings_row(
                        "bell-notice",
                        ux.settings_bell_notice,
                        ux.settings_bell_notice_hint,
                        bell_control,
                    ),
                    self.settings_row(
                        "finished-notice",
                        ux.settings_finished_notice,
                        ux.settings_finished_notice_hint,
                        finished_control,
                    ),
                ],
            ))
            .child(self.settings_keys_group(cx))
            .child(self.settings_group(
                "data",
                ux.settings_data,
                vec![
                    self.settings_row(
                        "hosts-file",
                        ux.settings_hosts_file,
                        "",
                        div()
                            .max_w(metrics.scale(360.0))
                            .truncate()
                            .font_family(crate::app::terminal::monospace_family())
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .debug_selector(|| "settings.paths".to_owned())
                            .child(SharedString::from(super::home_relative(
                                &self.store.path().display().to_string(),
                            ))),
                    ),
                    self.settings_row(
                        "import",
                        strings.import_open,
                        ux.settings_import_hint,
                        components::icon_label_button(
                            Icon::Import,
                            strings.import_open,
                            Variant::Secondary,
                            metrics,
                        )
                        .id("settings.import")
                        .debug_selector(|| "settings.import".to_owned())
                        .on_click(cx.listener(|this, _event, window, cx| {
                            this.select(Destination::Hosts, window, cx);
                            this.open_import(cx);
                        })),
                    ),
                ],
            ))
            .child(self.settings_group(
                "about",
                ux.settings_about,
                vec![self.settings_row(
                    "about",
                    crate::app::product::NAME,
                    match self.workspace.locale() {
                        Locale::English => crate::app::product::TAGLINE_EN,
                        Locale::Chinese => crate::app::product::TAGLINE_ZH,
                    },
                    div()
                        .text_size(metrics.label_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .debug_selector(|| "status.version".to_owned())
                        .child(SharedString::from(format!(
                            "{} {}",
                            ux.settings_version,
                            self.version()
                        ))),
                )],
            ));

        div()
            .id("content.scroll")
            .debug_selector(|| "content".to_owned())
            .track_focus(self.destination_focus(Destination::Settings))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                this.settings_page_key(event, window, cx);
            }))
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .bg(theme::swatch(palette::CANVAS))
            .child(page)
    }

    // -----------------------------------------------------------------------
    // Keyboard shortcuts (R5)
    // -----------------------------------------------------------------------

    /// Keys the settings page reads before anything else does.
    ///
    /// The arrow keys choose a shortcut row and then one of its controls, and Enter
    /// runs the control the cursor is on, so a binding can be changed, unbound or
    /// restored with the keyboard alone (R4, R5). Modifier combinations are left to
    /// the action catalogue.
    pub(super) fn settings_page_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.key_capture.is_some() {
            return;
        }
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform {
            return;
        }
        match event.keystroke.key.as_str() {
            "down" => self.step_key_row(1, cx),
            "up" => self.step_key_row(-1, cx),
            "right" => self.step_key_control(1, cx),
            "left" => self.step_key_control(-1, cx),
            "enter" => self.run_key_row_control(cx),
            _ => return,
        }
        // Stepping into the list takes the keyboard off the search field, so the
        // following ←/→ choose a control instead of moving a caret.
        window.focus(self.destination_focus(Destination::Settings));
        cx.stop_propagation();
    }

    /// The shortcut group: a search over the action catalogue and one row per action
    /// with the keys in force, the way to change them, to unbind them and to restore
    /// the default.
    pub(super) fn settings_keys_group(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let locale = self.workspace.locale();
        let platform = self.keymap.platform();
        let query = self.keys_search.read(cx).text().to_owned();
        let matches = self.keymap.matching(&query, locale);

        let mut rows: Vec<Div> = Vec::new();
        if let Some(problem) = &self.keymap_problem {
            rows.push(self.settings_keys_banner(
                Tone::Warning,
                problem.text(locale, platform),
                ux.keys_problem_hint,
            ));
        }
        if let Some(reason) = &self.keymap_save_error {
            rows.push(self.settings_keys_banner(
                Tone::Danger,
                format!("{} {reason}", ux.keys_save_failed),
                ux.keys_save_failed_next,
            ));
        }
        rows.push(
            div()
                .px(metrics.scale(spacing::BASE))
                .py(metrics.scale(spacing::TIGHT))
                .debug_selector(|| "settings.keys.search".to_owned())
                .child(self.keys_search.clone()),
        );
        for (index, spec) in matches.iter().enumerate() {
            let selected = self.keys_selected.min(matches.len().saturating_sub(1));
            rows.push(self.settings_keys_row(spec, index, selected, cx));
        }
        if matches.is_empty() {
            rows.push(
                div()
                    .px(metrics.scale(spacing::BASE))
                    .py(metrics.scale(spacing::SNUG))
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "settings.keys.empty".to_owned())
                    .child(ux.keys_no_match),
            );
        }
        rows.push(self.settings_keys_footer(cx));

        div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "settings.keys".to_owned())
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(ux.settings_keys_hint),
            )
            .child(self.settings_group("keys", ux.settings_keys, rows))
    }

    /// A tone-carrying line inside the shortcut card: why the saved keys were not
    /// applied, or why a change could not be written.
    fn settings_keys_banner(&self, tone: Tone, reason: String, next: &'static str) -> Div {
        let metrics = self.metrics();
        div()
            .px(metrics.scale(spacing::TIGHT))
            .py(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "settings.keys.banner".to_owned())
            .child(
                components::banner(tone, metrics)
                    .child(SharedString::from(reason))
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(next),
                    ),
            )
    }

    /// One action: its name, where the shortcut applies, the keys in force and the
    /// three ways to change them.
    ///
    /// The row the keyboard is on carries the focus ring, so the page can be worked
    /// without a pointer: the arrow keys move between rows and Enter opens the one
    /// under the ring for capture.
    fn settings_keys_row(
        &self,
        spec: &'static crate::app::keymap::ActionSpec,
        index: usize,
        selected: usize,
        cx: &mut Context<Self>,
    ) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let locale = self.workspace.locale();
        let labels = self.keymap.labels(spec.id);
        let bound = !self.keymap.is_unbound(spec.id);
        let default = self.keymap.is_default(spec.id);
        let state = if !bound {
            ux.keys_unbound
        } else if default {
            ux.keys_default
        } else {
            ux.keys_changed
        };
        let action = spec.id;
        let selector = format!("settings.keys.row.{action}");
        let ringed = !self.pointer_input && index == selected;
        // The controls this row offers, in the order the keyboard steps through them:
        // an unbound action cannot be unbound again, and one at its default has
        // nothing to restore (R5).
        let controls = super::key_row_controls(bound, default);
        let cursor = self.keys_control.min(controls.len().saturating_sub(1));
        let focus = theme::swatch(palette::ACCENT);
        let focus_fill = theme::swatch(palette::SURFACE_SELECTED);

        let keys = if bound {
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .min_w(px(0.0))
                .gap(metrics.scale(spacing::HAIRLINE))
                .children(labels.into_iter().enumerate().map(|(position, label)| {
                    let selector = format!("settings.keys.keycap.{action}.{position}");
                    components::keycap(SharedString::from(label), metrics)
                        .debug_selector(move || selector.clone())
                }))
                .into_any_element()
        } else {
            div()
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .child(ux.keys_unbound)
                .into_any_element()
        };

        div()
            .debug_selector(move || selector.clone())
            .flex()
            .items_center()
            .justify_between()
            .flex_wrap()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::BASE))
            .py(metrics.scale(spacing::TIGHT))
            .border_1()
            .border_color(if ringed {
                theme::swatch(palette::ACCENT)
            } else {
                theme::clear()
            })
            .rounded(px(radius::SMALL))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(metrics.scale(180.0))
                    .gap(metrics.scale(2.0))
                    .child(
                        div()
                            .text_size(metrics.body_size())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(spec.name(locale)),
                    )
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(format!(
                                "{} · {state}",
                                spec.context.label(locale)
                            ))),
                    ),
            )
            .child({
                // The keys and their three controls wrap as a group of their own: when
                // the name column leaves no room on the line, the group takes the next
                // line instead of pushing its controls past the card's right edge
                // (R4, 720×480 at 200% text).
                let mut group = div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .min_w(px(0.0))
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(keys);
                for (position, control) in controls.iter().copied().enumerate() {
                    let (name, label, variant) = match control {
                        super::KeyRowControl::Change => {
                            ("change", ux.keys_change, Variant::Secondary)
                        }
                        super::KeyRowControl::Unbind => ("unbind", ux.keys_unbind, Variant::Ghost),
                        super::KeyRowControl::Reset => ("reset", ux.keys_reset, Variant::Ghost),
                    };
                    // The control the keyboard's cursor is on carries the accent ring,
                    // so the step between Change, Unbind and Restore is visible (R4).
                    let focused = ringed && position == cursor;
                    let selector = format!("settings.keys.{name}.{action}");
                    let button = components::compact_button(label, variant, metrics)
                        .id(SharedString::from(selector.clone()))
                        .debug_selector(move || selector.clone())
                        .when(focused, |button| button.border_color(focus).bg(focus_fill))
                        .on_click(cx.listener(move |this, _event, _window, cx| match control {
                            super::KeyRowControl::Change => this.keys_capture_start(action, cx),
                            super::KeyRowControl::Unbind => this.keys_unbind(action, cx),
                            super::KeyRowControl::Reset => this.keys_reset(action, cx),
                        }));
                    group = group.child(button);
                }
                group
            })
    }

    /// The end of the shortcut card: how many actions differ from their defaults, and
    /// the one control that puts every one of them back.
    fn settings_keys_footer(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let count = self.keymap.custom_count();
        div()
            .flex()
            .items_center()
            .justify_between()
            .flex_wrap()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::BASE))
            .py(metrics.scale(spacing::TIGHT))
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "settings.keys.count".to_owned())
                    .child(SharedString::from(
                        ux.keys_custom_summary.replace("{}", &count.to_string()),
                    )),
            )
            .child(
                components::compact_button(ux.keys_reset_all, Variant::Secondary, metrics)
                    .id("settings.keys.reset_all")
                    .debug_selector(|| "settings.keys.reset_all".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.keys_reset_all(cx);
                    })),
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

    /// A titled group of settings rows inside one card.
    ///
    /// `id` names the card in measurements: a row, and every control in it, has to stay
    /// inside the card's own box — the card is what the window clips nothing of, so a
    /// control drawn past it is drawn past the window as well (R4, 720×480 at 200%).
    fn settings_group(&self, id: &'static str, title: &'static str, rows: Vec<Div>) -> Div {
        let metrics = self.metrics();
        let count = rows.len();
        let mut card = div()
            .flex()
            .flex_col()
            .rounded(px(radius::LARGE))
            .bg(theme::swatch(palette::SURFACE))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(move || format!("settings.card.{id}"));
        for (index, row) in rows.into_iter().enumerate() {
            card = card.child(row.when(index + 1 < count, |row| {
                row.border_b_1()
                    .border_color(theme::swatch(palette::BORDER))
            }));
        }
        div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .child(components::section_label(title, metrics))
            .child(card)
    }

    /// One settings row: the label and its hint on the left, the control on the right.
    ///
    /// The row wraps, and the control's column may shrink below the control's own
    /// width, so a control that does not fit beside its label moves under the label and
    /// a control wider than the row wraps inside itself. Nothing is drawn past the card
    /// it sits in — and the card is inside the window — because the settings page scrolls
    /// only downwards, so a control past the card's right edge has no scroll that could
    /// bring it back (R4, 720×480 at 200% text).
    fn settings_row(
        &self,
        id: &'static str,
        label: &'static str,
        hint: &'static str,
        control: impl IntoElement,
    ) -> Div {
        let metrics = self.metrics();
        div()
            .flex()
            .items_center()
            .justify_between()
            .flex_wrap()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::BASE))
            .py(metrics.scale(spacing::SNUG))
            .debug_selector(move || format!("settings.row.{id}"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(metrics.scale(200.0))
                    .gap(metrics.scale(2.0))
                    .child(
                        div()
                            .text_size(metrics.body_size())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(label),
                    )
                    .when(!hint.is_empty(), |column| {
                        column.child(
                            div()
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED))
                                .child(hint),
                        )
                    }),
            )
            .child(div().flex().flex_col().min_w(px(0.0)).child(control))
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
        let mut control = components::segmented(metrics).debug_selector(move || id.to_owned());
        for (index, (label, selected)) in options.into_iter().enumerate() {
            let pick = pick.clone();
            control = control.child(
                components::segment(label, selected, metrics)
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
        header: Div,
        body: AnyElement,
        footer: Option<Div>,
    ) -> Div {
        let metrics = self.metrics();
        let viewport = window.viewport_size();
        let width = metrics
            .scale(width)
            .min(viewport.width - metrics.scale(spacing::LOOSE) * 2.0);
        let max_height = viewport.height - metrics.scale(spacing::LOOSE) * 2.0;
        let dialog = div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .w(width)
            .max_h(max_height)
            .flex()
            .flex_col()
            .rounded(px(radius::XLARGE))
            .bg(theme::swatch(palette::SURFACE))
            .border_1()
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .shadow_lg()
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
                    .child(body),
            )
            .children(footer);
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
                        .bg(gpui::rgba(0x0000_0080))
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
    ) -> Div {
        self.modal_header_with(icon, title, subtitle, None, close_id, cx, close)
    }

    /// [`Self::modal_header`] with a badge before the close button.
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
    ) -> Div {
        let metrics = self.metrics();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pt(metrics.scale(spacing::LOOSE - 4.0))
            .pb(metrics.scale(spacing::SNUG))
            .children(icon.map(|icon| {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(metrics.scale(36.0))
                    .rounded(px(radius::MEDIUM + 2.0))
                    .bg(theme::swatch(palette::ACCENT_TINT))
                    .child(icons::icon(
                        icon,
                        metrics.icon_size(),
                        theme::swatch(palette::ACCENT),
                    ))
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .gap(metrics.scale(2.0))
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics.heading_size())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(SharedString::from(title)),
                    )
                    .children(subtitle.map(|subtitle| {
                        div()
                            // One line, like the title above it: a subtitle that wraps at
                            // 200% text in the smallest window grows the header past the
                            // dialog's own height and leaves the body — the controls the
                            // header introduces — without a line to stand on (R4).
                            .truncate()
                            .text_size(metrics.label_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(SharedString::from(subtitle))
                    })),
            )
            .children(badge)
            .child(
                components::icon_only_button(Icon::Close, metrics)
                    .id(close_id)
                    .debug_selector(move || close_id.to_owned())
                    .tooltip(components::tooltip(copy::ux().tool_close))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        close(this, cx);
                    })),
            )
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
            let header = self.modal_header(
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
            return Some(self.modal(window, "quick.dialog", 520.0, header, body, None));
        }
        if !(self.showing_connection_form && self.remote.is_some()) {
            return None;
        }
        let panel = self.remote.as_ref()?;
        let host = panel.host();
        let (status_label, tone) = super::remote_status_label(panel.status(), strings);
        // The hint is about choosing a sign-in; while the server asks a question or
        // the connection runs, it would only be in the way.
        let choosing = matches!(
            panel.status(),
            crate::app::remote::RemoteStatus::Idle | crate::app::remote::RemoteStatus::Failed(_)
        );
        let header = self.modal_header_with(
            None,
            format!("{} {}", ux.connect_title, host.name()),
            Some(host.target()),
            Some(components::chip(status_label, tone, metrics)),
            "remote.close",
            cx,
            |this, cx| this.close_remote(cx),
        );
        let header = div()
            .flex()
            .items_center()
            .pl(metrics.scale(spacing::LOOSE))
            .child(host_avatar(host.name(), false, metrics))
            .child(header.flex_1().pl(metrics.scale(spacing::SNUG)));
        let body = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::LOOSE))
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
        // its tab, and clicking that tab brings the dialog back.
        let footer = self.form_session.is_some().then(|| {
            div()
                .flex()
                .justify_end()
                .px(metrics.scale(spacing::LOOSE))
                .py(metrics.scale(spacing::SNUG))
                .border_t_1()
                .border_color(theme::swatch(palette::BORDER))
                .child(self.text_button(
                    SharedString::from("remote.hide"),
                    ux.connect_background,
                    Variant::Ghost,
                    cx,
                    |view, cx| view.hide_connection(cx),
                ))
        });
        Some(self.modal(window, "remote.dialog", 600.0, header, body, footer))
    }

    /// Importing hosts from an SSH configuration, as a dialog: read, preview, then
    /// save only what the preview offered.
    pub(super) fn import_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        if !self.import.is_open() {
            return None;
        }
        let metrics = self.metrics();
        let strings = self.strings();
        let header = self.modal_header(
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
        Some(self.modal(window, "hosts.import.dialog", 720.0, header, body, None))
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
        let header = self.modal_header(
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
            header,
            body,
            Some(footer),
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
                let header = self.modal_header(
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
                Some(self.modal(window, "forwards.dialog", 680.0, header, body, None))
            }
            Tool::Transfers => {
                let header = self.modal_header(
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
                Some(self.modal(window, "transfers.dialog", 680.0, header, body, None))
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
        let header = self.modal_header(
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
            header,
            body.into_any_element(),
            Some(footer),
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
        let mut detail = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .gap(px(1.0))
            .child(
                div()
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
            .child(self.snippet_row_button(
                format!("snippet.insert.{index}"),
                ux.snippet_insert,
                id,
                false,
                cx,
            ))
            .child(self.snippet_row_button(
                format!("snippet.run.{index}"),
                ux.snippet_run,
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
            ))
    }

    /// The insert or run button of one row.
    fn snippet_row_button(
        &self,
        selector: String,
        label: &'static str,
        id: SnippetId,
        execute: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        self.text_button(
            SharedString::from(selector),
            label,
            if execute {
                Variant::Secondary
            } else {
                Variant::Ghost
            },
            cx,
            move |this, cx| this.use_snippet(id, execute, cx),
        )
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
