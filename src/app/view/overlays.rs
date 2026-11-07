//! The floating layers of the window: the command palette and the tab menu.
//!
//! Both are keyboard-and-pointer shortcuts to things the window can already do; neither
//! holds state of its own beyond what is open and what is highlighted, so every action
//! they run goes through the same methods as the buttons elsewhere.

use gpui::prelude::*;
use gpui::{
    Context, Div, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point,
    SharedString, Window, div, px,
};
use shellune_core::{HostConfig, LocalShell, SessionId, SessionKind};

use super::{PaneKind, ShelluneView, destination_icon, program_label, session_target};
use crate::app::components;
use crate::app::copy;
use crate::app::icons::{self, Icon};
use crate::app::nav::{Destination, Platform};
use crate::app::palette::{self, Command, Entry, Group};
use crate::app::text_field::TextField;
use crate::app::theme::{self, Tone, palette as colors, radius, spacing};

/// How a host is described in the palette: its target, then a jump or ProxyCommand.
fn host_palette_detail(host: &HostConfig) -> String {
    let mut detail = host.target();
    if !host.jumps().is_empty() {
        detail.push_str(&format!(" · {} jump", host.jumps().len()));
    }
    if host.proxy_command().is_some() {
        detail.push_str(" · ProxyCommand");
    }
    detail
}

/// One line of the palette list: a group heading or an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Heading(Group),
    Item(usize),
}

/// The lines the palette draws for `results`: headed by group when nothing is typed,
/// a flat ranking otherwise.
fn rows(results: &[Entry], searching: bool) -> Vec<Row> {
    let mut rows = Vec::with_capacity(results.len() + 5);
    let mut last = None;
    for (index, entry) in results.iter().enumerate() {
        if !searching && last != Some(entry.group) {
            rows.push(Row::Heading(entry.group));
            last = Some(entry.group);
        }
        rows.push(Row::Item(index));
    }
    rows
}

fn group_label(group: Group, strings: &copy::Strings) -> &'static str {
    match group {
        Group::Sessions => strings.palette_group_sessions,
        Group::Recent => strings.palette_group_recent,
        Group::Shells => strings.palette_group_shells,
        Group::Hosts => strings.palette_group_hosts,
        Group::Navigate => strings.palette_group_navigate,
        Group::Actions => strings.palette_group_actions,
    }
}

impl ShelluneView {
    // -----------------------------------------------------------------------
    // Command palette
    // -----------------------------------------------------------------------

    /// Whether the command palette is open.
    #[allow(dead_code)]
    #[must_use]
    pub fn palette_open(&self) -> bool {
        self.palette.is_some()
    }

    /// Opens the palette with an empty search, or closes it when it is open.
    pub fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            self.close_palette(window, cx);
            return;
        }
        self.shell_menu_open = false;
        self.tab_menu = None;
        // The field is made per opening so its placeholder is in the current language
        // and no earlier search is waiting in it.
        let placeholder = self.strings().palette_placeholder;
        let field = cx.new(|cx| TextField::new(placeholder, cx));
        cx.observe(&field, |view, field, cx| {
            let query = field.read(cx).text().to_owned();
            if view.palette_query != query {
                view.palette_query = query;
                view.palette_selected = 0;
                view.palette_scroll.scroll_to_item(0);
                cx.notify();
            }
        })
        .detach();
        window.focus(field.read(cx).focus_handle());
        self.palette = Some(field);
        self.palette_query.clear();
        self.palette_selected = 0;
        cx.notify();
    }

    /// Closes the palette and gives the keyboard back to what was on screen.
    pub(super) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_none() {
            return;
        }
        self.palette_query.clear();
        self.restore_focus(window, cx);
        cx.notify();
    }

    /// Focuses the terminal on screen, or the current page when there is none.
    pub(super) fn restore_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let destination = self.workspace.destination();
        if self.shows_terminal(destination)
            && let Some(pane) = self.active_pane()
        {
            let handle = pane.read(cx).focus_handle().clone();
            window.focus(&handle);
        } else {
            window.focus(&self.destination_focus(destination).clone());
        }
    }

    /// Every entry the palette can offer right now.
    fn palette_entries(&self) -> Vec<Entry> {
        let strings = self.strings();
        let platform = Platform::current();
        let locale = self.workspace.locale();
        let mut entries = Vec::new();

        for (index, id) in self.tab_sessions().into_iter().enumerate() {
            let Some(session) = self.workspace.session(id) else {
                continue;
            };
            let kind = PaneKind::of(session.kind());
            let kind_label = match kind {
                PaneKind::Local => strings.session_kind_local,
                PaneKind::Remote => strings.session_kind_remote,
            };
            let target = match session.kind() {
                SessionKind::Local { .. } => program_label(&session_target(&self.book, session)),
                SessionKind::Remote { .. } => session_target(&self.book, session),
            };
            entries.push(Entry {
                command: Command::Switch(id),
                group: Group::Sessions,
                title: self.tab_title(id, index),
                detail: format!(
                    "{kind_label} · {target} · {}",
                    copy::session_state_label(session.state(), locale)
                ),
                shortcut: (index < crate::app::TAB_JUMP_COUNT)
                    .then(|| platform.tab_jump_label(index + 1)),
                icon: match kind {
                    PaneKind::Local => Icon::Terminal,
                    PaneKind::Remote => Icon::Server,
                },
                current: self.active == Some(id),
            });
        }

        let default_shell = self.workspace.shell_choice();
        for (index, shell) in self.workspace.shells().iter().enumerate() {
            entries.push(Entry {
                command: Command::Shell(index),
                group: Group::Shells,
                title: shell.name().to_owned(),
                detail: program_label(shell.program()),
                shortcut: (index == default_shell).then(|| platform.new_shell_label().to_owned()),
                icon: Icon::Terminal,
                current: false,
            });
        }

        let mut seen = Vec::new();
        for recent in self.book.recent_hosts() {
            let host = recent.host;
            seen.push(host.id());
            entries.push(Entry {
                command: Command::Connect(host.id()),
                group: Group::Recent,
                title: host.name().to_owned(),
                detail: host_palette_detail(host),
                shortcut: None,
                icon: Icon::Plug,
                current: false,
            });
        }
        for host in self.book.hosts() {
            if seen.contains(&host.id()) {
                continue;
            }
            entries.push(Entry {
                command: Command::Connect(host.id()),
                group: Group::Hosts,
                title: host.name().to_owned(),
                detail: host_palette_detail(host),
                shortcut: None,
                icon: Icon::Plug,
                current: false,
            });
        }

        for destination in Destination::ALL {
            entries.push(Entry {
                command: Command::GoTo(destination),
                group: Group::Navigate,
                title: destination.label(strings).to_owned(),
                detail: destination.summary(strings).to_owned(),
                shortcut: Some(destination.shortcut_label(platform)),
                icon: destination_icon(destination),
                current: false,
            });
        }

        let modifier = platform.shortcut_modifier();
        let joiner = if platform == Platform::MacOs { "" } else { "+" };
        if let Some(id) = self.active {
            entries.push(Entry {
                command: Command::CloseTab(id),
                group: Group::Actions,
                title: strings.palette_close_tab.to_owned(),
                detail: self
                    .workspace
                    .session(id)
                    .map(|session| session.title().to_owned())
                    .unwrap_or_default(),
                shortcut: Some(platform.close_tab_label().to_owned()),
                icon: Icon::Close,
                current: false,
            });
        }
        entries.push(Entry {
            command: Command::QuickConnect,
            group: Group::Actions,
            title: strings.quick_connect.to_owned(),
            detail: strings.quick_connect_hint.to_owned(),
            shortcut: None,
            icon: Icon::Plug,
            current: false,
        });
        entries.push(Entry {
            command: Command::NewHost,
            group: Group::Actions,
            title: strings.primary_hosts.to_owned(),
            detail: strings.nav_hosts_summary.to_owned(),
            shortcut: None,
            icon: Icon::Plus,
            current: false,
        });
        entries.push(Entry {
            command: Command::ImportSshConfig,
            group: Group::Actions,
            title: strings.import_open.to_owned(),
            detail: String::from("~/.ssh/config"),
            shortcut: None,
            icon: Icon::Import,
            current: false,
        });
        entries.push(Entry {
            command: Command::ToggleLanguage,
            group: Group::Actions,
            title: strings.palette_language.to_owned(),
            detail: format!("{} → {}", locale.endonym(), locale.toggle().endonym()),
            shortcut: Some(format!("{modifier}{joiner}L")),
            icon: Icon::Globe,
            current: false,
        });
        entries.push(Entry {
            command: Command::CycleTheme,
            group: Group::Actions,
            title: strings.palette_theme.to_owned(),
            detail: self.theme_label().to_owned(),
            shortcut: None,
            icon: self.theme_icon(),
            current: false,
        });
        entries.push(Entry {
            command: Command::ToggleSidebar,
            group: Group::Actions,
            title: if self.rail_collapsed {
                strings.sidebar_expand
            } else {
                strings.sidebar_collapse
            }
            .to_owned(),
            detail: String::new(),
            shortcut: Some(platform.sidebar_label().to_owned()),
            icon: Icon::Sidebar,
            current: false,
        });
        entries.push(Entry {
            command: Command::CycleTextSize,
            group: Group::Actions,
            title: strings.palette_text_size.to_owned(),
            detail: self.workspace.zoom().label().to_owned(),
            shortcut: Some(format!("{modifier}{joiner}=")),
            icon: Icon::TextSize,
            current: false,
        });
        entries
    }

    /// The entries that match what is typed, best first.
    fn palette_results(&self) -> Vec<Entry> {
        palette::rank(&self.palette_entries(), &self.palette_query)
    }

    /// The palette's current search, for tests.
    #[allow(dead_code)]
    pub fn palette_titles(&self) -> Vec<String> {
        self.palette_results()
            .into_iter()
            .map(|entry| entry.title)
            .collect()
    }

    /// Moves the highlight and keeps it in view.
    fn palette_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let results = self.palette_results();
        self.palette_selected = palette::step(self.palette_selected, delta, results.len());
        let searching = !self.palette_query.trim().is_empty();
        if let Some(position) = rows(&results, searching)
            .iter()
            .position(|row| *row == Row::Item(self.palette_selected))
        {
            // The heading above the first entry of a group scrolls in with it.
            let position = if position > 0
                && matches!(rows(&results, searching)[position - 1], Row::Heading(_))
            {
                position - 1
            } else {
                position
            };
            self.palette_scroll.scroll_to_item(position);
        }
        cx.notify();
    }

    /// Runs the highlighted entry.
    pub fn palette_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let results = self.palette_results();
        if let Some(entry) = results.get(self.palette_selected) {
            let command = entry.command;
            self.run_palette(command, window, cx);
        }
    }

    /// Closes the palette and does what the entry says.
    fn run_palette(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        // Closing first lets the action decide where the keyboard goes next: a new
        // shell takes it, a page takes it, a form takes it.
        self.close_palette(window, cx);
        match command {
            Command::Switch(id) => self.activate_in(id, window, cx),
            Command::Shell(index) => self.launch_choice(index, window, cx),
            Command::Connect(host) => {
                if let Some(host) = self.book.host(host).cloned() {
                    // The connection belongs to the workspace. The hosts page is the
                    // one place that already shows the form beside the saved record.
                    if !matches!(
                        self.workspace.destination(),
                        Destination::LocalShells | Destination::Sessions | Destination::Hosts
                    ) {
                        self.select(Destination::LocalShells, window, cx);
                    }
                    self.open_remote(&host, cx);
                }
            }
            Command::GoTo(destination) => self.select(destination, window, cx),
            Command::NewHost => {
                self.select(Destination::Hosts, window, cx);
                self.open_create_host(cx);
            }
            Command::ImportSshConfig => {
                self.select(Destination::Hosts, window, cx);
                self.open_import(cx);
            }
            Command::QuickConnect => self.open_quick_connect(window, cx),
            Command::CloseTab(id) => self.request_close_session(id, cx),
            Command::ToggleLanguage => self.toggle_locale(cx),
            Command::ToggleSidebar => self.toggle_rail(cx),
            Command::CycleTheme => self.cycle_theme(cx),
            Command::CycleTextSize => {
                self.workspace.cycle_text_size();
                cx.notify();
            }
        }
    }

    /// Handles the keys the palette reads before its field does.
    fn palette_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = &event.keystroke.modifiers;
        match event.keystroke.key.as_str() {
            "up" => self.palette_step(-1, cx),
            "down" => self.palette_step(1, cx),
            "tab" if modifiers.shift => self.palette_step(-1, cx),
            "tab" => self.palette_step(1, cx),
            "pageup" => self.palette_step(-8, cx),
            "pagedown" => self.palette_step(8, cx),
            "enter" => self.palette_confirm(window, cx),
            "escape" => self.close_palette(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The palette over a dimmed window.
    pub(super) fn palette_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let field = self.palette.clone()?;
        let metrics = self.metrics();
        let strings = self.strings();
        let viewport = window.viewport_size();
        let results = self.palette_results();
        let searching = !self.palette_query.trim().is_empty();
        let width = px(600.0).min(viewport.width - metrics.scale(spacing::LOOSE) * 2.0);
        let selected = self.palette_selected.min(results.len().saturating_sub(1));

        let mut list = div()
            .id("palette.list")
            .debug_selector(|| "palette.list".to_owned())
            .flex()
            .flex_col()
            .max_h(px(380.0).min(viewport.height * 0.6))
            .overflow_y_scroll()
            .track_scroll(&self.palette_scroll)
            .px(metrics.scale(spacing::HAIRLINE + 2.0))
            .pb(metrics.scale(spacing::HAIRLINE + 2.0));
        for row in rows(&results, searching) {
            list = match row {
                Row::Heading(group) => list.child(
                    components::section_label(group_label(group, strings), metrics)
                        .px(metrics.scale(spacing::TIGHT))
                        .pt(metrics.scale(spacing::TIGHT + 2.0))
                        .pb(metrics.scale(spacing::HAIRLINE)),
                ),
                Row::Item(index) => {
                    let entry = &results[index];
                    list.child(self.palette_row(entry, index, index == selected, searching, cx))
                }
            };
        }
        if results.is_empty() {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(metrics.scale(72.0))
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(colors::TEXT_MUTED))
                    .debug_selector(|| "palette.empty".to_owned())
                    .child(strings.palette_empty),
            );
        }

        let hint = |keys: &'static str, label: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(metrics.scale(spacing::HAIRLINE + 1.0))
                .child(components::keycap(keys, metrics))
                .child(label)
        };
        let panel = div()
            .id("palette")
            .debug_selector(|| "palette".to_owned())
            .absolute()
            .top(viewport.height * 0.12)
            .left((viewport.width - width) / 2.0)
            .w(width)
            .flex()
            .flex_col()
            .rounded(px(radius::LARGE + 2.0))
            .bg(theme::swatch(colors::SURFACE_RAISED))
            .border_1()
            .border_color(theme::swatch(colors::BORDER_STRONG))
            .shadow_lg()
            .overflow_hidden()
            .occlude()
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.palette_key(event, window, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .p(metrics.scale(spacing::TIGHT + 2.0))
                    .child(icons::icon(
                        Icon::Search,
                        metrics.icon_size(),
                        theme::swatch(colors::TEXT_MUTED),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .debug_selector(|| "palette.field".to_owned())
                            .child(field),
                    ),
            )
            .child(div().h(px(1.0)).bg(theme::swatch(colors::BORDER)))
            .child(list)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::SNUG))
                    .h(metrics.scale(30.0))
                    .px(metrics.scale(spacing::SNUG))
                    .border_t_1()
                    .border_color(theme::swatch(colors::BORDER))
                    .bg(theme::swatch(colors::SURFACE))
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(colors::TEXT_MUTED))
                    .child(hint("\u{2191}\u{2193}", strings.palette_key_move))
                    .child(hint("\u{21b5}", strings.palette_key_run))
                    .child(hint("Esc", strings.palette_key_close)),
            );

        Some(
            div().child(
                gpui::deferred(
                    gpui::anchored().position(Point::default()).child(
                        div()
                            .relative()
                            .w(viewport.width)
                            .h(viewport.height)
                            .child(
                                div()
                                    .id("palette.backdrop")
                                    .absolute()
                                    .inset_0()
                                    .bg(gpui::rgba(0x0000_0080))
                                    .occlude()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _event, window, cx| {
                                            this.close_palette(window, cx);
                                        }),
                                    ),
                            )
                            .child(panel),
                    ),
                )
                .with_priority(2),
            ),
        )
    }

    /// One entry of the palette list.
    fn palette_row(
        &self,
        entry: &Entry,
        index: usize,
        selected: bool,
        searching: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let metrics = self.metrics();
        let strings = self.strings();
        let command = entry.command;
        let accent = matches!(entry.group, Group::Sessions | Group::Shells | Group::Hosts);
        div()
            .id(("palette.item", index))
            .debug_selector(move || format!("palette.item.{index}"))
            .flex()
            .flex_none()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT + 2.0))
            .h(metrics.scale(38.0))
            .px(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::MEDIUM))
            .cursor_pointer()
            .text_size(metrics.label_size())
            .when(selected, |row| {
                row.bg(theme::swatch(colors::SURFACE_SELECTED))
            })
            .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                if *hovered && this.palette_selected != index {
                    this.palette_selected = index;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _event, window, cx| {
                this.run_palette(command, window, cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .w(metrics.scale(24.0))
                    .h(metrics.scale(24.0))
                    .rounded(px(radius::SMALL))
                    .bg(theme::swatch(if accent {
                        colors::ACCENT_TINT
                    } else {
                        colors::SURFACE_HOVER
                    }))
                    .child(icons::icon(
                        entry.icon,
                        metrics.small_icon_size(),
                        theme::swatch(if accent {
                            colors::ACCENT
                        } else {
                            colors::TEXT_SECONDARY
                        }),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_baseline()
                    .gap(metrics.scale(spacing::TIGHT))
                    .min_w(px(0.0))
                    .child(
                        div()
                            .flex_none()
                            .max_w(metrics.scale(260.0))
                            .truncate()
                            .text_color(theme::swatch(colors::TEXT_PRIMARY))
                            .when(selected, |title| title.font_weight(FontWeight::MEDIUM))
                            .child(SharedString::from(entry.title.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(colors::TEXT_MUTED))
                            .child(SharedString::from(entry.detail.clone())),
                    ),
            )
            .when(entry.current, |row| {
                row.child(components::chip(
                    strings.palette_current,
                    Tone::Accent,
                    metrics,
                ))
            })
            .when(searching, |row| {
                row.child(
                    div()
                        .flex_none()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(colors::TEXT_MUTED))
                        .child(group_label(entry.group, strings)),
                )
            })
            .children(
                entry
                    .shortcut
                    .clone()
                    .map(|shortcut| components::keycap(shortcut, metrics)),
            )
    }

    // -----------------------------------------------------------------------
    // Tab menu
    // -----------------------------------------------------------------------

    /// Opens the menu of one tab where the pointer is.
    ///
    /// The menu takes the keyboard while it is open, so Escape closes it even when a
    /// terminal had the focus (Escape in a terminal belongs to the program in it).
    pub(super) fn open_tab_menu(
        &mut self,
        id: SessionId,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shell_menu_open = false;
        self.tab_menu = Some((id, event.position));
        window.focus(&self.root_focus.clone());
        cx.notify();
    }

    /// Whether a tab menu is open.
    #[allow(dead_code)]
    #[must_use]
    pub fn tab_menu_open(&self) -> bool {
        self.tab_menu.is_some()
    }

    /// The number the sidebar shows beside a destination.
    #[allow(dead_code)]
    #[must_use]
    pub fn sidebar_count(&self, destination: Destination) -> usize {
        self.badge_count(destination)
    }

    /// Closes the tab menu and gives the keyboard back to what was on screen.
    pub(super) fn close_tab_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_menu.take().is_some() {
            self.restore_focus(window, cx);
            cx.notify();
        }
    }

    /// Opens another session like `id`: the same shell, or the same host's form.
    pub fn duplicate_session(
        &mut self,
        id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.workspace.session(id) else {
            return;
        };
        let kind = session.kind().clone();
        let title = session.title().to_owned();
        // A duplicate is a new tab, not a split of the pane that was armed.
        self.workspace.clear_armed();
        match kind {
            SessionKind::Local { shell } => {
                let known = self
                    .workspace
                    .shells()
                    .iter()
                    .find(|candidate| {
                        shell
                            .as_deref()
                            .is_some_and(|program| candidate.program() == program)
                    })
                    .cloned();
                let shell = match (known, shell) {
                    (Some(known), _) => known,
                    (None, Some(program)) => LocalShell::new(title, program),
                    (None, None) => match self.workspace.selected_shell() {
                        Some(shell) => shell.clone(),
                        None => return,
                    },
                };
                self.start_shell(shell, Some(window), cx);
            }
            SessionKind::Remote { host } => {
                if let Some(host) = self.book.host(host).cloned() {
                    self.select(Destination::Hosts, window, cx);
                    self.open_remote(&host, cx);
                }
            }
        }
    }

    /// Closes every tab in `ids`.
    ///
    /// A session with transfers still running is not closed behind the user's back:
    /// the others close at once, and the first such session asks, as its own close
    /// button would.
    pub fn close_sessions(&mut self, ids: Vec<SessionId>, cx: &mut Context<Self>) {
        let mut asked = false;
        for id in ids {
            if self.transfers.active_count(id) > 0 {
                if !asked {
                    self.closing = Some(id);
                    asked = true;
                }
                continue;
            }
            self.close_session(id, cx);
        }
        cx.notify();
    }

    /// The tab menu, where it was opened.
    pub(super) fn tab_menu_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let (id, position) = self.tab_menu?;
        let metrics = self.metrics();
        let strings = self.strings();
        let viewport = window.viewport_size();
        let tab_count = self.workspace.layout().tabs().len();
        let index = (0..tab_count)
            .find(|index| self.workspace.layout().anchor_of(*index) == Some(id))?;
        let platform = Platform::current();

        let item = |label: &'static str,
                    selector: &'static str,
                    shortcut: Option<&'static str>,
                    enabled: bool| {
            let hover = theme::swatch(colors::SURFACE_HOVER);
            div()
                .id(selector)
                .debug_selector(move || selector.to_owned())
                .flex()
                .items_center()
                .justify_between()
                .gap(metrics.scale(spacing::BASE))
                .h(metrics.scale(30.0))
                .px(metrics.scale(spacing::TIGHT + 2.0))
                .rounded(px(radius::MEDIUM))
                .text_size(metrics.label_size())
                .text_color(theme::swatch(if enabled {
                    colors::TEXT_PRIMARY
                } else {
                    colors::TEXT_MUTED
                }))
                .when(enabled, |item| {
                    item.cursor_pointer().hover(move |style| style.bg(hover))
                })
                .child(label)
                .children(shortcut.map(|shortcut| {
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(colors::TEXT_MUTED))
                        .child(shortcut)
                }))
        };
        let divider = || {
            div()
                .h(px(1.0))
                .mx(metrics.scale(spacing::HAIRLINE))
                .my(metrics.scale(2.0))
                .bg(theme::swatch(colors::BORDER))
        };

        let has_others = tab_count > 1;
        let has_right = index + 1 < tab_count;
        let menu = div()
            .debug_selector(|| "tab.menu".to_owned())
            .flex()
            .flex_col()
            .min_w(metrics.scale(220.0))
            .p(metrics.scale(spacing::HAIRLINE))
            .rounded(px(radius::LARGE))
            .bg(theme::swatch(colors::SURFACE_RAISED))
            .border_1()
            .border_color(theme::swatch(colors::BORDER_STRONG))
            .shadow_lg()
            .child(
                item(strings.tab_menu_duplicate, "tab.menu.duplicate", None, true).on_click(
                    cx.listener(move |this, _event, window, cx| {
                        this.close_tab_menu(window, cx);
                        this.duplicate_session(id, window, cx);
                    }),
                ),
            )
            .child(divider())
            .child(
                item(
                    strings.tab_menu_move_left,
                    "tab.menu.move_left",
                    None,
                    index > 0,
                )
                .when(index > 0, |item| {
                    item.on_click(cx.listener(move |this, _event, window, cx| {
                        this.close_tab_menu(window, cx);
                        this.reorder_tab(id, -1, cx);
                    }))
                }),
            )
            .child(
                item(
                    strings.tab_menu_move_right,
                    "tab.menu.move_right",
                    None,
                    index + 1 < tab_count,
                )
                .when(index + 1 < tab_count, |item| {
                    item.on_click(cx.listener(move |this, _event, window, cx| {
                        this.close_tab_menu(window, cx);
                        this.reorder_tab(id, 1, cx);
                    }))
                }),
            )
            .child(divider())
            .child(
                item(
                    strings.tab_menu_close,
                    "tab.menu.close",
                    Some(platform.close_tab_label()),
                    true,
                )
                .on_click(cx.listener(move |this, _event, window, cx| {
                    this.close_tab_menu(window, cx);
                    this.request_close_tab_anchor(id, cx);
                })),
            )
            .child(
                item(
                    strings.tab_menu_close_others,
                    "tab.menu.close_others",
                    None,
                    has_others,
                )
                .when(has_others, |item| {
                    item.on_click(cx.listener(move |this, _event, window, cx| {
                        this.close_tab_menu(window, cx);
                        this.close_other_tabs(id, cx);
                    }))
                }),
            )
            .child(
                item(
                    strings.tab_menu_close_right,
                    "tab.menu.close_right",
                    None,
                    has_right,
                )
                .when(has_right, |item| {
                    item.on_click(cx.listener(move |this, _event, window, cx| {
                        this.close_tab_menu(window, cx);
                        this.close_tabs_after(id, cx);
                    }))
                }),
            );

        Some(
            div().child(
                gpui::deferred(
                    gpui::anchored().position(Point::default()).child(
                        div()
                            .relative()
                            .w(viewport.width)
                            .h(viewport.height)
                            .child(
                                div()
                                    .id("tab.menu.backdrop")
                                    .absolute()
                                    .inset_0()
                                    .occlude()
                                    .on_any_mouse_down(cx.listener(|this, _event, window, cx| {
                                        this.close_tab_menu(window, cx);
                                    })),
                            )
                            .child(
                                gpui::anchored()
                                    .position(clamp_menu(
                                        position,
                                        viewport,
                                        metrics.scale(230.0),
                                        metrics.header_height() + metrics.scale(2.0),
                                    ))
                                    .snap_to_window_with_margin(metrics.scale(spacing::TIGHT))
                                    .child(div().occlude().child(menu)),
                            ),
                    ),
                )
                .with_priority(2),
            ),
        )
    }
}

/// Places a tab menu at the pointer, inside the window and below the tab bar, so it
/// never covers the tab's own tooltip or the tab it belongs to.
pub(super) fn clamp_menu(
    position: Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    width: Pixels,
    top: Pixels,
) -> Point<Pixels> {
    Point {
        x: position.x.min(viewport.width - width).max(px(0.0)),
        y: position.y.max(top),
    }
}
