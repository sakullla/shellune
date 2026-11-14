//! The settings page: two levels of navigation on the left, and the one page being
//! edited on the right (R7, R16).
//!
//! A category is a group a person would name out loud - appearance, terminal,
//! connection, background notices, shortcuts, data, about - and an item is one of the
//! settings inside it. Only the chosen item is drawn, so the shortcut catalogue can no
//! longer push every other setting below the fold, and the search, the bindings in
//! force and their conflicts are the first thing the shortcut page shows.
//!
//! A window too narrow for two columns folds the navigation into two pickers above the
//! page and keeps navigating from there; nothing is clipped (R8).

use gpui::prelude::*;
use gpui::{App, Context, Div, FontWeight, SharedString, Stateful, Window, div, px};

use super::ShelluneView;
use crate::app::components;
use crate::app::copy::{self, Locale};
use crate::app::hosts::{ConnectionField, connection_problem_text};
use crate::app::keymap;
use crate::app::nav::Destination;
use crate::app::terminal;
use crate::app::theme::{self, Motion, Zoom, palette, radius, spacing};
use crate::app::workspace::AttentionKind;

/// Width below which the navigation column and the page no longer fit side by side, in
/// logical pixels before the text scale is applied (R8).
const TWO_COLUMN_WIDTH: f32 = 780.0;

/// Width of the navigation column, in logical pixels before the text scale.
const NAV_WIDTH: f32 = 190.0;

/// One first-level group of the settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Category {
    /// Theme, language, text size and motion.
    Appearance,
    /// Colours, font size and the shell the + button opens.
    Terminal,
    /// The policy every connection is dialled with.
    Connection,
    /// The two background reminders.
    Notices,
    /// The action catalogue and the keys in force.
    Keys,
    /// Where the hosts are saved, and the SSH import.
    Data,
    /// Name, tagline and version.
    About,
}

/// One settings item: the page the navigation opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    Theme,
    Language,
    TextSize,
    Motion,
    Scheme,
    FontSize,
    DefaultShell,
    Policy,
    BellNotice,
    FinishedNotice,
    Shortcuts,
    HostsFile,
    Import,
    Version,
}

/// The items of every category, in the order the navigation lists them.
const APPEARANCE: [Item; 4] = [Item::Theme, Item::Language, Item::TextSize, Item::Motion];
const TERMINAL: [Item; 3] = [Item::Scheme, Item::FontSize, Item::DefaultShell];
const CONNECTION: [Item; 1] = [Item::Policy];
const NOTICES: [Item; 2] = [Item::BellNotice, Item::FinishedNotice];
const KEYS: [Item; 1] = [Item::Shortcuts];
const DATA: [Item; 2] = [Item::HostsFile, Item::Import];
const ABOUT: [Item; 1] = [Item::Version];

impl Category {
    /// Every category, in the order the navigation lists them.
    pub(super) const ALL: [Self; 7] = [
        Self::Appearance,
        Self::Terminal,
        Self::Connection,
        Self::Notices,
        Self::Keys,
        Self::Data,
        Self::About,
    ];

    /// Stable identifier, used by the navigation selectors and the capture scenarios.
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Terminal => "terminal",
            Self::Connection => "connection",
            Self::Notices => "notices",
            Self::Keys => "keys",
            Self::Data => "data",
            Self::About => "about",
        }
    }

    /// Index of this category among the remembered item positions.
    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or_default()
    }

    /// The category's items, in the order the navigation lists them.
    pub(super) fn items(self) -> &'static [Item] {
        match self {
            Self::Appearance => &APPEARANCE,
            Self::Terminal => &TERMINAL,
            Self::Connection => &CONNECTION,
            Self::Notices => &NOTICES,
            Self::Keys => &KEYS,
            Self::Data => &DATA,
            Self::About => &ABOUT,
        }
    }

    /// The category's name in one language.
    pub(super) fn label(self, locale: Locale) -> &'static str {
        let ux = copy::ux_for(locale);
        match self {
            Self::Appearance => ux.settings_appearance,
            Self::Terminal => ux.settings_terminal,
            Self::Connection => ux.settings_connection,
            Self::Notices => ux.settings_notices,
            Self::Keys => ux.settings_keys,
            Self::Data => ux.settings_data,
            Self::About => ux.settings_about,
        }
    }
}

impl Item {
    /// The category this item belongs to.
    pub(super) fn category(self) -> Category {
        match self {
            Self::Theme | Self::Language | Self::TextSize | Self::Motion => Category::Appearance,
            Self::Scheme | Self::FontSize | Self::DefaultShell => Category::Terminal,
            Self::Policy => Category::Connection,
            Self::BellNotice | Self::FinishedNotice => Category::Notices,
            Self::Shortcuts => Category::Keys,
            Self::HostsFile | Self::Import => Category::Data,
            Self::Version => Category::About,
        }
    }

    /// Stable identifier, used by the page and card selectors.
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Language => "language",
            Self::TextSize => "text-size",
            Self::Motion => "motion",
            Self::Scheme => "scheme",
            Self::FontSize => "font-size",
            Self::DefaultShell => "default-shell",
            Self::Policy => "policy",
            Self::BellNotice => "bell-notice",
            Self::FinishedNotice => "finished-notice",
            Self::Shortcuts => "keys",
            Self::HostsFile => "hosts-file",
            Self::Import => "import",
            Self::Version => "version",
        }
    }

    /// The item's name in one language.
    pub(super) fn label(self, locale: Locale) -> &'static str {
        let ux = copy::ux_for(locale);
        match self {
            Self::Theme => ux.settings_theme,
            Self::Language => ux.settings_language,
            Self::TextSize => ux.settings_text_size,
            Self::Motion => ux.settings_motion,
            Self::Scheme => ux.settings_scheme,
            Self::FontSize => ux.settings_font_size,
            Self::DefaultShell => ux.settings_default_shell,
            Self::Policy => ux.settings_connection_policy,
            Self::BellNotice => ux.settings_bell_notice,
            Self::FinishedNotice => ux.settings_finished_notice,
            Self::Shortcuts => ux.settings_keys,
            Self::HostsFile => ux.settings_hosts_file,
            Self::Import => locale.strings().import_open,
            Self::Version => ux.settings_version,
        }
    }

    /// The sentence under the item's name, when it has one.
    pub(super) fn hint(self, locale: Locale) -> &'static str {
        let ux = copy::ux_for(locale);
        match self {
            Self::Theme => ux.settings_theme_hint,
            Self::Language => ux.settings_language_hint,
            Self::TextSize => ux.settings_text_size_hint,
            Self::Motion => ux.settings_motion_hint,
            Self::Scheme => ux.settings_scheme_hint,
            Self::FontSize => ux.settings_font_size_hint,
            Self::DefaultShell => ux.settings_default_shell_hint,
            Self::Policy => ux.settings_connection_hint,
            Self::BellNotice => ux.settings_bell_notice_hint,
            Self::FinishedNotice => ux.settings_finished_notice_hint,
            Self::Shortcuts => ux.settings_keys_hint,
            Self::HostsFile => "",
            Self::Import => ux.settings_import_hint,
            Self::Version => "",
        }
    }
}

/// Where the settings page is: the category, the item inside it, and the shortcut page's
/// scope filter.
///
/// One item index per category is kept, so going back to a category shows the setting the
/// user was on rather than its first one again (R16). The scope filter belongs to the
/// page rather than to the keymap: it narrows what is listed and changes no binding.
pub(super) struct SettingsNav {
    category: Category,
    item: [usize; 7],
    scope: Option<keymap::Context>,
    /// Whether the category column has the keyboard.
    ///
    /// The shortcut page uses the arrows for its rows, so without this the categories were
    /// a keyboard dead end: Escape hands the keyboard to the column, the arrows walk it,
    /// and Enter opens the page the cursor is on (R16, R8).
    nav_focus: bool,
}

impl SettingsNav {
    /// The page a fresh window opens on: the appearance category, its first item.
    pub(super) fn new() -> Self {
        Self {
            category: Category::Appearance,
            item: [0; 7],
            scope: None,
            nav_focus: false,
        }
    }

    /// The item the category is on.
    fn item_of(&self, category: Category) -> usize {
        self.item[category.index()].min(category.items().len().saturating_sub(1))
    }

    /// Remembers an item of one category.
    fn set_item(&mut self, category: Category, index: usize) {
        self.item[category.index()] = index.min(category.items().len().saturating_sub(1));
    }

    /// The item the page shows.
    fn current(&self) -> Item {
        self.category.items()[self.item_of(self.category)]
    }
}

impl ShelluneView {
    /// Whether the navigation has to fold into pickers above the page.
    ///
    /// The two columns need their own room at the text size in force; below it the page
    /// would either squeeze its labels or push the controls past the window (R8).
    fn settings_folded(&self) -> bool {
        self.viewport_width < self.metrics().scale(TWO_COLUMN_WIDTH)
    }

    /// The settings page: the navigation, then the one item it opens.
    pub(super) fn settings_page(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let folded = self.settings_folded();
        let shown = self.settings_nav.current();
        // Once folded, the navigation comes first: the two pickers are the only way to reach
        // another category, so they stay at the top of the page and the item's heading and its
        // card follow them (R8).
        let body = if folded {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .w_full()
                .min_w(px(0.0))
                .gap(metrics.scale(spacing::LOOSE))
                .child(self.settings_pickers(cx))
                .child(self.settings_item_heading(shown))
                .child(self.settings_item_body(shown, cx))
        } else {
            div()
                .flex()
                .items_start()
                .w_full()
                .min_w(px(0.0))
                .gap(metrics.scale(spacing::LOOSE))
                .child(self.settings_navigation(cx))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.0))
                        .max_w(metrics.scale(640.0))
                        .gap(metrics.scale(spacing::LOOSE))
                        .child(self.settings_item_heading(shown))
                        .child(self.settings_item_body(shown, cx)),
                )
        };
        let page = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w(px(0.0))
            .max_w(metrics.scale(980.0))
            .mx_auto()
            .px(metrics.scale(spacing::LOOSE))
            .py(metrics.scale(spacing::LOOSE))
            .gap(metrics.scale(spacing::LOOSE))
            .debug_selector(|| "settings".to_owned())
            .child(body);

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
            .w_full()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .overflow_y_scroll()
            .overflow_x_hidden()
            .bg(theme::swatch(palette::CANVAS))
            .child(page)
    }

    /// The navigation column: every category, with the items of the chosen one under it.
    fn settings_navigation(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let locale = self.workspace.locale();
        let ringed = !self.pointer_input || self.settings_nav.nav_focus;
        let mut column = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(metrics.scale(NAV_WIDTH))
            .min_w(px(0.0))
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(|| "settings.nav".to_owned());
        for category in Category::ALL {
            let chosen = category == self.settings_nav.category;
            let selector = format!("settings.category.{}", category.id());
            // A category is a line of the column, not a card: the column is navigation,
            // and seven bordered boxes made it heavier than the page it navigates. The
            // chosen category is told by weight and fill; with its pages listed under it,
            // the page carries the bar instead, so one mark says where the page is.
            let expanded = chosen && !Self::single_named_item(category, locale);
            let hover = theme::swatch(palette::SURFACE_HOVER);
            column = column.child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .w_full()
                    .min_w(px(0.0))
                    .min_h(metrics.scale(32.0))
                    .px(metrics.scale(spacing::SNUG))
                    .rounded(px(radius::SMALL))
                    .border_1()
                    .border_color(theme::clear())
                    .cursor_pointer()
                    .when(chosen && !expanded, |row| {
                        row.bg(theme::swatch(palette::SURFACE_SELECTED))
                    })
                    .when(!chosen, |row| row.hover(move |style| style.bg(hover)))
                    .when(expanded, |row| row.mt(metrics.scale(spacing::HAIRLINE)))
                    .id(SharedString::from(selector.clone()))
                    .debug_selector({
                        let selector = selector.clone();
                        move || selector.clone()
                    })
                    .when(chosen && ringed, |row| {
                        row.border_color(theme::swatch(palette::FOCUS_RING))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_size(metrics.body_size())
                            .font_weight(if chosen {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::NORMAL
                            })
                            .text_color(theme::swatch(if chosen {
                                palette::TEXT_PRIMARY
                            } else {
                                palette::TEXT_SECONDARY
                            }))
                            .child(category.label(locale)),
                    )
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.settings_select_category(category, cx);
                    })),
            );
            // A category whose only page carries its own name would put the same label
            // twice in a row; the chosen category already says where the page is (R16).
            if chosen && !Self::single_named_item(category, locale) {
                for (index, item) in category.items().iter().enumerate() {
                    column = column.child(self.settings_nav_item(category, index, *item, cx));
                }
            }
        }
        if self.settings_nav.nav_focus {
            column = column.child(self.settings_nav_hint());
        }
        column
    }

    /// The keys that work while the category column has the keyboard.
    fn settings_nav_hint(&self) -> Div {
        let metrics = self.metrics();
        div()
            .mt(metrics.scale(spacing::TIGHT))
            .px(metrics.scale(spacing::SNUG))
            .w_full()
            .min_w(px(0.0))
            .whitespace_normal()
            .text_size(metrics.caption_size())
            .text_color(theme::swatch(palette::TEXT_MUTED))
            .debug_selector(|| "settings.nav.hint".to_owned())
            .child(copy::ux().settings_nav_hint)
    }

    /// One item under its category: the chosen one carries a bar and a fill, so the state
    /// is not colour alone (R8).
    fn settings_nav_item(
        &self,
        category: Category,
        index: usize,
        item: Item,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let locale = self.workspace.locale();
        let picked = index == self.settings_nav.item_of(category);
        let ringed = !self.pointer_input;
        let selector = format!("settings.item.{}", item.id());
        div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .w_full()
            .min_w(px(0.0))
            .pl(metrics.scale(spacing::SNUG))
            .pr(metrics.scale(spacing::TIGHT))
            .py(metrics.scale(spacing::HAIRLINE + 2.0))
            .rounded(px(radius::SMALL))
            .cursor_pointer()
            .when(picked, |row| {
                row.bg(theme::swatch(palette::SURFACE_SELECTED))
            })
            .when(!picked, |row| {
                let hover = theme::swatch(palette::SURFACE_HOVER);
                row.hover(move |style| style.bg(hover))
            })
            .when(picked && ringed, |row| {
                row.border_1()
                    .border_color(theme::swatch(palette::FOCUS_RING))
            })
            .id(SharedString::from(selector.clone()))
            .debug_selector({
                let selector = selector.clone();
                move || selector.clone()
            })
            .child(
                div()
                    .flex_none()
                    .w(px(2.0))
                    .h(metrics.scale(14.0))
                    .rounded(px(1.0))
                    .bg(if picked {
                        theme::swatch(palette::ACCENT)
                    } else {
                        theme::clear()
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .whitespace_normal()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(if picked {
                        palette::TEXT_PRIMARY
                    } else {
                        palette::TEXT_SECONDARY
                    }))
                    .child(item.label(locale)),
            )
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.settings_select_item(category, index, cx);
            }))
    }

    /// Whether a category has one page, named exactly like the category.
    ///
    /// Such a page is reached by its category: drawing a sub-item with the same words
    /// only repeats the label (R16).
    fn single_named_item(category: Category, locale: Locale) -> bool {
        matches!(category.items(), [only] if only.label(locale) == category.label(locale))
    }

    /// The folded navigation: a category picker and the item picker of the category it
    /// chose, both above the page (R8).
    fn settings_pickers(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let locale = self.workspace.locale();
        let category = self.settings_nav.category;
        let mut categories = div()
            .flex()
            .flex_wrap()
            .items_center()
            .w_full()
            .min_w(px(0.0))
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(|| "settings.picker.category".to_owned());
        for candidate in Category::ALL {
            let selector = format!("settings.category.{}", candidate.id());
            let reserved = components::segment_reserved_width(candidate.label(locale), metrics);
            categories = categories.child(
                components::segment(candidate.label(locale), candidate == category, metrics)
                    .min_w(reserved)
                    .max_w_full()
                    .flex_shrink()
                    .whitespace_normal()
                    .h(gpui::auto())
                    .min_h(metrics.compact_control_height())
                    .id(SharedString::from(selector.clone()))
                    .debug_selector({
                        let selector = selector.clone();
                        move || selector.clone()
                    })
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.settings_select_category(candidate, cx);
                    })),
            );
        }
        let mut items = div()
            .flex()
            .flex_wrap()
            .items_center()
            .w_full()
            .min_w(px(0.0))
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(|| "settings.picker.item".to_owned());
        for (index, item) in category.items().iter().enumerate() {
            let picked = index == self.settings_nav.item_of(category);
            let selector = format!("settings.item.{}", item.id());
            let reserved = components::segment_reserved_width(item.label(locale), metrics);
            items = items.child(
                components::segment(item.label(locale), picked, metrics)
                    .min_w(reserved)
                    .max_w_full()
                    .flex_shrink()
                    .whitespace_normal()
                    .h(gpui::auto())
                    .min_h(metrics.compact_control_height())
                    .id(SharedString::from(selector.clone()))
                    .debug_selector({
                        let selector = selector.clone();
                        move || selector.clone()
                    })
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.settings_select_item(category, index, cx);
                    })),
            );
        }
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .min_w(px(0.0))
            .gap(metrics.scale(spacing::TIGHT))
            .child(categories)
            .when(!Self::single_named_item(category, locale), |column| {
                column.child(items)
            })
            .when(self.settings_nav.nav_focus, |column| {
                column.child(self.settings_nav_hint())
            })
    }

    /// The page of one item: where it sits, its name and its sentence, then its own
    /// controls - and nothing else (R16).
    fn settings_item_heading(&self, item: Item) -> Div {
        let metrics = self.metrics();
        let locale = self.workspace.locale();
        let selector = format!("settings.page.{}", item.id());
        // A page whose card is one setting already says the name in its row; repeating the
        // sentence here would print the same words twice (R16). Pages that are a list of
        // rows keep it, because nothing below repeats it.
        let hint = match item {
            Item::Shortcuts | Item::Policy => item.hint(locale),
            _ => "",
        };
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w(px(0.0))
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(move || selector.clone())
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "settings.crumb".to_owned())
                    .child(item.category().label(locale)),
            )
            .child(
                div()
                    .w_full()
                    .min_w(px(0.0))
                    .text_size(metrics.hero_size())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "header.title".to_owned())
                    .child(item.label(locale)),
            )
            .when(!hint.is_empty(), |column| {
                column.child(
                    div()
                        .w_full()
                        .min_w(px(0.0))
                        .text_size(metrics.body_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .debug_selector(|| "settings.intro".to_owned())
                        .child(hint),
                )
            })
    }
}

impl ShelluneView {
    /// The controls of one item, all of them inside one card.
    fn settings_item_body(&self, item: Item, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        match item {
            Item::Shortcuts => self.settings_keys_page(cx),
            Item::Policy => self.settings_connection_page(cx),
            Item::HostsFile => self.settings_card(
                item.id(),
                vec![
                    self.settings_row(
                        "hosts-file",
                        copy::ux().settings_hosts_file,
                        "",
                        div()
                            .max_w(metrics.scale(360.0))
                            .truncate()
                            .font_family(terminal::monospace_family())
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .debug_selector(|| "settings.paths".to_owned())
                            .child(SharedString::from(super::home_relative(
                                &self.store.path().display().to_string(),
                            ))),
                    ),
                ],
            ),
            Item::Version => self.settings_card(item.id(), vec![self.settings_version_row()]),
            _ => {
                let row = self.settings_control_row(item, cx);
                self.settings_card(item.id(), vec![row])
            }
        }
    }

    /// One item whose control is a choice: the tracks, the font stepper, the two
    /// reminders and the import button, each doing what it did before the page was split
    /// (R16).
    fn settings_control_row(&self, item: Item, cx: &mut Context<Self>) -> Div {
        let ux = copy::ux();
        match item {
            Item::Theme => self.settings_row(
                "theme",
                "",
                ux.settings_theme_hint,
                self.settings_theme_control(cx),
            ),
            Item::Language => self.settings_row(
                "language",
                "",
                ux.settings_language_hint,
                self.settings_language_control(cx),
            ),
            Item::TextSize => self.settings_row(
                "text-size",
                "",
                ux.settings_text_size_hint,
                self.settings_zoom_control(cx),
            ),
            Item::Motion => self.settings_row(
                "motion",
                "",
                ux.settings_motion_hint,
                self.settings_motion_control(cx),
            ),
            Item::Scheme => self.settings_row(
                "scheme",
                "",
                ux.settings_scheme_hint,
                self.settings_scheme_control(cx),
            ),
            Item::FontSize => self.settings_row(
                "font-size",
                "",
                ux.settings_font_size_hint,
                self.settings_font_control(cx),
            ),
            Item::DefaultShell => self.settings_row(
                "default-shell",
                "",
                ux.settings_default_shell_hint,
                self.settings_shell_control(cx),
            ),
            Item::BellNotice => self.settings_row(
                "bell-notice",
                "",
                ux.settings_bell_notice_hint,
                self.settings_notice_control(AttentionKind::Bell, cx),
            ),
            Item::FinishedNotice => self.settings_row(
                "finished-notice",
                "",
                ux.settings_finished_notice_hint,
                self.settings_notice_control(AttentionKind::CommandFinished, cx),
            ),
            Item::Import => self.settings_row(
                "import",
                "",
                ux.settings_import_hint,
                components::icon_label_button(
                    crate::app::icons::Icon::Import,
                    self.strings().import_open,
                    components::Variant::Secondary,
                    self.metrics(),
                )
                .id("settings.import")
                .debug_selector(|| "settings.import".to_owned())
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.select(Destination::Hosts, window, cx);
                    this.open_import(cx);
                })),
            ),
            // These have a page of their own, and a page is never built as one row.
            Item::Shortcuts | Item::Policy | Item::HostsFile | Item::Version => {
                div().flex().flex_col()
            }
        }
    }

    /// The theme choice.
    fn settings_theme_control(&self, cx: &mut Context<Self>) -> Div {
        let strings = self.strings();
        let choice = self.theme_choice;
        self.segments(
            "settings.theme",
            crate::app::theme::ThemeChoice::ALL
                .iter()
                .map(|candidate| {
                    let label = match candidate {
                        crate::app::theme::ThemeChoice::System => strings.theme_system,
                        crate::app::theme::ThemeChoice::Light => strings.theme_light,
                        crate::app::theme::ThemeChoice::Dark => strings.theme_dark,
                    };
                    (label.to_owned(), *candidate == choice)
                })
                .collect(),
            cx,
            |view, index, cx| view.set_theme(crate::app::theme::ThemeChoice::ALL[index], cx),
        )
    }

    /// The interface language.
    fn settings_language_control(&self, cx: &mut Context<Self>) -> Div {
        let locale = self.workspace.locale();
        self.segments(
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
        )
    }

    /// The interface text size.
    fn settings_zoom_control(&self, cx: &mut Context<Self>) -> Div {
        let zoom = self.workspace.zoom();
        self.segments(
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
        )
    }

    /// Whether short fades are drawn.
    fn settings_motion_control(&self, cx: &mut Context<Self>) -> Div {
        let strings = self.strings();
        let motion = self.workspace.motion();
        self.segments(
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
        )
    }

    /// The terminal's own palette.
    fn settings_scheme_control(&self, cx: &mut Context<Self>) -> Div {
        let ux = copy::ux();
        let strings = self.strings();
        let scheme = self.terminal_scheme();
        self.segments(
            "settings.scheme",
            crate::app::theme::TerminalScheme::ALL
                .iter()
                .map(|candidate| {
                    let label = match candidate {
                        crate::app::theme::TerminalScheme::FollowApp => ux.settings_scheme_app,
                        crate::app::theme::TerminalScheme::Dark => strings.theme_dark,
                        crate::app::theme::TerminalScheme::Light => strings.theme_light,
                    };
                    (label.to_owned(), *candidate == scheme)
                })
                .collect(),
            cx,
            |view, index, cx| {
                view.set_terminal_scheme(crate::app::theme::TerminalScheme::ALL[index], cx);
            },
        )
    }

    /// The terminal font size: two buttons and the value between them.
    fn settings_font_control(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let font = self.workspace.local_font_size();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .min_w(px(0.0))
            .max_w_full()
            .gap(metrics.scale(spacing::HAIRLINE))
            .child(
                components::icon_button("A-", metrics)
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
            )
    }

    /// The shell the + button opens.
    fn settings_shell_control(&self, cx: &mut Context<Self>) -> Div {
        let choice = self.workspace.shell_choice();
        let options = self
            .workspace
            .shells()
            .iter()
            .enumerate()
            .map(|(index, shell)| (shell.name().to_owned(), index == choice))
            .collect();
        self.segments("settings.shell", options, cx, |view, index, cx| {
            view.workspace.select_shell(index);
            cx.notify();
        })
    }

    /// One background reminder, on or off (R7).
    ///
    /// The two are switched one by one: knowing that a long command in another tab ended
    /// is not the same wish as hearing about every bell. Both only decide whether a tab is
    /// marked, so turning one off leaves the bell byte, the command blocks and every
    /// disconnect reason exactly as they were.
    fn settings_notice_control(&self, kind: AttentionKind, cx: &mut Context<Self>) -> Div {
        let ux = copy::ux();
        let notifications = self.workspace.attention_settings();
        let on = match kind {
            AttentionKind::Bell => notifications.bell,
            AttentionKind::CommandFinished => notifications.finished,
        };
        let label = |state: bool| if state { ux.notice_on } else { ux.notice_off }.to_owned();
        let id = match kind {
            AttentionKind::Bell => "settings.notice.bell",
            AttentionKind::CommandFinished => "settings.notice.finished",
        };
        self.segments(
            id,
            vec![(label(on), on), (label(!on), !on)],
            cx,
            move |view, index, cx| {
                view.set_attention_notice(kind, index == 0, cx);
            },
        )
    }

    /// The connection page: the keepalive and timeout numbers every host is dialled with
    /// unless it says something of its own (R20).
    ///
    /// The numbers are edited where they are read: a change here is what the next attempt
    /// is dialled with, and a field keeps the reason it refused a value instead of being
    /// corrected silently. Zero in the interval is OpenSSH's way of switching the probe
    /// off, so it is a value rather than an error.
    fn settings_connection_page(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let on = self.connection_draft.keepalive_on();
        let switching = self.segments(
            "settings.keepalive",
            vec![
                (ux.notice_on.to_owned(), on),
                (ux.notice_off.to_owned(), !on),
            ],
            cx,
            |view, index, cx| view.set_connection_keepalive(index == 0, cx),
        );
        let window = match self.connection_draft.detection_window_secs() {
            Some(seconds) => copy::fill(ux.connection_window, &[("seconds", &seconds.to_string())]),
            None => ux.connection_window_off.to_owned(),
        };
        let keepalive_row = self.settings_row(
            "keepalive",
            ux.host_connection_title,
            ux.connection_window_hint,
            switching,
        );
        let interval_row = self.settings_connection_row(
            ConnectionField::KeepaliveInterval,
            ux.settings_keepalive_interval_hint,
        );
        let misses_row = self.settings_connection_row(
            ConnectionField::KeepaliveMisses,
            ux.settings_keepalive_misses_hint,
        );
        let timeout_row = self.settings_connection_row(
            ConnectionField::ConnectTimeout,
            ux.settings_connect_timeout_hint,
        );
        self.settings_card(
            "policy",
            vec![
                keepalive_row,
                interval_row,
                misses_row,
                timeout_row,
                div()
                    .w_full()
                    .min_w(px(0.0))
                    .px(metrics.scale(spacing::BASE))
                    .py(metrics.scale(spacing::TIGHT))
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .debug_selector(|| "settings.policy.window".to_owned())
                    .child(SharedString::from(window)),
                div()
                    .w_full()
                    .min_w(px(0.0))
                    .px(metrics.scale(spacing::BASE))
                    .py(metrics.scale(spacing::TIGHT))
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "settings.policy.note".to_owned())
                    .child(ux.connection_defaults_note),
            ],
        )
    }

    /// One number of the connection page: its field, and the reason a value was refused.
    ///
    /// The refusal sits under the field that produced it and nowhere else, so a person
    /// correcting a number reads the sentence about that number (R20).
    fn settings_connection_row(&self, field: ConnectionField, hint: &'static str) -> Div {
        let metrics = self.metrics();
        let locale = self.workspace.locale();
        let ux = copy::ux();
        let mut control = div()
            .flex()
            .flex_col()
            .items_end()
            .gap(metrics.scale(2.0))
            .child(
                div()
                    .w(metrics.scale(84.0))
                    .debug_selector(move || format!("settings.policy.{}", field.id()))
                    .child(self.connection_fields.get(field).clone()),
            );
        if let Some((refused, problem)) = self.connection_draft.error()
            && refused == field
        {
            control = control.child(
                div()
                    .max_w(metrics.scale(220.0))
                    .text_size(metrics.caption_size())
                    .text_color(theme::Tone::Danger.text())
                    .debug_selector(move || format!("settings.policy.error.{}", field.id()))
                    .child(SharedString::from(connection_problem_text(problem, locale))),
            );
        }
        self.settings_row(field.id(), field.label(ux), hint, control)
    }

    /// The about row: what this build is and the version it reports.
    fn settings_version_row(&self) -> Div {
        let ux = copy::ux();
        let metrics = self.metrics();
        let version = format!("{} {}", ux.settings_version, self.version());
        // A bare text div stretches to the control column, and that column's minimum is
        // zero, so the line collapses and the version wraps one glyph per row. The floor
        // is the real-font width, capped to the row, so it stays one line in the card.
        let reserved = components::reserved_text_width(&version, metrics.label_size())
            .min(components::settings_row_content_cap(metrics));
        self.settings_row(
            "about",
            crate::app::product::NAME,
            match self.workspace.locale() {
                Locale::English => crate::app::product::TAGLINE_EN,
                Locale::Chinese => crate::app::product::TAGLINE_ZH,
            },
            div()
                .min_w(reserved)
                .flex_none()
                .whitespace_nowrap()
                .text_size(metrics.label_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .debug_selector(|| "status.version".to_owned())
                .child(SharedString::from(version)),
        )
    }
}

impl ShelluneView {
    /// One item's controls inside one card.
    ///
    /// The card is the box a row and every control in it has to stay inside: the page
    /// scrolls downwards only, so a control drawn past the card's right edge is drawn
    /// past the window as well (R8, 720x480).
    fn settings_card(&self, id: &'static str, rows: Vec<Div>) -> Div {
        let count = rows.len();
        let mut card = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w(px(0.0))
            .rounded(px(radius::LARGE))
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .shadow_xs()
            .debug_selector(move || format!("settings.card.{id}"));
        for (index, row) in rows.into_iter().enumerate() {
            card = card.child(row.when(index + 1 < count, |row| {
                row.border_b_1()
                    .border_color(theme::swatch(palette::BORDER))
            }));
        }
        card
    }

    /// One settings row: the label and its sentence on the left, the control on the right.
    ///
    /// The row is at most as wide as the card. Its label keeps a floor, and the control is
    /// at most the row, so a control that does not fit beside the label moves under it and
    /// a control wider than the row wraps inside itself (R8).
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
            .w_full()
            .min_w(px(0.0))
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
                    .max_w_full()
                    .gap(metrics.scale(2.0))
                    // A row whose page heading already names it has no label of its own;
                    // an empty line here pushed the hint below the control's centre.
                    .when(!label.is_empty(), |column| {
                        column.child(
                            div()
                                .w_full()
                                .min_w(px(0.0))
                                .text_size(metrics.body_size())
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                .child(label),
                        )
                    })
                    .when(!hint.is_empty(), |column| {
                        column.child(
                            div()
                                .w_full()
                                .min_w(px(0.0))
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED))
                                .child(hint),
                        )
                    }),
            )
            .child(
                // No max width here: a percentage max is cyclic with the row and collapses
                // a text control to nothing. Each control caps itself instead.
                div().flex().flex_col().min_w(px(0.0)).child(control),
            )
    }

    /// The shortcut page: the search, the scope filter, anything that needs an answer, and
    /// one row per action that matches (R16).
    ///
    /// The catalogue is the one source of truth: every row reads the binding in force from
    /// the keymap the menus, tooltips and the palette read, so a change here shows there
    /// without a second copy.
    fn settings_keys_page(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let locale = self.workspace.locale();
        let platform = self.keymap.platform();
        let matches = self.key_rows(cx);

        let mut rows: Vec<Div> = Vec::new();
        // A saved set that two actions share a combination in is not applied halfway: the
        // defaults are in force and this line says which two actions collided, which is the
        // one place a conflict can really be read (R16).
        if let Some(problem) = &self.keymap_problem {
            let (selector, next) = match problem {
                keymap::Problem::Conflict { .. } => {
                    ("settings.keys.conflicts", ux.keys_conflict_next)
                }
                _ => ("settings.keys.banner", ux.keys_problem_hint),
            };
            rows.push(self.settings_keys_banner(
                selector,
                theme::Tone::Warning,
                problem.text(locale, platform),
                next,
            ));
        }
        if let Some(reason) = &self.keymap_save_error {
            rows.push(self.settings_keys_banner(
                "settings.keys.banner",
                theme::Tone::Danger,
                format!("{} {reason}", ux.keys_save_failed),
                ux.keys_save_failed_next,
            ));
        }
        rows.push(
            div()
                .flex()
                .w_full()
                .min_w(px(0.0))
                .px(metrics.scale(spacing::BASE))
                .py(metrics.scale(spacing::TIGHT))
                .debug_selector(|| "settings.keys.search".to_owned())
                .child(self.keys_search.clone()),
        );
        rows.push(self.settings_keys_scope(cx));
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

        self.settings_card("keys", rows)
            .debug_selector(|| "settings.keys".to_owned())
    }

    /// The scope filter of the shortcut page: where a combination has to be pressed for the
    /// action to answer.
    ///
    /// It narrows the list and changes no binding, so a person looking for a key the
    /// terminal owns does not have to read the window's own actions first.
    fn settings_keys_scope(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let locale = self.workspace.locale();
        let chosen = self.settings_nav.scope;
        let mut options: Vec<(String, bool)> =
            vec![(ux.keys_scope_all.to_owned(), chosen.is_none())];
        for scope in keymap::Context::ALL {
            options.push((scope.label(locale).to_owned(), chosen == Some(scope)));
        }
        let mut track = div()
            .flex()
            .flex_wrap()
            .items_center()
            .w_full()
            .min_w(px(0.0))
            .gap(metrics.scale(spacing::HAIRLINE))
            .px(metrics.scale(spacing::BASE))
            .py(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "settings.keys.scope".to_owned())
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(ux.keys_scope_label),
            );
        for (index, (label, selected)) in options.into_iter().enumerate() {
            let selector = if index == 0 {
                "settings.keys.scope.all".to_owned()
            } else {
                format!(
                    "settings.keys.scope.{}",
                    keymap::Context::ALL[index - 1].id()
                )
            };
            let scope = (index > 0).then(|| keymap::Context::ALL[index - 1]);
            let reserved = components::segment_reserved_width(&label, metrics);
            track = track.child(
                components::segment(label, selected, metrics)
                    .min_w(reserved)
                    .max_w_full()
                    .flex_shrink()
                    .whitespace_normal()
                    .h(gpui::auto())
                    .min_h(metrics.compact_control_height())
                    .id(SharedString::from(selector.clone()))
                    .debug_selector({
                        let selector = selector.clone();
                        move || selector.clone()
                    })
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.settings_select_scope(scope, cx);
                    })),
            );
        }
        track
    }

    /// A tone-carrying line inside the shortcut card: why the saved keys were not applied,
    /// why a change could not be written, or which two actions share a combination.
    fn settings_keys_banner(
        &self,
        selector: &'static str,
        tone: theme::Tone,
        reason: String,
        next: &'static str,
    ) -> Div {
        let metrics = self.metrics();
        div()
            .px(metrics.scale(spacing::TIGHT))
            .py(metrics.scale(spacing::TIGHT))
            .debug_selector(move || selector.to_owned())
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

    /// One action: its name, where the shortcut applies, the keys in force and the three
    /// ways to change them.
    ///
    /// The row the keyboard is on carries the focus ring, so the page can be worked without
    /// a pointer: the arrow keys move between rows and Enter opens the one under the ring.
    fn settings_keys_row(
        &self,
        spec: &'static keymap::ActionSpec,
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
        // The controls this row offers, in the order the keyboard steps through them: an
        // unbound action cannot be unbound again, and one at its default has nothing to
        // restore (R5).
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
                    let reserved = components::keycap_reserved_width(&label, metrics);
                    components::keycap(SharedString::from(label), metrics)
                        .min_w(reserved)
                        .max_w_full()
                        .flex_shrink()
                        .whitespace_normal()
                        .h(gpui::auto())
                        .min_h(metrics.scale(18.0))
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
            .w_full()
            .min_w(px(0.0))
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
                    .max_w_full()
                    .gap(metrics.scale(2.0))
                    .child(
                        div()
                            .w_full()
                            .min_w(px(0.0))
                            .text_size(metrics.body_size())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(spec.name(locale)),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w(px(0.0))
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(format!(
                                "{} - {state}",
                                spec.context.label(locale)
                            ))),
                    ),
            )
            .child({
                // The keys and their controls wrap as a group of their own: when the name
                // column leaves no room on the line, the group takes the next line instead
                // of pushing its controls past the card's right edge (R8).
                let mut group = div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .min_w(px(0.0))
                    .max_w_full()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(keys);
                for (position, control) in controls.iter().copied().enumerate() {
                    let (name, label, variant) = match control {
                        super::KeyRowControl::Change => {
                            ("change", ux.keys_change, components::Variant::Secondary)
                        }
                        super::KeyRowControl::Unbind => {
                            ("unbind", ux.keys_unbind, components::Variant::Ghost)
                        }
                        super::KeyRowControl::Reset => {
                            ("reset", ux.keys_reset, components::Variant::Ghost)
                        }
                    };
                    // The control the keyboard's cursor is on carries the accent ring, so
                    // the step between Change, Unbind and Restore is visible (R4).
                    let focused = ringed && position == cursor;
                    let selector = format!("settings.keys.{name}.{action}");
                    let reserved = components::compact_label_reserved_width(label, metrics);
                    let button = components::compact_button(label, variant, metrics)
                        .min_w(reserved)
                        .max_w_full()
                        .flex_shrink()
                        .whitespace_normal()
                        .h(gpui::auto())
                        .min_h(metrics.compact_control_height())
                        .id(SharedString::from(selector.clone()))
                        .debug_selector({
                            let selector = selector.clone();
                            move || selector.clone()
                        })
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

    /// The end of the shortcut card: how many actions differ from their defaults, and the
    /// one control that puts every one of them back.
    fn settings_keys_footer(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let count = self.keymap.custom_count();
        div()
            .flex()
            .w_full()
            .min_w(px(0.0))
            .items_center()
            .justify_between()
            .flex_wrap()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::BASE))
            .py(metrics.scale(spacing::TIGHT))
            .child(
                div()
                    .min_w(px(0.0))
                    .max_w_full()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "settings.keys.count".to_owned())
                    .child(SharedString::from(
                        ux.keys_custom_summary.replace("{}", &count.to_string()),
                    )),
            )
            .child(
                components::compact_button(
                    ux.keys_reset_all,
                    components::Variant::Secondary,
                    metrics,
                )
                .min_w(components::compact_label_reserved_width(
                    ux.keys_reset_all,
                    metrics,
                ))
                .max_w_full()
                .flex_shrink()
                .whitespace_normal()
                .h(gpui::auto())
                .min_h(metrics.compact_control_height())
                .id("settings.keys.reset_all")
                .debug_selector(|| "settings.keys.reset_all".to_owned())
                .on_click(cx.listener(|this, _event, _window, cx| {
                    this.keys_reset_all(cx);
                })),
            )
    }
}

impl ShelluneView {
    /// The shortcut rows the page lists: the catalogue narrowed by the search and by the
    /// scope filter, in catalogue order.
    ///
    /// The keyboard's row stepping and the drawn list come from this one function, so the
    /// ring can never sit on a row the page does not show.
    pub(super) fn key_rows(&self, cx: &App) -> Vec<&'static keymap::ActionSpec> {
        let query = self.keys_search.read(cx).text().to_owned();
        let scope = self.settings_nav.scope;
        self.keymap
            .matching(&query, self.workspace.locale())
            .into_iter()
            .filter(|spec| match scope {
                Some(scope) => spec.context == scope,
                None => true,
            })
            .collect()
    }

    /// Opens one category, keeping the item it was last on (R16).
    fn settings_select_category(&mut self, category: Category, cx: &mut Context<Self>) {
        self.settings_nav.nav_focus = false;
        if self.settings_nav.category != category {
            self.settings_nav.category = category;
            cx.notify();
        }
    }

    /// Opens one item of one category.
    fn settings_select_item(&mut self, category: Category, index: usize, cx: &mut Context<Self>) {
        self.settings_nav.category = category;
        self.settings_nav.set_item(category, index);
        self.settings_nav.nav_focus = false;
        self.keys_selected = 0;
        self.keys_control = 0;
        cx.notify();
    }

    /// Narrows the shortcut list to one scope, or to every scope again.
    fn settings_select_scope(&mut self, scope: Option<keymap::Context>, cx: &mut Context<Self>) {
        if self.settings_nav.scope != scope {
            self.settings_nav.scope = scope;
            self.keys_selected = 0;
            self.keys_control = 0;
            cx.notify();
        }
    }

    /// Moves the chosen category by one, wrapping.
    fn settings_step_category(&mut self, delta: isize, cx: &mut Context<Self>) {
        let all = Category::ALL;
        let at = all
            .iter()
            .position(|candidate| *candidate == self.settings_nav.category)
            .unwrap_or_default();
        self.settings_nav.category = all[crate::app::palette::step(at, delta, all.len())];
        cx.notify();
    }

    /// Moves the chosen item inside the current category, wrapping.
    fn settings_step_item(&mut self, delta: isize, cx: &mut Context<Self>) {
        let category = self.settings_nav.category;
        let count = category.items().len();
        let at = self.settings_nav.item_of(category);
        self.settings_nav
            .set_item(category, crate::app::palette::step(at, delta, count));
        cx.notify();
    }

    /// Keys the settings page reads before anything else does (R16).
    ///
    /// On the shortcut page the arrow keys choose a row and then one of its controls, and
    /// Enter runs the control the cursor is on, so a binding can be changed, unbound or
    /// restored with the keyboard alone. Everywhere else the same keys walk the two levels
    /// of the navigation: up and down a category, left and right an item of it. Modifier
    /// combinations are left to the action catalogue.
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
        // The category column has its own mode: every page, including the shortcut list,
        // can hand the keyboard back to it with Escape, and the arrows then walk both
        // levels instead of the page (R16).
        if self.settings_nav.nav_focus {
            match event.keystroke.key.as_str() {
                "up" => self.settings_step_category(-1, cx),
                "down" => self.settings_step_category(1, cx),
                "left" => self.settings_step_item(-1, cx),
                "right" => self.settings_step_item(1, cx),
                "enter" | "escape" => self.settings_nav.nav_focus = false,
                _ => return,
            }
            window.focus(self.destination_focus(Destination::Settings));
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "escape" {
            self.settings_nav.nav_focus = true;
            window.focus(self.destination_focus(Destination::Settings));
            cx.notify();
            cx.stop_propagation();
            return;
        }
        let on_shortcuts = self.settings_nav.category == Category::Keys;
        match event.keystroke.key.as_str() {
            "down" => {
                if on_shortcuts {
                    self.step_key_row(1, cx);
                } else {
                    self.settings_step_category(1, cx);
                }
            }
            "up" => {
                if on_shortcuts {
                    self.step_key_row(-1, cx);
                } else {
                    self.settings_step_category(-1, cx);
                }
            }
            "right" => {
                if on_shortcuts {
                    self.step_key_control(1, cx);
                } else {
                    self.settings_step_item(1, cx);
                }
            }
            "left" => {
                if on_shortcuts {
                    self.step_key_control(-1, cx);
                } else {
                    self.settings_step_item(-1, cx);
                }
            }
            "enter" if on_shortcuts => self.run_key_row_control(cx),
            _ => return,
        }
        // Stepping into the page takes the keyboard off a field, so the following arrows
        // move the page instead of a caret.
        window.focus(self.destination_focus(Destination::Settings));
        cx.stop_propagation();
    }

    /// The actions the shortcut page lists now, as identifiers, for the capture scenarios
    /// and tests.
    #[cfg(any(test, feature = "ui-capture"))]
    #[must_use]
    pub fn settings_key_rows(&self, cx: &App) -> Vec<&'static str> {
        self.key_rows(cx).into_iter().map(|spec| spec.id).collect()
    }

    /// Whether the category column has the keyboard, for the tests that walk it.
    #[cfg(test)]
    #[must_use]
    pub fn settings_nav_focused_for_test(&self) -> bool {
        self.settings_nav.nav_focus
    }

    /// Which page the settings show, as identifiers, for the capture scenarios and tests.
    #[cfg(any(test, feature = "ui-capture"))]
    #[must_use]
    pub fn settings_page_id(&self) -> (&'static str, &'static str) {
        let category = self.settings_nav.category;
        (
            category.id(),
            category.items()[self.settings_nav.item_of(category)].id(),
        )
    }

    /// Opens one settings page, for a capture (R2).
    #[cfg(feature = "ui-capture")]
    pub fn capture_settings_page(&mut self, item: Item, cx: &mut Context<Self>) {
        let category = item.category();
        let index = category
            .items()
            .iter()
            .position(|candidate| *candidate == item)
            .unwrap_or_default();
        self.settings_nav.category = category;
        self.settings_nav.set_item(category, index);
        cx.notify();
    }

    /// Types a query into the shortcut search, for a capture (R2).
    #[cfg(feature = "ui-capture")]
    pub fn capture_settings_query(&mut self, query: &str, cx: &mut Context<Self>) {
        self.keys_search
            .update(cx, |field, cx| field.set_text(query, cx));
        cx.notify();
    }
}
