//! The design catalogue: every shared component in every state, on one page (ADR-2).
//!
//! It is the visual baseline of the component library. The capture tool photographs it
//! in both palettes (`design-catalog`), and a test lays it out in a window and checks
//! that nothing in it runs past its column. Nothing here is interactive and nothing is
//! read from the user's data: the page only draws [`crate::app::components`] with fixed
//! words, so a change to a token or a component shows up here before it shows up in a
//! screen.

use gpui::prelude::*;
use gpui::{AnyElement, Div, SharedString, div};

use crate::app::components::{
    self, ButtonSize, ControlState, ListRow, Progress, Variant, button_sized, button_with_state,
};
use crate::app::icons::{self, Icon};
use crate::app::theme::{self, Metrics, Token, Tone, palette, spacing};

/// The four columns of the page, left to right.
const COLUMNS: usize = 4;

/// The share of the page's width each column takes; the rest is the space between
/// them. The lists take the widest column, so a row shows its words, its metadata and
/// its command side by side.
///
/// The widths are fractions of a definite width rather than flex weights: a zero flex
/// basis measured every line of a column at nothing.
const COLUMN_SHARES: [f32; COLUMNS] = [0.21, 0.21, 0.31, 0.21];

/// The narrowest each column becomes, in logical pixels before zoom.
///
/// At the design window and 100% text every column is at least this wide already, so
/// the four stand side by side as before. At 200% text a column keeps the room its
/// components need and the page wraps the columns into rows, instead of squeezing a
/// list row's name or a dialog's title down to nothing.
const COLUMN_MIN_WIDTHS: [f32; COLUMNS] = [220.0, 220.0, 300.0, 220.0];

/// One titled group of the catalogue.
fn section(title: &'static str, metrics: Metrics) -> Div {
    div()
        .debug_selector(move || format!("catalog.section.{title}"))
        .flex()
        .flex_col()
        .min_w_0()
        .gap(metrics.scale(spacing::TIGHT))
        .child(components::section_label(title, metrics))
}

/// A row of samples that wraps when the column is narrower than the row.
fn samples(metrics: Metrics) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .min_w_0()
        .gap(metrics.scale(spacing::NARROW))
}

/// The words a sample field shows in place of a real text field.
fn placeholder(text: &'static str, metrics: Metrics) -> AnyElement {
    div()
        .min_w_0()
        .truncate()
        .text_size(metrics.label_size())
        .text_color(theme::swatch(palette::TEXT_MUTED))
        .child(components::line_text(text))
        .into_any_element()
}

/// The five button weights in the states a caller sets, and the three sizes.
fn buttons(metrics: Metrics) -> Div {
    let mut section = section("Buttons", metrics);
    for variant in Variant::ALL {
        let name = match variant {
            Variant::Primary => "Primary",
            Variant::Secondary => "Secondary",
            Variant::Ghost => "Ghost",
            Variant::Danger => "Danger",
            Variant::Icon => "Icon",
        };
        let mut row = samples(metrics);
        for state in [
            ControlState::Resting,
            ControlState::Hovered,
            ControlState::Focused,
            ControlState::Disabled,
        ] {
            row = row.child(if variant == Variant::Icon {
                let face = components::button_face(variant, state);
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(metrics.hit_target())
                    .rounded(gpui::px(theme::radius::MEDIUM))
                    .bg(face.fill_color())
                    .border_1()
                    .border_color(face.border_color())
                    .child(icons::icon(
                        Icon::Copy,
                        metrics.small_icon_size(),
                        face.text_color(),
                    ))
            } else {
                button_with_state(name, variant, state, metrics).h(metrics.compact_control_height())
            });
        }
        section = section.child(row);
    }
    let mut sizes = samples(metrics);
    for size in ButtonSize::ALL {
        let word = match size {
            ButtonSize::Compact => "Compact 28",
            ButtonSize::Regular => "Regular 32",
            ButtonSize::Large => "Large 36",
        };
        sizes = sizes.child(button_sized(word, Variant::Primary, size, metrics));
    }
    section.child(sizes)
}

/// Fields, the search box and the choices.
fn inputs(metrics: Metrics) -> Div {
    let field = |state: ControlState, text: &'static str| {
        components::field_frame(state, metrics).child(placeholder(text, metrics))
    };
    section("Fields and choices", metrics)
        .child(field(ControlState::Resting, "Resting field"))
        .child(field(ControlState::Focused, "Focused field"))
        .child(field(ControlState::Invalid, "Rejected field"))
        .child(components::search_field(
            placeholder("Search hosts or user@host", metrics),
            Some(components::icon_only_button(Icon::Close, metrics).into_any_element()),
            Some(SharedString::from("Ctrl+F")),
            ControlState::Resting,
            metrics,
        ))
        .child(
            components::segmented(metrics)
                .child(components::segment("All", true, metrics))
                .child(components::segment("Recent", false, metrics))
                .child(components::segment("Groups", false, metrics)),
        )
        .child(
            samples(metrics)
                .child(components::switch(true, metrics))
                .child(components::switch(false, metrics))
                .child(components::checkbox(
                    "catalog.check.on",
                    "On",
                    true,
                    metrics,
                ))
                .child(components::checkbox(
                    "catalog.check.off",
                    "Off",
                    false,
                    metrics,
                )),
        )
}

/// Every tone as a chip and a mark, the quiet labels and the keycap.
fn labels(metrics: Metrics) -> Div {
    let tones = [
        (Tone::Positive, "Connected"),
        (Tone::Warning, "Connecting"),
        (Tone::Danger, "Failed"),
        (Tone::Info, "Info"),
        (Tone::Accent, "Current"),
        (Tone::Neutral, "Idle"),
        (Tone::Planned, "Closed"),
    ];
    let mut chips = samples(metrics);
    let mut marks = samples(metrics);
    for (tone, word) in tones {
        chips = chips.child(components::chip(word, tone, metrics));
        marks = marks.child(components::status_mark(
            tone,
            metrics.scale(spacing::TIGHT + spacing::HALF),
        ));
    }
    section("Labels", metrics).child(chips).child(marks).child(
        samples(metrics)
            .child(components::tag("Prod", metrics))
            .child(components::badge(24, Tone::Neutral, metrics))
            .child(components::badge(3, Tone::Accent, metrics))
            .child(components::kind_badge("SSH", true, metrics))
            .child(components::kbd("Ctrl+K", metrics))
            .child(components::kbd("⌘N", metrics)),
    )
}

/// The banners, the progress bars and the skeleton.
fn feedback(metrics: Metrics) -> Div {
    let banner = |tone: Tone, icon: Icon, words: &'static str, action: &'static str| {
        components::inline_banner(tone, icon, words, metrics)
            .rounded(gpui::px(theme::radius::MEDIUM))
            .child(components::compact_button(
                action,
                Variant::Secondary,
                metrics,
            ))
            .child(components::icon_only_button(Icon::Close, metrics))
    };
    section("Feedback", metrics)
        .child(banner(
            Tone::Danger,
            Icon::Plug,
            "Connection lost · keepalive timed out",
            "Reconnect",
        ))
        .child(banner(
            Tone::Warning,
            Icon::Refresh,
            "Reconnecting to build-01…",
            "Cancel",
        ))
        .child(banner(
            Tone::Info,
            Icon::Globe,
            "A new version is ready",
            "Details",
        ))
        .child(components::progress_bar(
            "catalog.progress.half",
            Progress::Determinate(0.6),
            metrics,
        ))
        .child(components::progress_bar(
            "catalog.progress.sweep",
            Progress::Indeterminate,
            metrics,
        ))
        .child(components::skeleton_row("catalog.skeleton", metrics))
}

/// The sidebar entries, the list rows and a toolbar.
fn lists(metrics: Metrics) -> Div {
    let avatar = |name: &'static str, pair: (Token, Token)| {
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(metrics.control_height())
            .rounded(gpui::px(theme::radius::MEDIUM))
            .bg(theme::swatch(pair.0))
            .text_size(metrics.caption_size())
            .font_weight(metrics.title_weight())
            .text_color(theme::swatch(pair.1))
            .child(crate::app::view::host_initials(name))
            .into_any_element()
    };
    let row = |name: &'static str, address: &'static str, state: ControlState, index: usize| {
        components::list_row(
            ListRow {
                leading: Some(avatar(
                    name,
                    palette::AVATARS[index % palette::AVATARS.len()],
                )),
                title: SharedString::from(name),
                subtitle: Some(SharedString::from(address)),
                monospace_subtitle: true,
                meta: Some(
                    div()
                        .flex()
                        .items_center()
                        .gap(metrics.scale(spacing::TIGHT))
                        .child(components::tag("Prod", metrics))
                        .child("2h")
                        .child(components::status_dot(Tone::Positive, metrics))
                        .into_any_element(),
                ),
                actions: Some(
                    components::compact_button("Connect", Variant::Primary, metrics)
                        .into_any_element(),
                ),
                reveal_actions: true,
            },
            state,
            metrics,
        )
    };
    section("Lists", metrics)
        .child(
            components::toolbar(metrics)
                .child(
                    div()
                        .flex_none()
                        .text_size(metrics.title_size())
                        .font_weight(metrics.title_weight())
                        .child("Hosts"),
                )
                .child(div().flex_1())
                .child(components::icon_only_button(Icon::More, metrics))
                .child(components::compact_button(
                    "New host",
                    Variant::Primary,
                    metrics,
                )),
        )
        .child(components::sidebar_item(
            Icon::Server,
            "All hosts",
            Some(24),
            true,
            metrics,
        ))
        .child(components::sidebar_item(
            Icon::Star,
            "Favorites",
            Some(3),
            false,
            metrics,
        ))
        .child(
            samples(metrics)
                .child(components::sidebar_icon_item(Icon::Server, true, metrics))
                .child(components::sidebar_icon_item(Icon::Layers, false, metrics)),
        )
        .child(row(
            "build-01",
            "deploy@192.0.2.11",
            ControlState::Resting,
            0,
        ))
        .child(row(
            "api-02",
            "deploy@192.0.2.24:2222",
            ControlState::Selected,
            1,
        ))
        .child(row("edge-03", "ops@198.51.100.7", ControlState::Hovered, 2))
}

/// The floating layers: a menu, a toast and a status card.
fn surfaces(metrics: Metrics) -> Div {
    section("Surfaces", metrics)
        .child(
            components::menu_card("catalog.menu", metrics)
                .child(
                    components::menu_entry(
                        "Copy",
                        "catalog.menu.copy",
                        Some(Icon::Copy),
                        true,
                        false,
                        metrics,
                    )
                    .child(components::menu_shortcut("Ctrl+C", metrics)),
                )
                .child(components::menu_divider(metrics))
                .child(components::menu_entry(
                    "Delete",
                    "catalog.menu.delete",
                    Some(Icon::Trash),
                    true,
                    true,
                    metrics,
                )),
        )
        .child(
            components::toast(
                Tone::Positive,
                Icon::Check,
                div().child("Copied"),
                Some(SharedString::from("deploy@192.0.2.11")),
                metrics,
            )
            .w_full(),
        )
        .child(components::status_card_slot(
            components::status_card(
                Tone::Danger,
                icons::icon(Icon::Plug, metrics.icon_size(), Tone::Danger.text())
                    .into_any_element(),
                div().child("Connection lost"),
                Some(
                    div()
                        .child("The server stopped answering keepalives. The last screen is kept."),
                ),
                metrics,
            ),
            metrics,
        ))
}

/// The colour tokens as swatches: the layers, the text and the accent.
fn swatches(metrics: Metrics) -> Div {
    let tokens = [
        palette::CANVAS,
        palette::SURFACE,
        palette::RAISED,
        palette::OVERLAY,
        palette::SURFACE_HOVER,
        palette::SURFACE_SELECTED,
        palette::BORDER,
        palette::BORDER_STRONG,
        palette::TEXT_PRIMARY,
        palette::TEXT_SECONDARY,
        palette::TEXT_MUTED,
        palette::ACCENT,
        palette::POSITIVE,
        palette::WARNING,
        palette::DANGER,
        palette::INFO,
    ];
    let mut strip = samples(metrics).gap(metrics.scale(spacing::HAIRLINE));
    for token in tokens {
        strip = strip.child(
            div()
                .flex_none()
                .size(metrics.scale(spacing::LOOSE))
                .rounded(gpui::px(theme::radius::SMALL))
                .bg(theme::swatch(token))
                .border_1()
                .border_color(theme::swatch(palette::BORDER_STRONG)),
        );
    }
    let type_line = |size: gpui::Pixels, words: &'static str| {
        div()
            .debug_selector(move || format!("catalog.type.{words}"))
            .text_size(size)
            .child(words)
    };
    // The type scale runs on as one wrapping line, largest first, so the whole page
    // fits the design window.
    section("Tokens", metrics).child(strip).child(
        samples(metrics)
            .items_baseline()
            .gap(metrics.scale(spacing::SNUG))
            .child(type_line(metrics.hero_size(), "Shellune 28"))
            .child(type_line(metrics.heading_size(), "Page title 20"))
            .child(type_line(metrics.title_size(), "Heading 16"))
            .child(type_line(metrics.emphasis_size(), "Emphasis 14"))
            .child(type_line(metrics.body_size(), "Body 13"))
            .child(
                type_line(metrics.caption_size(), "Caption 12")
                    .text_color(theme::swatch(palette::TEXT_SECONDARY)),
            ),
    )
}

/// The whole catalogue: four columns of sections on the window's floor, scrolling when
/// the window is shorter than the page.
pub fn design_catalog(metrics: Metrics) -> gpui::Stateful<Div> {
    let columns: [Vec<Div>; COLUMNS] = [
        vec![buttons(metrics), labels(metrics)],
        vec![inputs(metrics), feedback(metrics)],
        vec![lists(metrics), swatches(metrics)],
        vec![surfaces(metrics)],
    ];
    let mut grid = div()
        .flex()
        .flex_wrap()
        .items_start()
        .justify_between()
        .gap_y(metrics.scale(spacing::LOOSE))
        .w_full()
        .min_w_0();
    for (index, sections) in columns.into_iter().enumerate() {
        grid = grid.child(
            div()
                .debug_selector(move || format!("catalog.column.{index}"))
                .flex()
                .flex_col()
                .flex_none()
                .w(gpui::relative(COLUMN_SHARES[index]))
                .min_w(metrics.scale(COLUMN_MIN_WIDTHS[index]))
                .gap(metrics.scale(spacing::LOOSE))
                .children(sections),
        );
    }
    div()
        .id("catalog")
        .debug_selector(|| "catalog".to_owned())
        .size_full()
        .overflow_y_scroll()
        .bg(theme::swatch(palette::CANVAS))
        .text_color(theme::swatch(palette::TEXT_PRIMARY))
        .text_size(metrics.body_size())
        .p(metrics.scale(spacing::LOOSE))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::BASE))
                .child(
                    div()
                        .text_size(metrics.heading_size())
                        .font_weight(metrics.title_weight())
                        .child("Design catalogue · token v2"),
                )
                .child(grid),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, px, size};

    struct Catalog {
        zoom: theme::Zoom,
    }

    impl gpui::Render for Catalog {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            theme::set_motion(theme::Motion::Reduced);
            design_catalog(Metrics::new(self.zoom))
        }
    }

    #[gpui::test]
    fn every_section_of_the_catalogue_stays_inside_its_column(cx: &mut TestAppContext) {
        for (appearance, zoom) in [
            (theme::Appearance::Dark, theme::Zoom::Regular),
            (theme::Appearance::Light, theme::Zoom::Regular),
            (theme::Appearance::Dark, theme::Zoom::Largest),
        ] {
            theme::set_appearance(appearance);
            let (_view, cx) = cx.add_window_view(move |_window, _cx| Catalog { zoom });
            cx.simulate_resize(size(px(1120.0), px(720.0)));
            cx.run_until_parked();
            let columns: Vec<_> = (0..COLUMNS)
                .map(|index| {
                    cx.debug_bounds(Box::leak(
                        format!("catalog.column.{index}").into_boxed_str(),
                    ))
                    .expect("every column is drawn")
                })
                .collect();
            for title in [
                "Buttons",
                "Fields and choices",
                "Labels",
                "Lists",
                "Feedback",
                "Surfaces",
                "Tokens",
            ] {
                let selector: &'static str =
                    Box::leak(format!("catalog.section.{title}").into_boxed_str());
                let section = cx
                    .debug_bounds(selector)
                    .unwrap_or_else(|| panic!("{title} is drawn"));
                assert!(
                    columns.iter().any(|column| {
                        section.left() + px(0.5) >= column.left()
                            && section.right() <= column.right() + px(0.5)
                    }),
                    "{title} runs out of its column at {zoom:?}: {section:?} in {columns:?}"
                );
            }
            // Words that end in an ellipsis are shaped at the width they are drawn at:
            // a list row's name is not cut to its ellipsis in a column that has room.
            let title = cx.debug_bounds("list.row.title").expect("a row is drawn");
            assert!(
                title.size.width > px(40.0) * zoom.factor(),
                "the row's name is cut short at {zoom:?}: {title:?}"
            );
            // At 200% text the columns wrap into rows rather than run past the window
            // or squeeze their components (T9).
            for (index, column) in columns.iter().enumerate() {
                assert!(
                    column.right() <= px(1120.0) + px(0.5),
                    "column {index} runs past the window at {zoom:?}: {column:?}"
                );
                for other in &columns[index + 1..] {
                    assert!(
                        column.right() <= other.left() + px(0.5)
                            || column.bottom() <= other.top() + px(0.5),
                        "two columns overlap at {zoom:?}: {column:?} and {other:?}"
                    );
                }
            }
            // The capture shoots the page at the design window: at 100% the whole page,
            // every section to its last line, is on the one screen.
            if zoom == theme::Zoom::Regular {
                for (index, column) in columns.iter().enumerate() {
                    assert!(
                        column.bottom() <= px(720.0),
                        "column {index} runs past the design window: {column:?}"
                    );
                }
            }
        }
        theme::set_appearance(theme::Appearance::Dark);
        theme::set_motion(theme::Motion::Full);
    }
}
