//! The floating layers of the window: the command palette and the tab menu.
//!
//! Both are keyboard-and-pointer shortcuts to things the window can already do; neither
//! holds state of its own beyond what is open and what is highlighted, so every action
//! they run goes through the same methods as the buttons elsewhere.

use gpui::prelude::*;
use gpui::{
    App, Context, Div, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point,
    SharedString, Stateful, Window, div, px,
};
use shellune_core::{
    HostConfig, LocalShell, MAX_TAB_NAME_CHARS, PaneTarget, SessionId, SessionKind, TabAccent,
    TabId, tab_name_in_limits,
};

use super::{
    AccentRow, PaneKind, ShelluneView, TabRenameLimit, destination_icon, program_label,
    session_target,
};
use crate::app::components::{self, Variant};
use crate::app::copy;
use crate::app::icons::{self, Icon};
use crate::app::layout::Node;
use crate::app::nav::Destination;
use crate::app::palette::{self, Command, Entry, Group};
use crate::app::snippets::body_preview;
use crate::app::text_field::TextField;
use crate::app::theme::{self, Metrics, Tone, palette as colors, radius, spacing};
use crate::app::workspace::UnreadNotices;

/// The background marks of one tab, in the all-tabs list (R7).
///
/// The strip has room for one word, so the list writes the whole story: a chip per kind
/// with its tooltip saying what happened and that opening the tab clears it. Each chip
/// is a word, so the list is readable in both themes and needs no animation.
fn attention_rows(
    unread: UnreadNotices,
    strings: &copy::Strings,
    metrics: Metrics,
    index: usize,
) -> Vec<Stateful<Div>> {
    let kinds = [
        (
            unread.bell,
            strings.tab_mark_bell,
            strings.tab_mark_bell_hint,
            "bell",
        ),
        (
            unread.finished,
            strings.tab_mark_finished,
            strings.tab_mark_finished_hint,
            "finished",
        ),
    ];
    let mut marks = Vec::new();
    for (present, label, hint, name) in kinds {
        if !present {
            continue;
        }
        marks.push(
            components::chip(label, Tone::Accent, metrics)
                .id(SharedString::from(format!("tabs.list.mark.{name}.{index}")))
                .debug_selector(move || format!("tabs.list.mark.{name}.{index}"))
                .tooltip(components::tooltip(hint)),
        );
    }
    marks
}

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
        Group::Snippets => strings.palette_group_snippets,
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
        self.remember_focus(window, cx);
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
        self.restore_overlay_focus(window, cx);
        cx.notify();
    }

    /// Remembers the focused element so an overlay can give it back.
    ///
    /// A page-level focus does not replace a field noted on the previous frame. The
    /// click that opens the overlay may already have blurred that field.
    pub(super) fn remember_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = window.focused(cx);
        if let Some(handle) = current.as_ref()
            && !self.is_page_focus(handle)
        {
            self.focus_return = Some(handle.downgrade());
            return;
        }
        let kept = self
            .focus_return
            .as_ref()
            .and_then(|handle| handle.upgrade());
        if kept
            .as_ref()
            .is_some_and(|handle| !self.is_page_focus(handle))
        {
            return;
        }
        self.focus_return = current.map(|handle| handle.downgrade());
    }

    /// Returns the keyboard to the element that opened the overlay, or to the page.
    pub(super) fn restore_overlay_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(handle) = self.focus_return.take().and_then(|handle| handle.upgrade()) {
            window.focus(&handle);
            return;
        }
        self.restore_focus(window, cx);
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
    ///
    /// Shortcuts come from the bindings in force, not from the catalogue's defaults,
    /// so a rebound or unbound action is shown as it really is (R5).
    fn palette_entries(&self) -> Vec<Entry> {
        let strings = self.strings();
        let ux = copy::ux();
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
                title: self.tab_title(id),
                detail: format!(
                    "{kind_label} · {target} · {}",
                    copy::session_state_label(session.state(), locale)
                ),
                shortcut: (index < crate::app::TAB_JUMP_COUNT)
                    .then(|| crate::app::keymap::tab_jump_action(index + 1))
                    .flatten()
                    .and_then(|spec| self.keymap.label(spec.id)),
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
                shortcut: (index == default_shell)
                    .then(|| self.keymap.label("shell.new"))
                    .flatten(),
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

        // The saved commands, each with the two things the palette can do with it (R6).
        // Both entries name the command, so a search for the command finds the pair and a
        // reader can tell "type it" from "run it" before choosing.
        for snippet in self.snippets.snippets() {
            let id = snippet.id();
            let name = snippet.name().to_owned();
            let lines = snippet.line_count();
            let mut detail = body_preview(snippet.body(), 60);
            if lines > 1 {
                detail.push_str(" · ");
                detail.push_str(&copy::fill(
                    ux.snippet_line_count,
                    &[("count", &lines.to_string())],
                ));
            }
            entries.push(Entry {
                command: Command::InsertSnippet(id),
                group: Group::Snippets,
                title: copy::fill(ux.snippet_insert_action, &[("name", &name)]),
                detail: detail.clone(),
                shortcut: None,
                icon: Icon::Terminal,
                current: false,
            });
            entries.push(Entry {
                command: Command::RunSnippet(id),
                group: Group::Snippets,
                title: copy::fill(ux.snippet_run_action, &[("name", &name)]),
                detail,
                shortcut: None,
                icon: Icon::Play,
                current: false,
            });
        }
        entries.push(Entry {
            command: Command::ManageSnippets,
            group: Group::Snippets,
            title: ux.snippet_manage_action.to_owned(),
            detail: ux.snippet_manage_hint.to_owned(),
            shortcut: None,
            icon: Icon::Terminal,
            current: false,
        });
        entries.push(Entry {
            command: Command::NewSnippet,
            group: Group::Snippets,
            title: ux.snippet_new_action.to_owned(),
            detail: ux.snippet_storage_note.to_owned(),
            shortcut: None,
            icon: Icon::Plus,
            current: false,
        });

        for destination in Destination::PAGES {
            entries.push(Entry {
                command: Command::GoTo(destination),
                group: Group::Navigate,
                title: destination.label(strings).to_owned(),
                detail: destination.summary(strings).to_owned(),
                shortcut: crate::app::keymap::page_action(destination)
                    .and_then(|spec| self.keymap.label(spec.id)),
                icon: destination_icon(destination),
                current: false,
            });
        }

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
                shortcut: self.keymap.label("tab.close"),
                icon: Icon::Close,
                current: false,
            });
            // Naming a tab and colouring it are tab actions, so the palette is how
            // they are reached without a pointer on the strip (R2, R4).
            let tab = self.workspace.tab_of_session(id);
            entries.push(Entry {
                command: Command::RenameTab(id),
                group: Group::Actions,
                title: strings.tab_menu_rename.to_owned(),
                detail: self.tab_title(id),
                shortcut: None,
                icon: Icon::Pencil,
                current: false,
            });
            let accent = tab.and_then(|tab| self.workspace.tab_accent(tab));
            entries.push(Entry {
                command: Command::CycleTabAccent(id),
                group: Group::Actions,
                title: strings.tab_accent_label.to_owned(),
                detail: format!(
                    "{} → {}",
                    accent.map_or(strings.tab_accent_none, |accent| {
                        copy::tab_accent_label(accent, locale)
                    }),
                    copy::tab_accent_label(
                        accent.map_or(TabAccent::ALL[0], TabAccent::next),
                        locale
                    )
                ),
                shortcut: None,
                icon: Icon::Layers,
                current: false,
            });
            // Stepping through the colours cannot get back to none of them, and a name
            // can only be cleared from a control; both are here so the keyboard alone
            // can undo them as well as apply them (R2, R4).
            entries.push(Entry {
                command: Command::RestoreTabTitle(id),
                group: Group::Actions,
                title: strings.palette_restore_tab_title.to_owned(),
                detail: self.tab_title(id),
                shortcut: None,
                icon: Icon::Pencil,
                current: false,
            });
            entries.push(Entry {
                command: Command::ClearTabAccent(id),
                group: Group::Actions,
                title: strings.palette_clear_tab_accent.to_owned(),
                detail: accent
                    .map_or(strings.tab_accent_none, |accent| {
                        copy::tab_accent_label(accent, locale)
                    })
                    .to_owned(),
                shortcut: None,
                icon: Icon::Layers,
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
            shortcut: self.keymap.label("language.toggle"),
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
            command: Command::CycleMotion,
            group: Group::Actions,
            title: strings.palette_motion.to_owned(),
            detail: self.motion_label().to_owned(),
            shortcut: None,
            icon: Icon::Play,
            current: false,
        });
        entries.push(Entry {
            command: Command::CycleTextSize,
            group: Group::Actions,
            title: strings.palette_text_size.to_owned(),
            detail: self.workspace.zoom().label().to_owned(),
            shortcut: self.keymap.label("text.size"),
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

    /// The shortcut the palette lists for one entry, for the tests that check the
    /// palette reads the bindings in force rather than a copy of the defaults (R5).
    #[cfg(test)]
    pub fn palette_shortcut(&self, title: &str) -> Option<String> {
        self.palette_entries()
            .into_iter()
            .find(|entry| entry.title == title)
            .and_then(|entry| entry.shortcut)
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
            Command::GoTo(destination) => {
                if destination == Destination::Forwards {
                    self.open_global_forwards(window, cx);
                } else {
                    self.select(destination, window, cx);
                }
            }
            Command::NewHost => {
                self.select(Destination::Hosts, window, cx);
                self.open_create_host(cx);
            }
            Command::ImportSshConfig => {
                self.select(Destination::Hosts, window, cx);
                self.open_import(cx);
            }
            Command::QuickConnect => self.open_quick_connect(window, cx),
            Command::InsertSnippet(id) => self.use_snippet(id, false, cx),
            Command::RunSnippet(id) => self.use_snippet(id, true, cx),
            Command::ManageSnippets => self.open_snippets(false, cx),
            Command::NewSnippet => self.open_snippets(true, cx),
            Command::CloseTab(id) => self.request_close_session(id, cx),
            Command::RenameTab(id) => {
                if let Some(tab) = self.workspace.tab_of_session(id) {
                    self.open_tab_rename(tab, window, cx);
                }
            }
            Command::CycleTabAccent(id) => {
                if let Some(tab) = self.workspace.tab_of_session(id) {
                    let next = Some(
                        self.workspace
                            .tab_accent(tab)
                            .map_or(TabAccent::ALL[0], TabAccent::next),
                    );
                    self.set_tab_accent(tab, next, cx);
                }
            }
            Command::RestoreTabTitle(id) => {
                if let Some(tab) = self.workspace.tab_of_session(id) {
                    self.restore_tab_title(tab, cx);
                }
            }
            Command::ClearTabAccent(id) => {
                if let Some(tab) = self.workspace.tab_of_session(id) {
                    self.set_tab_accent(tab, None, cx);
                }
            }
            Command::ToggleLanguage => self.toggle_locale(cx),
            Command::CycleTheme => self.cycle_theme(cx),
            Command::CycleMotion => self.cycle_motion(cx),
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
            super::overlay_layer().child(
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
        self.remember_focus(window, cx);
        self.accent_hover = None;
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
            self.accent_hover = None;
            self.restore_overlay_focus(window, cx);
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
    /// A session with transfers or forwards still running is not closed behind the
    /// user's back: the others close at once, and the first such session asks, as
    /// its own close button would.
    pub fn close_sessions(&mut self, ids: Vec<SessionId>, cx: &mut Context<Self>) {
        let mut asked = false;
        for id in ids {
            if self.close_needs_confirmation(id) {
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
        let index =
            (0..tab_count).find(|index| self.workspace.layout().anchor_of(*index) == Some(id))?;
        let item = |label: &'static str,
                    selector: &'static str,
                    shortcut: Option<String>,
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
        let tab = self.workspace.tab_of_session(id);
        // The menu holds more than it used to (a name, the colours and the close
        // actions), so it caps its own height and scrolls: at 200% text in the
        // smallest window the last entry still has to be reachable (R4).
        let max_height =
            (viewport.height - metrics.header_height() - metrics.scale(spacing::LOOSE))
                .max(metrics.scale(120.0));
        let menu = div()
            .id("tab.menu")
            .debug_selector(|| "tab.menu".to_owned())
            .flex()
            .flex_col()
            .min_w(metrics.scale(220.0))
            .max_h(max_height)
            .overflow_y_scroll()
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
            .child(
                item(
                    strings.tab_menu_rename,
                    "tab.menu.rename",
                    None,
                    tab.is_some(),
                )
                .when_some(tab, |menu, tab| {
                    menu.on_click(cx.listener(move |this, _event, window, cx| {
                        this.close_tab_menu(window, cx);
                        this.open_tab_rename(tab, window, cx);
                    }))
                }),
            )
            .child(divider())
            .children(tab.map(|tab| self.tab_accent_choices(tab, AccentRow::Menu, cx)))
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
                    self.keymap.label("tab.close"),
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
            super::overlay_layer().child(
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

/// The first pane's target of a tab that has not been started.
///
/// A waiting tab has no session to describe it, so its target is read from the tree
/// the snapshot restored. Nothing here is opened.
fn first_target(node: &Node) -> Option<&PaneTarget> {
    match node {
        Node::Leaf(leaf) => Some(leaf.target()),
        Node::Split { first, second, .. } => first_target(first).or_else(|| first_target(second)),
    }
}

impl ShelluneView {
    // -----------------------------------------------------------------------
    // Tab identity: its own name, its colour and the list of every tab (R2)
    // -----------------------------------------------------------------------

    /// The colour choices of one tab, as a labelled row of swatches.
    ///
    /// The chosen colour carries a ring and the rest do not, so the row says which one
    /// is set without relying on the mark on the tab being visible. Choosing a colour
    /// changes the mark at once and nothing else: the state, the target and the name of
    /// every tab stay what they are.
    ///
    /// A swatch is a control like the button beside it, so it answers the pointer the
    /// same way: the fill and ring of a hovered or pressed swatch come from
    /// [`theme::accent_swatch_face`], and the chosen one keeps its ring in every state.
    fn tab_accent_choices(&self, tab: TabId, row: AccentRow, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let locale = self.workspace.locale();
        let chosen = self.workspace.tab_accent(tab);
        let prefix = row.selector();
        let mut swatches = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(metrics.scale(spacing::HAIRLINE + 2.0))
            .px(metrics.scale(spacing::TIGHT + 2.0))
            .py(metrics.scale(2.0))
            .debug_selector(move || format!("{prefix}.accents"));
        let none_selector = format!("{prefix}.accent.none");
        swatches = swatches.child(
            components::compact_button(strings.tab_accent_none, Variant::Ghost, metrics)
                .id(SharedString::from(none_selector.clone()))
                .debug_selector({
                    let selector = none_selector.clone();
                    move || selector.clone()
                })
                .when(chosen.is_none(), |button| {
                    button.border_color(theme::swatch(colors::ACCENT))
                })
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.set_tab_accent(tab, None, cx);
                })),
        );
        for accent in TabAccent::ALL {
            let selector = format!("{prefix}.accent.{}", accent_selector(accent));
            let selected = chosen == Some(accent);
            let face = self.accent_face(row, accent, chosen);
            let pressed = theme::accent_swatch_face(selected, theme::SwatchState::Pressed);
            swatches = swatches.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector({
                        let selector = selector.clone();
                        move || selector.clone()
                    })
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(metrics.hit_target())
                    .rounded(px(radius::SMALL))
                    .bg(face.fill_color())
                    .border_1()
                    .border_color(face.ring_color())
                    .cursor_pointer()
                    .active(move |style| {
                        style
                            .bg(pressed.fill_color())
                            .border_color(pressed.ring_color())
                    })
                    // The pointer's own position is what the row draws from, so the
                    // hover a person sees and the hover a test reads are one value.
                    .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                        let on_this_swatch = Some((row, accent));
                        if *hovered && this.accent_hover != on_this_swatch {
                            this.accent_hover = on_this_swatch;
                            cx.notify();
                        } else if !*hovered && this.accent_hover == on_this_swatch {
                            this.accent_hover = None;
                            cx.notify();
                        }
                    }))
                    .tooltip(components::tooltip(copy::tab_accent_label(accent, locale)))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.set_tab_accent(tab, Some(accent), cx);
                    }))
                    .child(
                        div()
                            .flex_none()
                            .size(metrics.scale(14.0))
                            .rounded(px(radius::SMALL))
                            .bg(theme::tab_accent_color(accent)),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(metrics.scale(2.0))
            .child(
                div()
                    .px(metrics.scale(spacing::TIGHT + 2.0))
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(colors::TEXT_MUTED))
                    .child(strings.tab_accent_label),
            )
            .child(swatches)
    }

    /// The face one swatch of `row` draws for `accent`.
    ///
    /// The drawing and the tests that check the hover and press feedback read this one
    /// place, so what a test asserts is what the swatch is painted with.
    fn accent_face(
        &self,
        row: AccentRow,
        accent: TabAccent,
        chosen: Option<TabAccent>,
    ) -> theme::SwatchFace {
        let state = if self.accent_hover == Some((row, accent)) {
            theme::SwatchState::Hovered
        } else {
            theme::SwatchState::Resting
        };
        theme::accent_swatch_face(chosen == Some(accent), state)
    }

    /// The row and colour the pointer is on, for the tests that check both rows answer
    /// it.
    #[cfg(test)]
    pub fn accent_hover_for_test(&self) -> Option<(AccentRow, TabAccent)> {
        self.accent_hover
    }

    /// The face the swatch of `accent` in `row` is drawn with now, for the tests that
    /// check the drawing follows the pointer.
    #[cfg(test)]
    pub fn accent_face_for_test(
        &self,
        row: AccentRow,
        tab: TabId,
        accent: TabAccent,
    ) -> theme::SwatchFace {
        self.accent_face(row, accent, self.workspace.tab_accent(tab))
    }

    /// Drops the tab layers whose tab is gone.
    ///
    /// Closing a session can happen while its menu or its name dialog is open (the
    /// close shortcut works behind an overlay). A layer that names a tab nobody has any
    /// more would be describing something that no longer exists.
    pub(super) fn forget_closed_tab_layers(&mut self) {
        let alive = |view: &Self, tab: TabId| view.workspace.layout().tab(tab).is_some();
        if self
            .tab_rename
            .as_ref()
            .is_some_and(|dialog| !alive(self, dialog.tab))
        {
            self.tab_rename = None;
        }
        if let Some((id, _)) = self.tab_menu
            && self
                .workspace
                .tab_of_session(id)
                .is_none_or(|tab| !alive(self, tab))
        {
            self.tab_menu = None;
        }
    }

    /// Chooses the distinguishing colour of one tab, or clears it.
    fn set_tab_accent(&mut self, tab: TabId, accent: Option<TabAccent>, cx: &mut Context<Self>) {
        if self.workspace.recolor_tab(tab, accent) {
            // The colour is part of the saved layout, like the name and the order.
            self.checkpoint_layout();
        }
        cx.notify();
    }

    /// Opens the dialog that names one tab.
    ///
    /// The field is made per opening, so it starts with the name the tab has now (or
    /// empty when it has none) and its placeholder is in the language on screen.
    pub(super) fn open_tab_rename(
        &mut self,
        tab: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.layout().tab(tab).is_none() {
            return;
        }
        let current = self
            .workspace
            .tab_custom_title(tab)
            .unwrap_or_default()
            .to_owned();
        let placeholder = self.strings().tab_rename_placeholder;
        let field = cx.new(|cx| TextField::with_text(placeholder, &current, cx));
        let view = cx.entity().downgrade();
        field.update(cx, |text_field, _cx| {
            text_field.on_submit(move |_text, _window, app| {
                view.update(app, |view, cx| {
                    view.submit_tab_rename(cx);
                    // The field keeps its text: a refused name stays visible beside the
                    // reason it was refused.
                    false
                })
                .unwrap_or(false)
            });
        });
        let session = self
            .workspace
            .tab_anchors()
            .into_iter()
            .find(|id| self.workspace.tab_of_session(*id) == Some(tab));
        self.remember_focus(window, cx);
        window.focus(field.read(cx).focus_handle());
        self.accent_hover = None;
        self.tab_rename = Some(super::TabRename {
            tab,
            session,
            field,
            problem: None,
            shortened: false,
        });
        cx.notify();
    }

    /// Closes the name dialog and gives the keyboard back to what was on screen.
    pub(super) fn close_tab_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_rename.take().is_some() {
            self.accent_hover = None;
            self.restore_overlay_focus(window, cx);
            cx.notify();
        }
    }

    /// Closes the name dialog from a control that has no window, returning the keyboard
    /// on the next frame.
    fn close_tab_rename_pending(&mut self, cx: &mut Context<Self>) {
        if self.tab_rename.take().is_some() {
            self.accent_hover = None;
            self.restore_focus_pending = true;
            cx.notify();
        }
    }

    /// Names the tab the dialog is about, or says why the name cannot be used.
    ///
    /// A name longer than [`MAX_TAB_NAME_CHARS`] is not refused. The tab keeps the
    /// first [`MAX_TAB_NAME_CHARS`] characters, and the dialog stays open on that name
    /// with a line saying what was kept and how to change it, so the shortening is
    /// never the quiet difference between what was typed and what the tab says.
    fn submit_tab_rename(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.tab_rename.as_ref() else {
            return;
        };
        let tab = dialog.tab;
        let field = dialog.field.clone();
        let entered = field.read(cx).text().to_owned();
        let name = entered.trim().to_owned();
        if name.is_empty() {
            let reason = self.strings().tab_rename_empty.to_owned();
            if let Some(dialog) = self.tab_rename.as_mut() {
                dialog.problem = Some(reason);
            }
            cx.notify();
            return;
        }
        let kept = tab_name_in_limits(&name);
        let cut = kept.chars().count() < name.chars().count();
        if self.workspace.rename_tab(tab, Some(kept.clone())) {
            self.checkpoint_layout();
        }
        if !cut {
            self.close_tab_rename_pending(cx);
            return;
        }
        // The dialog stays open, holding the name the tab kept, so the line about the
        // cut sits beside what was saved instead of closing over it.
        field.update(cx, |field, cx| field.set_text(kept, cx));
        if let Some(dialog) = self.tab_rename.as_mut() {
            dialog.problem = None;
            dialog.shortened = true;
        }
        cx.notify();
    }

    /// One line of the name dialog about the length limit, with its own selector so a
    /// test can tell the line shown while typing from the one shown after saving.
    fn tab_rename_notice(&self, selector: &'static str, template: &'static str) -> Div {
        let metrics = self.metrics();
        let text = copy::fill(template, &[("max", &MAX_TAB_NAME_CHARS.to_string())]);
        components::banner(Tone::Warning, metrics)
            .debug_selector(move || selector.to_owned())
            .text_color(theme::swatch(colors::TEXT_PRIMARY))
            .child(SharedString::from(text))
    }

    /// Which line the name dialog shows about the length limit, if any.
    ///
    /// The drawing and the tests read this one place, so the line a person sees and the
    /// line a test expects cannot drift apart.
    fn tab_rename_limit(&self, cx: &App) -> Option<TabRenameLimit> {
        let dialog = self.tab_rename.as_ref()?;
        if dialog.field.read(cx).text().trim().chars().count() > MAX_TAB_NAME_CHARS {
            return Some(TabRenameLimit::Typing);
        }
        dialog.shortened.then_some(TabRenameLimit::Saved)
    }

    /// Which line the name dialog shows about the length limit, for the tests that
    /// check the cut is never silent.
    #[cfg(test)]
    pub fn tab_rename_limit_for_test(&self, cx: &App) -> Option<TabRenameLimit> {
        self.tab_rename_limit(cx)
    }

    /// Returns one tab to its automatic title, keeping its colour and its panes.
    fn restore_tab_title(&mut self, tab: TabId, cx: &mut Context<Self>) {
        if self.workspace.rename_tab(tab, None) {
            self.checkpoint_layout();
        }
        self.close_tab_rename_pending(cx);
    }

    /// Whether the list of every tab is open.
    #[allow(dead_code)]
    #[must_use]
    pub fn tab_list_open(&self) -> bool {
        self.tab_list_open
    }

    /// Whether the tab-name dialog is open.
    #[allow(dead_code)]
    #[must_use]
    pub fn tab_rename_open(&self) -> bool {
        self.tab_rename.is_some()
    }

    /// Why the name dialog refused the name that was entered, if it did.
    #[allow(dead_code)]
    #[must_use]
    pub fn tab_rename_problem(&self) -> Option<&str> {
        self.tab_rename
            .as_ref()
            .and_then(|dialog| dialog.problem.as_deref())
    }

    /// Opens the all-tabs list, or closes it when it is open.
    pub(super) fn toggle_tab_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_list_open {
            self.close_tab_list(window, cx);
            return;
        }
        self.shell_menu_open = false;
        self.tab_menu = None;
        self.remember_focus(window, cx);
        self.tab_list_open = true;
        cx.notify();
    }

    /// Closes the all-tabs list and gives the keyboard back to what was on screen.
    pub(super) fn close_tab_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.tab_list_open) {
            self.restore_overlay_focus(window, cx);
            cx.notify();
        }
    }

    /// The list of every open tab, drawn over the page.
    ///
    /// The strip can only show the tabs that fit; this list shows all of them, in strip
    /// order, with the target, the kind and the state of each, so a session can be found
    /// by what it connects to and not only by its name (R2, R4). Choosing a row shows
    /// that tab; the pencil names it and the colours give it a mark.
    pub(super) fn tab_list_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        if !self.tab_list_open {
            return None;
        }
        let metrics = self.metrics();
        let strings = self.strings();
        let locale = self.workspace.locale();
        let count = self.workspace.layout().tabs().len();
        let active = self.workspace.layout().active_tab_id();
        let (header, subtitle) = self.modal_header(
            Some(Icon::Layers),
            strings.tab_list_title.to_owned(),
            Some(strings.tab_list_hint.to_owned()),
            "tabs.list.close",
            cx,
            |this, cx| {
                this.tab_list_open = false;
                this.restore_focus_pending = true;
                cx.notify();
            },
        );
        let mut rows = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(2.0))
            .debug_selector(|| "tabs.list".to_owned());
        for index in 0..count {
            let Some(tab) = self.workspace.layout().tabs().get(index) else {
                continue;
            };
            let tab_id = tab.id();
            let session = self
                .workspace
                .tab_anchors()
                .into_iter()
                .find(|id| self.workspace.tab_of_session(*id) == Some(tab_id));
            let name = match session {
                Some(id) => self.tab_title(id),
                None => tab.display_title().to_owned(),
            };
            let (kind_label, remote, target, state_label, tone) =
                match session.and_then(|id| self.workspace.session(id)) {
                    Some(session) => {
                        let kind = PaneKind::of(session.kind());
                        let remote = kind == PaneKind::Remote;
                        let target = match session.kind() {
                            SessionKind::Local { .. } => {
                                program_label(&session_target(&self.book, session))
                            }
                            SessionKind::Remote { .. } => session_target(&self.book, session),
                        };
                        (
                            if remote {
                                strings.session_kind_remote
                            } else {
                                strings.session_kind_local
                            },
                            remote,
                            target,
                            copy::session_state_label(session.state(), locale),
                            super::session_tone(session.state()),
                        )
                    }
                    None => {
                        let target = first_target(tab.root())
                            .map(|target| match target {
                                PaneTarget::Local { shell } => shell
                                    .as_deref()
                                    .map(program_label)
                                    .unwrap_or_else(|| strings.session_kind_local.to_owned()),
                                PaneTarget::Remote { host } => self
                                    .book
                                    .host(*host)
                                    .map(|host| host.target())
                                    .unwrap_or_else(|| host.to_string()),
                            })
                            .unwrap_or_default();
                        (
                            strings.session_kind_local,
                            false,
                            target,
                            strings.pane_pending.to_owned(),
                            Tone::Warning,
                        )
                    }
                };
            let accent = tab.accent();
            let selector = format!("tabs.list.row.{index}");
            let current = active == Some(tab_id);
            // What this tab is carrying from the background, spelled out: the strip has
            // room for one word, the list says which news it was and what clears it
            // (R7). Choosing the row shows the tab, which clears the marks.
            let unread = self.workspace.unread(tab_id);
            let marks = attention_rows(unread, strings, metrics, index);
            let hover = theme::swatch(colors::SURFACE_HOVER);
            let rename = tab_id;
            let row = div()
                .id(SharedString::from(selector.clone()))
                .debug_selector({
                    let selector = selector.clone();
                    move || selector.clone()
                })
                .flex()
                .items_center()
                .flex_wrap()
                .gap(metrics.scale(spacing::TIGHT))
                .min_h(metrics.hit_target())
                .px(metrics.scale(spacing::TIGHT + 2.0))
                .py(metrics.scale(2.0))
                .rounded(px(radius::SMALL))
                .border_1()
                .border_color(if current {
                    theme::swatch(colors::BORDER_STRONG)
                } else {
                    theme::clear()
                })
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_click(cx.listener(move |this, _event, window, cx| {
                    this.close_tab_list(window, cx);
                    this.select_tab(index, window, cx);
                }))
                .children(accent.map(|accent| {
                    super::tab_accent_mark(accent, metrics, format!("tabs.list.accent.{index}"))
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w(metrics.scale(120.0))
                        .truncate()
                        .text_size(metrics.label_size())
                        .when(current, |name| name.font_weight(FontWeight::MEDIUM))
                        .text_color(theme::swatch(colors::TEXT_PRIMARY))
                        .child(SharedString::from(name)),
                )
                .children(accent.map(|accent| {
                    div()
                        .flex_none()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(colors::TEXT_MUTED))
                        .debug_selector(move || format!("tabs.list.accent-name.{index}"))
                        .child(copy::tab_accent_label(accent, locale))
                }))
                .child(components::kind_badge(kind_label, remote, metrics))
                .child(
                    div()
                        .flex_1()
                        .min_w(metrics.scale(96.0))
                        .truncate()
                        .font_family(crate::app::terminal::monospace_family())
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(colors::TEXT_MUTED))
                        .child(SharedString::from(target)),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(metrics.scale(spacing::HAIRLINE))
                        .child(components::status_dot(tone, metrics))
                        .child(
                            div()
                                .text_size(metrics.caption_size())
                                .text_color(tone.text())
                                .child(SharedString::from(state_label)),
                        ),
                )
                .when(current, |row| {
                    row.child(components::chip(
                        strings.palette_current,
                        Tone::Accent,
                        metrics,
                    ))
                })
                .children(marks)
                .child(
                    components::icon_only_button(Icon::Pencil, metrics)
                        .id(SharedString::from(format!("tabs.list.rename.{index}")))
                        .debug_selector(move || format!("tabs.list.rename.{index}"))
                        .tooltip(components::tooltip(strings.tab_menu_rename))
                        .on_click(cx.listener(move |this, _event, window, cx| {
                            cx.stop_propagation();
                            this.close_tab_list(window, cx);
                            this.open_tab_rename(rename, window, cx);
                        })),
                );
            rows = rows.child(row);
        }
        let body = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::LOOSE))
            .child(rows)
            .into_any_element();
        Some(self.modal(
            window,
            "tabs.list.dialog",
            680.0,
            header,
            subtitle,
            body,
            None,
        ))
    }

    /// The dialog that names one tab and chooses its colour.
    pub(super) fn tab_rename_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let dialog = self.tab_rename.as_ref()?;
        let metrics = self.metrics();
        let strings = self.strings();
        let tab = dialog.tab;
        // The dialog names the tab it is about by what the tab shows and by what it
        // connects to, so two tabs with one name are still told apart here (R2).
        let shown = self
            .workspace
            .layout()
            .tab(tab)
            .map(|tab| tab.display_title().to_owned());
        let target = dialog
            .session
            .and_then(|id| self.workspace.session(id))
            .map(|session| match session.kind() {
                SessionKind::Local { .. } => program_label(&session_target(&self.book, session)),
                SessionKind::Remote { .. } => session_target(&self.book, session),
            });
        let subtitle = shown.map(|shown| match target {
            Some(target) => format!("{shown} · {target}"),
            None => shown,
        });
        let has_name = self.workspace.tab_custom_title(tab).is_some();
        let tab_for_auto = tab;
        // What the field holds now: a name past the limit says so before it is saved,
        // so the cut is visible while the user can still shorten it by hand.
        let limit = self.tab_rename_limit(cx);
        let (header, hint) = self.modal_header(
            Some(Icon::Pencil),
            strings.tab_rename_title.to_owned(),
            subtitle,
            "tab.rename.close",
            cx,
            |this, cx| this.close_tab_rename_pending(cx),
        );
        // The field is the first thing in the body, because at the smallest window with
        // 200% text the body is only a few lines tall: the control the dialog exists for
        // has to be the one that is on screen without scrolling. What explains the name
        // and what changes the tab's mark follow it, and are reached by the body's own
        // scroll (R4); the body always has one line to show and never collapses, which it
        // did while the hint above the field was taller than the whole body.
        let body = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::LOOSE))
            .child(
                div()
                    .debug_selector(|| "tab.rename.field".to_owned())
                    // Escape closes the dialog; without this the field would take it to
                    // empty itself, and the name just typed would be lost.
                    .capture_key_down(cx.listener(
                        |this, event: &gpui::KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                cx.stop_propagation();
                                this.close_tab_rename(window, cx);
                            }
                        },
                    ))
                    .child(dialog.field.clone()),
            )
            // What the dialog says about the name sits right under the name, above the
            // colours, so it is next to the field it is about.
            .children(
                dialog
                    .problem
                    .as_ref()
                    .map(|problem| self.notice_line(problem.clone(), Tone::Danger)),
            )
            .children(limit.map(|limit| {
                let (selector, template) = match limit {
                    TabRenameLimit::Typing => ("tab.rename.limit", strings.tab_rename_limit),
                    TabRenameLimit::Saved => ("tab.rename.shortened", strings.tab_rename_shortened),
                };
                self.tab_rename_notice(selector, template)
            }))
            .child(self.tab_accent_choices(tab, AccentRow::Rename, cx))
            // The explanation of the whole dialog closes the body, where it scrolls
            // rather than pushing the field out of the part of the body that is on screen.
            .child(
                div()
                    .debug_selector(|| "tab.rename.hint".to_owned())
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(colors::TEXT_SECONDARY))
                    .child(strings.tab_rename_hint),
            )
            .into_any_element();
        let footer = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_wrap()
            .gap(metrics.scale(spacing::TIGHT))
            .px(metrics.scale(spacing::LOOSE))
            .py(metrics.scale(spacing::SNUG))
            .border_t_1()
            .border_color(theme::swatch(colors::BORDER))
            .child(
                components::compact_button(strings.tab_rename_auto, Variant::Ghost, metrics)
                    .id("tab.rename.auto")
                    .debug_selector(|| "tab.rename.auto".to_owned())
                    .when(!has_name, |button| button.opacity(0.5))
                    .when(has_name, |button| {
                        button.on_click(cx.listener(move |this, _event, _window, cx| {
                            this.restore_tab_title(tab_for_auto, cx);
                        }))
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(self.text_button(
                        SharedString::from("tab.rename.cancel"),
                        copy::ux().fwd_cancel,
                        Variant::Ghost,
                        cx,
                        |this, cx| this.close_tab_rename_pending(cx),
                    ))
                    .child(
                        components::button(strings.tab_rename_save, Variant::Primary, metrics)
                            .id("tab.rename.save")
                            .debug_selector(|| "tab.rename.save".to_owned())
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.submit_tab_rename(cx);
                            })),
                    ),
            );
        Some(self.modal(
            window,
            "tab.rename.dialog",
            520.0,
            header,
            hint,
            body,
            Some(footer),
        ))
    }

    /// The control beside the strip that opens the list of every tab.
    ///
    /// It is drawn outside the strip's clipped row, so it stays reachable however many
    /// tabs are open, and it says how many there are (R2, R4).
    pub(super) fn tab_list_control(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let count = self.workspace.layout().tabs().len();
        if count == 0 {
            return None;
        }
        let metrics = self.metrics();
        let strings = self.strings();
        let open = self.tab_list_open;
        let tooltip = format!("{} ({})", strings.tab_list_title, count);
        Some(
            components::icon_only_button(Icon::Layers, metrics)
                .size(metrics.hit_target())
                .rounded(px(radius::MEDIUM))
                .when(open, |button| {
                    button.bg(theme::swatch(colors::SURFACE_SELECTED))
                })
                .id("tabs.list.open")
                .debug_selector(|| "tabs.list.open".to_owned())
                .tooltip(components::tooltip(SharedString::from(tooltip)))
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.toggle_tab_list(window, cx);
                })),
        )
    }
}

/// The stable part of an accent's selector, for the tests that click one colour.
fn accent_selector(accent: TabAccent) -> &'static str {
    match accent {
        TabAccent::Iris => "iris",
        TabAccent::Violet => "violet",
        TabAccent::Teal => "teal",
        TabAccent::Lime => "lime",
        TabAccent::Orchid => "orchid",
        TabAccent::Clay => "clay",
    }
}
