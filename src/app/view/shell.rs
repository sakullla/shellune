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
    AnyElement, Context, Div, FontWeight, MouseButton, Point, SharedString, Stateful, Window, div,
    px,
};

use super::{ELLIPSIS, ShelluneView, brand_mark, host_avatar, program_label};
use crate::app::components::{self, Variant};
use crate::app::copy::{self, Locale};
use crate::app::icons::{self, Icon};
use crate::app::nav::{Destination, Platform};
use crate::app::theme::{self, Motion, ThemeChoice, Tone, Zoom, palette, radius, spacing};
use crate::app::workspace::SectionState;

/// A tool of a host or session that opens over the page instead of being a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Saved tunnels, for every host or one.
    Forwards,
    /// Uploads and downloads of every connection.
    Transfers,
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
                    format!("{} ({})", ux.search, platform.palette_label()),
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
                    ux.settings_title.to_owned(),
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
            .child(self.new_tab_control(window, cx))
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
        let tooltip = format!(
            "{} · {}",
            strings.nav_hosts,
            Destination::Hosts.shortcut_label(Platform::current())
        );
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
    fn new_tab_control(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let platform = Platform::current();
        let open = self.shell_menu_open;
        let size = metrics.scale(28.0);
        let mut control = div()
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
                    .tooltip(components::tooltip(SharedString::from(format!(
                        "{} ({})",
                        ux.new_tab,
                        super::new_shell_shortcut_label(platform)
                    ))))
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.new_local_shell(window, cx);
                    })),
            )
            .child(
                components::icon_only_button(Icon::ChevronDown, metrics)
                    .w(metrics.scale(18.0))
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
        if open {
            control = control.child(self.shell_menu(window, cx));
        }
        control
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
                this.toggle_custom_shell(window, cx);
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
        section = section.child(tiles);
        if self.custom_shell_open {
            section = section.child(
                div()
                    .w_full()
                    .max_w(metrics.scale(520.0))
                    .debug_selector(|| "shell.path".to_owned())
                    .child(self.shell_path.clone()),
            );
        }
        section
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
                ux.settings_appearance,
                vec![
                    self.settings_row(ux.settings_theme, ux.settings_theme_hint, theme_control),
                    self.settings_row(
                        ux.settings_language,
                        ux.settings_language_hint,
                        language_control,
                    ),
                    self.settings_row(
                        ux.settings_text_size,
                        ux.settings_text_size_hint,
                        zoom_control,
                    ),
                    self.settings_row(ux.settings_motion, ux.settings_motion_hint, motion_control),
                ],
            ))
            .child(self.settings_group(
                ux.settings_terminal,
                vec![
                    self.settings_row(
                        ux.settings_font_size,
                        ux.settings_font_size_hint,
                        font_control,
                    ),
                    self.settings_row(
                        ux.settings_default_shell,
                        ux.settings_default_shell_hint,
                        shell_control,
                    ),
                ],
            ))
            .child(self.settings_group(
                ux.settings_data,
                vec![
                    self.settings_row(
                        ux.settings_hosts_file,
                        "",
                        div()
                            .max_w(metrics.scale(360.0))
                            .truncate()
                            .font_family(crate::app::terminal::monospace_family())
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(SharedString::from(super::home_relative(
                                &self.store.path().display().to_string(),
                            ))),
                    ),
                    self.settings_row(
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
                ux.settings_about,
                vec![self.settings_row(
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
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .bg(theme::swatch(palette::CANVAS))
            .child(page)
    }

    /// A titled group of settings rows inside one card.
    fn settings_group(&self, title: &'static str, rows: Vec<Div>) -> Div {
        let metrics = self.metrics();
        let count = rows.len();
        let mut card = div()
            .flex()
            .flex_col()
            .rounded(px(radius::LARGE))
            .bg(theme::swatch(palette::SURFACE))
            .border_1()
            .border_color(theme::swatch(palette::BORDER));
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
    fn settings_row(
        &self,
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
            .child(div().flex_none().child(control))
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
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .child(body),
            )
            .children(footer);
        div().child(
            gpui::deferred(
                gpui::anchored().position(Point::default()).child(
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
                ),
            )
            .with_priority(1),
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

    /// Port forwarding or transfers, opened from a host or a session.
    pub(super) fn tool_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let tool = self.tool?;
        let metrics = self.metrics();
        let ux = copy::ux();
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.tool = None;
            this.forwards.close_editor();
            this.pending_exposure = None;
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
